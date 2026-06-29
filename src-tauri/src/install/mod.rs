//! Установка интеграции Jarvis ⇄ Claude Code — общая логика для CLI и приложения.
//!
//! Принципы: merge, не overwrite; идемпотентно; бэкап перед записью;
//! атомарная запись (tmp + rename); битый JSON не трогаем (возвращаем ошибку,
//! НЕ выходим из процесса — модуль вызывается и внутри живого демона).
//!
//! Шимы (jarvis-hook, claude-shim, tmux.conf, silero-server.py) вшиты в бинарь
//! include_str! — установщик самодостаточен и не зависит от расположения исходников.
//!
//! Модуль обслуживает два бинаря (приложение и CLI) с разным набором вызовов,
//! поэтому часть pub-API в каждом из них «не используется» — глушим dead_code.
#![allow(dead_code)]

use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const HOOK_SRC: &str = include_str!("../../../bin/jarvis-hook");
const SHIM_SRC: &str = include_str!("../../../bin/agent-shim");
const TMUX_CONF_SRC: &str = include_str!("../../../bin/jarvis-tmux.conf");
const SILERO_SERVER_SRC: &str = include_str!("../../../bin/silero-server.py");
/// STT-сайдкар (Qwen3-ASR MLX): Python-сервер для диктовки (инкр. 9, Phase 8).
const STT_SERVER_SRC: &str = include_str!("../../../bin/stt-server.py");
/// Codex-SDK сайдкар: одношаговый служебный LLM через `openai-codex` (саммари/
/// заголовки «под капотом»). Запускается из своего venv по запросу из настроек.
const CODEX_SUMMARY_SRC: &str = include_str!("../../../bin/codex-summary.py");
// MediaRemote-адаптер (BSD-3, ungive/mediaremote-adapter): пауза ЛЮБОГО медиа на
// время озвучки. Системный perl энтайтлен на MediaRemote — он dlopen-ит фреймворк.
const MRA_PL_SRC: &str = include_str!("../../../bin/mediaremote-adapter/mediaremote-adapter.pl");
const MRA_FW_SRC: &[u8] = include_bytes!("../../../bin/mediaremote-adapter/MediaRemoteAdapter.framework/MediaRemoteAdapter");

/// Признак «это наша запись» — путь шима в команде. Матчим без префикса каталога
/// данных, чтобы распознавать И прод (`.jarvis/bin/jarvis-hook`), И дев
/// (`.jarvis-dev/bin/jarvis-hook`) — иначе install/uninstall не видят dev-хуки.
const MARKER: &str = "bin/jarvis-hook";

/// Событие Claude Code → аргумент шима.
const EVENTS: [(&str, &str); 8] = [
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PreToolUse", "pre-tool"),
    ("PostToolUse", "post-tool"),
    ("Notification", "notification"),
    ("Stop", "stop"),
    ("StopFailure", "stop-failure"),
    ("SessionEnd", "session-end"),
];

/// Событие Codex → аргумент шима. PermissionRequest→waiting, SubagentStart/Stop
/// →доска. У Codex нет Notification/StopFailure/SessionEnd. (Дублируется с
/// backend::CODEX_EVENTS осознанно: install/mod.rs компилируется отдельным
/// бинарём jarvis-setup без остального крейта.)
const CODEX_EVENTS: [(&str, &str); 8] = [
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PreToolUse", "pre-tool"),
    ("PostToolUse", "post-tool"),
    ("Stop", "stop"),
    ("PermissionRequest", "permission"),
    ("SubagentStart", "subagent-start"),
    ("SubagentStop", "subagent-stop"),
];

/* ================= публичные типы (прогресс/статус) ================= */

/// Состояние шага установки для UI/CLI.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StepState {
    Start,
    Done,
    Warn,
    Info,
}

/// Один шаг установки: фаза + состояние + человекочитаемое сообщение.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub phase: String,
    pub state: StepState,
    pub msg: String,
    /// Процент загрузки 0..=100 для прогрессбара в UI (settings2.js читает `step.pct`).
    /// `None` у не-прогрессовых шагов → поле не сериализуется, полоса не рисуется.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
}

impl Step {
    fn new(phase: &str, state: StepState, msg: impl Into<String>) -> Step {
        Step { phase: phase.into(), state, msg: msg.into(), pct: None }
    }
    fn start(phase: &str) -> Step { Step::new(phase, StepState::Start, "") }
    fn done(phase: &str, msg: impl Into<String>) -> Step { Step::new(phase, StepState::Done, msg) }
    fn warn(phase: &str, msg: impl Into<String>) -> Step { Step::new(phase, StepState::Warn, msg) }
    fn info(phase: &str, msg: impl Into<String>) -> Step { Step::new(phase, StepState::Info, msg) }
    /// Шаг загрузки с процентом — UI рисует полосу прогресса по полю `pct`.
    fn progress(phase: &str, msg: impl Into<String>, pct: u8) -> Step {
        Step { phase: phase.into(), state: StepState::Info, msg: msg.into(), pct: Some(pct) }
    }
}

/// Колбэк прогресса. CLI печатает шаги, приложение шлёт их событием в окно.
pub type Progress<'a> = dyn Fn(Step) + 'a;

/// Что из интеграции уже установлено.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Status {
    pub hooks: bool,
    pub shim: bool,
    pub tmux_conf: bool,
    pub path_block: bool,
    pub silero: bool,
    /// Модель Whisper large-v3-turbo-q5_0.bin присутствует на диске.
    pub whisper_model: bool,
    /// Whisper вкомпилирован нативно (feature `whisper-native`). Без неё whisper-turbo
    /// — лишь стаб и падает при распознавании; источник правды для гейта/настроек.
    pub whisper_native_built: bool,
    /// Qwen3-ASR MLX-сайдкар (venv + stt-server.py) установлен.
    pub qwen3_sidecar: bool,
    /// Codex-SDK сайдкар (venv с openai-codex + codex-summary.py) установлен —
    /// служебный LLM «под капотом» на Codex.
    pub codex_sdk_sidecar: bool,
    /// Имя активного STT-движка из ~/.jarvis/settings.json (stt.engine).
    pub stt_engine_active: String,
    /// 3 ONNX-модели wake-word (инкр. 10) на месте (~3.5 МБ).
    pub wakeword_models: bool,
}

impl Status {
    /// Интеграция считается стоящей, если есть хуки и шим — без них Jarvis не
    /// получает события Claude Code и не оборачивает запуски.
    pub fn integrated(&self) -> bool {
        self.hooks && self.shim
    }
}

/* ================= пути ================= */

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("нет $HOME"))
}
/// $JARVIS_DIR или ~/.jarvis (изоляция dev-сборки — как и в util::jarvis_dir).
fn jarvis_dir() -> PathBuf {
    match std::env::var("JARVIS_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => home().join(".jarvis"),
    }
}

/// Чистый старт демона (зовётся ПЕРВЫМ в main::setup, ДО Daemon::new). Прибивает
/// прежние демоны и осиротевшие сайдкары, чтобы повторный запуск — в т.ч.
/// параллельным агентом — не плодил зомби-демонов и не уходил в цикл «порт занят».
/// В dev single-instance ВЫКЛ (см. main.rs), поэтому чистим руками. Best-effort:
/// ошибки молча игнорируются (системные pgrep/lsof/kill из /usr/bin).
pub fn prepare_clean_start() {
    let me = std::process::id().to_string();
    let kill = |pid: &str| {
        if !pid.is_empty() && pid != me {
            let _ = std::process::Command::new("kill").args(["-9", pid]).status();
        }
    };
    // 1) Прежние демоны: pgrep -x по имени процесса `jarvis` — матчит только бинарь
    //    демона, НЕ задевая cargo/rustc сборки (их comm = cargo/rustc).
    if let Ok(out) = std::process::Command::new("pgrep").args(["-x", "jarvis"]).output() {
        for pid in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            if pid != me {
                kill(pid);
                // install/mod.rs компилируется и в jarvis-setup, где нет `crate::log`
                // (см. конвенцию модуля выше). `log::line` всё равно начинается с
                // println! → daemon.log под nohup, поэтому stdout сохраняет диагностику.
                println!("[startup] прибит прежний демон pid {pid}");
            }
        }
    }
    // 2) Освободить порты сайдкаров (осиротевшие/чужие сайдкары держат :8731/:8732).
    for port in ["8731", "8732"] {
        if let Ok(out) = std::process::Command::new("lsof")
            .args(["-ti", &format!("tcp:{port}"), "-sTCP:LISTEN"])
            .output()
        {
            let pids = String::from_utf8_lossy(&out.stdout);
            for pid in pids.split_whitespace() {
                kill(pid);
            }
            if !pids.trim().is_empty() {
                println!("[startup] порт :{port} освобождён");
            }
        }
    }
    // 3) Stale unix-сокет — чтобы следующий bind прошёл чисто.
    let _ = std::fs::remove_file(jarvis_dir().join("run.sock"));
}

/// PATH с добавленными Homebrew + nvm путями. GUI-приложение из /Applications
/// наследует урезанный PATH (без /opt/homebrew/bin и ~/.nvm/.../bin) — поэтому
/// tmux (Homebrew) и claude (nvm) не находятся. Префиксуем их явно.
fn augmented_path() -> String {
    let base = std::env::var("PATH").unwrap_or_default();
    let mut extra = vec!["/opt/homebrew/bin".to_string(), "/usr/local/bin".to_string()];
    if let Ok(rd) = fs::read_dir(home().join(".nvm/versions/node")) {
        for e in rd.flatten() {
            let bin = e.path().join("bin");
            if bin.is_dir() {
                extra.push(bin.display().to_string());
            }
        }
    }
    format!("{}:{base}", extra.join(":"))
}
fn hook_dst() -> PathBuf { jarvis_dir().join("bin/jarvis-hook") }
fn mcp_dst() -> PathBuf { jarvis_dir().join("bin/jarvis-mcp") }
fn mcp_config_dst() -> PathBuf { jarvis_dir().join("jarvis-mcp.json") }

/// Выдать/прочитать токен агента в ~/.jarvis/tokens.json (0600). Самодостаточно:
/// install/mod.rs компилируется и в jarvis-setup (без `crate::capability`), поэтому
/// логику токена дублируем минимально. Формат совпадает с `capability::tokens::TokenStore`
/// (ключ "agent", 64-симв. hex) — приложение читает этот же файл.
fn ensure_agent_token() -> String {
    let path = jarvis_dir().join("tokens.json");
    let mut v: Value = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    if let Some(t) = v.get("agent").and_then(|t| t.as_str()) {
        return t.to_string();
    }
    let mut buf = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    }
    let tok: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("agent".into(), json!(tok));
    }
    let _ = fs::create_dir_all(jarvis_dir());
    if fs::write(&path, serde_json::to_string_pretty(&v).unwrap_or_default() + "\n").is_ok() {
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    tok
}

/// MCP-конфиг для `claude --strict-mcp-config --mcp-config <это>` (R5): единственный
/// сервер — наш мост; токен агента (R2) — в env, чтобы мост предъявлял его демону.
pub fn build_mcp_config(mcp_bin: &str, token: &str) -> Value {
    json!({
        "mcpServers": {
            "jarvis": {
                "command": mcp_bin,
                "env": { "JARVIS_TOKEN": token }
            }
        }
    })
}
fn shims_dir() -> PathBuf { jarvis_dir().join("shims") }
fn shim_dst() -> PathBuf { shims_dir().join("claude") }
fn codex_shim_dst() -> PathBuf { shims_dir().join("codex") }
fn tmux_conf_dst() -> PathBuf { jarvis_dir().join("tmux.conf") }
fn settings_path() -> PathBuf { home().join(".claude/settings.json") }
/// Codex: $CODEX_HOME или ~/.codex; файл регистрации хуков.
fn codex_home() -> PathBuf {
    match std::env::var("CODEX_HOME") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => home().join(".codex"),
    }
}
fn codex_hooks_path() -> PathBuf { codex_home().join("hooks.json") }
fn jarvis_settings_path() -> PathBuf { jarvis_dir().join("settings.json") }

/// Установлен ли `codex` в PATH (минуя наш шим).
fn codex_found() -> bool {
    Command::new("/bin/sh")
        .args(["-c", "command -v codex"])
        .env("PATH", augmented_path())
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Поддерживает ли установленный codex `--dangerously-bypass-hook-trust`
/// (feature-detect один раз при установке — чтобы не дёргать `codex --help`
/// на каждый интерактивный запуск из шима).
fn codex_supports_bypass_hook_trust() -> bool {
    Command::new("/bin/sh")
        .args(["-c", "codex --help 2>/dev/null | grep -q -- --dangerously-bypass-hook-trust"])
        .env("PATH", augmented_path())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/* ================= STT: Whisper + Qwen3-MLX (инкр. 9, Phase 8) ================= */

/// Каталог для Whisper-модели: ~/.jarvis/stt/
fn stt_dir() -> PathBuf { jarvis_dir().join("stt") }

/// Бинарный файл модели Whisper large-v3-turbo-q5 (~574 МБ).
fn whisper_model_path() -> PathBuf { stt_dir().join("ggml-large-v3-turbo-q5_0.bin") }

/// URL скачивания модели Whisper (HuggingFace, ggerganov).
const WHISPER_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin";

/* ===== Гибридная загрузка: huggingface.co/github — через прокси, CDN — напрямую =====
 *
 * В части сетей (корп. прокси) huggingface.co доступен ТОЛЬКО через прокси, а CDN
 * с самими весами (Xet/LFS, *.cdn.hf.co; GitHub-релизы → objects.githubusercontent.com)
 * через прокси РВЁТ CONNECT и качается только напрямую. Поэтому проходим цепочку
 * редиректов вручную и выбираем канал по хосту каждого хопа. На reqwest (а не curl):
 * нет утечки пароля прокси в argv, и нет наследования env-прокси для прямого хопа. */

/// Порядок каналов загрузки (значение = `direct`: true — напрямую, false — через
/// прокси). Сначала пробуем НАПРЯМУЮ; если не вышло и прокси задан — пробуем
/// через прокси (фолбэк). Без прокси — только прямой.
fn download_channels(has_proxy: bool) -> Vec<bool> {
    if has_proxy {
        vec![true, false] // прямой → фолбэк через прокси
    } else {
        vec![true] // только прямой
    }
}

/// Отправить GET, перебирая каналы (прямой → прокси) до первого успешного
/// коннекта. Фолбэк только на ТРАНСПОРТНЫХ ошибках (DNS/коннект/таймаут);
/// HTTP-статус (404/403/302) — валидный ответ, его возвращаем как есть. При
/// провале всех каналов — ошибка с перечислением попыток (видно в UI).
fn send_get(
    url: &reqwest::Url,
    proxy: Option<&str>,
    label: &str,
) -> Result<reqwest::blocking::Response, String> {
    let host = url.host_str().unwrap_or("").to_string();
    let has_proxy = proxy.map(|p| !p.is_empty()).unwrap_or(false);
    let mut errs = Vec::new();
    for direct in download_channels(has_proxy) {
        let how = if direct { "напрямую" } else { "через прокси" };
        let client = dl_client(proxy, direct)?;
        match client.get(url.clone()).send() {
            Ok(resp) => return Ok(resp),
            Err(e) => errs.push(format!("{how} к {host}: {e}")),
        }
    }
    Err(format!("{label}: не удалось подключиться — {}", errs.join(" · ")))
}

/// reqwest-клиент с ручной проходкой редиректов. `direct=true` → без прокси;
/// иначе через `proxy` (если задан).
fn dl_client(proxy: Option<&str>, direct: bool) -> Result<reqwest::blocking::Client, String> {
    let mut b = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent("jarvis-installer");
    b = match proxy {
        Some(p) if !direct && !p.is_empty() => {
            b.proxy(reqwest::Proxy::all(p).map_err(|e| format!("proxy: {e}"))?)
        }
        _ => b.no_proxy(),
    };
    b.build().map_err(|e| format!("http client: {e}"))
}

/// Скачать `url` в `dst` атомарно, маршрутизируя каналы по хосту каждого редиректа.
/// `expected` (если задан) сверяется с финальным размером. Прогресс — проценты по
/// Content-Length. tmp→rename. Fail-safe: при ошибке tmp удаляется, возвращается Err.
fn fetch_to_file(
    url: &str,
    dst: &Path,
    proxy: Option<&str>,
    progress: &Progress,
    label: &str,
    expected: Option<u64>,
) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut current = reqwest::Url::parse(url).map_err(|e| format!("{label}: url: {e}"))?;
    for _hop in 0..8 {
        let host = current.host_str().unwrap_or("").to_string();
        let resp = send_get(&current, proxy, label)?;
        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("{label}: редирект {status} без Location"))?;
            current = current.join(loc).map_err(|e| format!("{label}: bad Location: {e}"))?;
            continue;
        }
        if !status.is_success() {
            return Err(format!("{label}: HTTP {status} от {host}"));
        }

        // успех — стримим тело в tmp с прогрессом по Content-Length
        let total = resp.content_length().or(expected);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{label}: mkdir: {e}"))?;
        }
        let tmp = dst.with_file_name(format!(
            ".{}.tmp-{}",
            dst.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        let mut file = fs::File::create(&tmp).map_err(|e| format!("{label}: create tmp: {e}"))?;
        let mut resp = resp;
        let mut buf = [0u8; 65536];
        let mut done: u64 = 0;
        let mut last_pct: i64 = -10;
        loop {
            let n = match resp.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(format!("{label}: чтение тела: {e}"));
                }
            };
            if let Err(e) = file.write_all(&buf[..n]) {
                let _ = fs::remove_file(&tmp);
                return Err(format!("{label}: запись: {e}"));
            }
            done += n as u64;
            if let Some(t) = total {
                if t > 0 {
                    let pct = ((done as f64 / t as f64) * 100.0) as i64;
                    if pct - last_pct >= 4 || pct >= 100 {
                        last_pct = pct;
                        progress(Step::progress("Модель", format!("{label} — {pct}%"), pct.clamp(0, 100) as u8));
                    }
                }
            }
        }
        drop(file);

        // проверка целостности по ожидаемому размеру (если знаем)
        if let Some(exp) = expected {
            if exp > 0 {
                let got = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
                if got != exp {
                    let _ = fs::remove_file(&tmp);
                    return Err(format!("{label}: размер {got} != ожидаемого {exp}"));
                }
            }
        }
        fs::rename(&tmp, dst).map_err(|e| format!("{label}: rename: {e}"))?;
        return Ok(());
    }
    Err(format!("{label}: слишком много редиректов"))
}

/// Список файлов репозитория HF (`api/models/<repo>/tree/main`) — через прокси.
/// Возвращает пары (относительный путь, размер). Для LFS размер — из `lfs.size`.
fn hf_tree(repo: &str, proxy: Option<&str>) -> Result<Vec<(String, u64)>, String> {
    let url = format!("https://huggingface.co/api/models/{repo}/tree/main?recursive=1");
    let mut b = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("jarvis-installer");
    b = match proxy {
        Some(p) if !p.is_empty() => {
            b.proxy(reqwest::Proxy::all(p).map_err(|e| format!("proxy: {e}"))?)
        }
        _ => b.no_proxy(),
    };
    let client = b.build().map_err(|e| format!("http client: {e}"))?;
    let resp = client.get(&url).send().map_err(|e| format!("tree {repo}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("tree {repo}: HTTP {}", resp.status()));
    }
    let arr: Vec<Value> = resp.json().map_err(|e| format!("tree {repo}: json: {e}"))?;
    let mut files = Vec::new();
    for f in &arr {
        if f.get("type").and_then(Value::as_str) == Some("directory") {
            continue;
        }
        let path = f.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            continue;
        }
        let size = f
            .get("lfs")
            .and_then(|l| l.get("size"))
            .and_then(Value::as_u64)
            .or_else(|| f.get("size").and_then(Value::as_u64))
            .unwrap_or(0);
        files.push((path.to_string(), size));
    }
    if files.is_empty() {
        return Err(format!("tree {repo}: пустой список файлов"));
    }
    Ok(files)
}

/// Репозиторий mlx-community для ключа STT-движка Qwen.
fn qwen_repo(key: &str) -> Option<&'static str> {
    match key {
        "qwen3-0.6b" => Some("mlx-community/Qwen3-ASR-0.6B-8bit"),
        "qwen3-1.7b" => Some("mlx-community/Qwen3-ASR-1.7B-4bit"),
        _ => None,
    }
}

/// Предзагрузить веса Qwen3 в локальную папку сайдкара `models/<key>/` (гибридом).
/// Сайдкар возьмёт их локально (по `config.json`) и не пойдёт в HF. Идемпотентно:
/// пропускает файлы, чей размер уже совпал. Fail-safe.
pub fn preload_qwen(key: &str, progress: &Progress, proxy: Option<&str>) -> Result<(), String> {
    let repo = qwen_repo(key).ok_or_else(|| format!("неизвестная модель Qwen: {key}"))?;
    if qwen_weights_present(key) {
        progress(Step::info("STT-Qwen", format!("{key}: веса уже на месте")));
        return Ok(());
    }
    let dir = qwen_weights_dir(key);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {key}: {e}"))?;
    progress(Step::info("STT-Qwen", format!("{key}: получаю список файлов…")));
    let files = hf_tree(repo, proxy)?;
    progress(Step::info("STT-Qwen", format!("{key}: {} файлов, качаю (~1 ГБ)…", files.len())));
    for (path, size) in &files {
        let out = dir.join(path);
        if out.exists() && *size > 0 && fs::metadata(&out).map(|m| m.len()).unwrap_or(0) == *size {
            continue; // уже скачан и размер совпал
        }
        let url = format!("https://huggingface.co/{repo}/resolve/main/{path}");
        let exp = if *size > 0 { Some(*size) } else { None };
        fetch_to_file(&url, &out, proxy, progress, &format!("{key}/{path}"), exp)?;
    }
    progress(Step::done("STT-Qwen", format!("{key}: веса установлены (локально, без HF)")));
    Ok(())
}

/// Каталог STT-MLX сайдкара (Qwen3-ASR): ~/.jarvis/stt-mlx/
fn stt_mlx_dir() -> PathBuf { jarvis_dir().join("stt-mlx") }

fn stt_server_py() -> PathBuf { stt_mlx_dir().join("stt-server.py") }
fn stt_venv() -> PathBuf { stt_mlx_dir().join("venv") }
fn stt_python() -> PathBuf { stt_venv().join("bin/python") }
fn stt_pip() -> PathBuf { stt_venv().join("bin/pip") }

/// Скачать модель Whisper large-v3-turbo-q5_0.bin (~574 МБ) в ~/.jarvis/stt/.
/// Идемпотентно: пропускает скачивание если файл уже на месте.
/// Атомарно: скачивает во временный файл, затем переименовывает.
/// Fail-safe: ошибка возвращается как Err, демон не падает.
pub fn install_whisper(progress: &Progress, proxy: Option<&str>) -> Result<(), String> {
    fs::create_dir_all(stt_dir()).map_err(|e| format!("mkdir stt: {e}"))?;

    // Идемпотентность: если модель уже на месте — ничего не делаем.
    if whisper_model_path().exists() {
        progress(Step::info("STT-Whisper", "модель ggml-large-v3-turbo-q5_0.bin уже установлена"));
        return Ok(());
    }

    progress(Step::info(
        "STT-Whisper",
        "скачиваю ggml-large-v3-turbo-q5_0.bin (~574 МБ) — это займёт время…",
    ));

    // Гибридная загрузка: HF-resolve через прокси → CDN-блоб напрямую (атомарно).
    fetch_to_file(WHISPER_MODEL_URL, &whisper_model_path(), proxy, progress, "Whisper-модель", None)?;

    progress(Step::done("STT-Whisper", "модель установлена (~574 МБ)"));
    Ok(())
}

/// Каталог моделей wake-word: ~/.jarvis/wakeword/
fn wakeword_dir() -> PathBuf {
    jarvis_dir().join("wakeword")
}

/// 3 ONNX-модели openWakeWord (release v0.5.1, ~3.5 МБ суммарно): общий мел +
/// общий эмбеддер + детектор фразы hey_jarvis.
const WAKEWORD_MODELS: [(&str, &str); 3] = [
    (
        "melspectrogram.onnx",
        "https://github.com/dscripka/openWakeWord/releases/download/v0.5.1/melspectrogram.onnx",
    ),
    (
        "embedding_model.onnx",
        "https://github.com/dscripka/openWakeWord/releases/download/v0.5.1/embedding_model.onnx",
    ),
    (
        "hey_jarvis_v0.1.onnx",
        "https://github.com/dscripka/openWakeWord/releases/download/v0.5.1/hey_jarvis_v0.1.onnx",
    ),
];

/// Все ли 3 модели wake-word на месте.
fn wakeword_models_present() -> bool {
    WAKEWORD_MODELS.iter().all(|(f, _)| wakeword_dir().join(f).exists())
}

/// Скачать 3 ONNX-модели wake-word в ~/.jarvis/wakeword/.
/// Идемпотентно (пропуск существующих), атомарно (tmp→rename), fail-safe.
pub fn install_wakeword(progress: &Progress, proxy: Option<&str>) -> Result<(), String> {
    fs::create_dir_all(wakeword_dir()).map_err(|e| format!("mkdir wakeword: {e}"))?;
    for (name, url) in WAKEWORD_MODELS {
        let dst = wakeword_dir().join(name);
        if dst.exists() {
            progress(Step::info("wake-word", format!("{name} уже на месте")));
            continue;
        }
        // GitHub-релиз → редирект на objects.githubusercontent.com (CDN, напрямую).
        fetch_to_file(url, &dst, proxy, progress, name, None)?;
        progress(Step::done("wake-word", format!("{name} установлена")));
    }
    Ok(())
}

/// Установить STT-MLX-сайдкар (Qwen3-ASR): stt-server.py + venv + зависимости.
/// Идемпотентно: пропускает шаги где результат уже есть.
/// Fail-safe: ошибка возвращается как Err, демон не падает.
///
/// Зависимости: qwen3-asr-mlx mlx-audio fastapi uvicorn numpy certifi
/// Модели Qwen3 скачиваются сайдкаром при первом запросе (HuggingFace Hub).
pub fn install_stt_sidecar(progress: &Progress, proxy: Option<&str>) -> Result<(), String> {
    fs::create_dir_all(stt_mlx_dir()).map_err(|e| format!("mkdir stt-mlx: {e}"))?;

    // 1. Обновить server.py — атомарная запись поверх (держим актуальным).
    atomic_write(&stt_server_py(), STT_SERVER_SRC);
    progress(Step::info("STT-MLX", "stt-server.py установлен"));

    // 2. venv — если ещё нет интерпретатора.
    if !stt_python().exists() {
        progress(Step::info("STT-MLX", "создаю Python-venv…"));
        run_inherit(
            "python3 -m venv (stt-mlx)",
            Command::new("python3").env("PATH", augmented_path()).arg("-m").arg("venv").arg(stt_venv()),
        )?;
    }

    // 3. Зависимости — идемпотентно: пропускаем если уже импортируются.
    let deps_ok = Command::new(stt_python())
        .args(["-c", "import fastapi, uvicorn, numpy, certifi"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !deps_ok {
        progress(Step::info(
            "STT-MLX",
            "ставлю зависимости (qwen3-asr-mlx mlx-audio fastapi uvicorn numpy certifi) — ~2.6 ГБ для MLX-моделей…",
        ));

        let mut up = Command::new(stt_pip());
        up.args(["install", "--upgrade", "pip"]);
        set_proxy(&mut up, proxy);
        run_inherit("pip install --upgrade pip (stt)", &mut up)?;

        let proxy_arg = match proxy {
            Some(p) if !p.is_empty() => format!("--proxy '{p}' "),
            _ => String::new(),
        };
        let cmd = format!(
            "'{}' install --progress-bar raw {}qwen3-asr-mlx mlx-audio fastapi uvicorn numpy certifi",
            stt_pip().display(),
            proxy_arg,
        );
        run_streamed("pip install stt deps", &cmd, proxy, progress, "STT-зависимости")?;
    } else {
        progress(Step::info("STT-MLX", "зависимости уже установлены"));
    }

    // Примечание: модели Qwen3 (HuggingFace Hub) сайдкар скачает сам при первом запросе.
    // Принудительного preload здесь нет — пользователь может это сделать вручную.
    progress(Step::done("STT-MLX", "сайдкар установлен (модели Qwen3 загрузятся при первом запуске)"));
    Ok(())
}

/* ====== Codex-SDK сайдкар (openai-codex): служебный LLM «под капотом» ====== */

fn codex_sdk_dir() -> PathBuf { jarvis_dir().join("codex-sdk") }
fn codex_sdk_venv() -> PathBuf { codex_sdk_dir().join("venv") }
fn codex_sdk_python() -> PathBuf { codex_sdk_venv().join("bin/python") }
fn codex_sdk_pip() -> PathBuf { codex_sdk_venv().join("bin/pip") }
fn codex_sdk_script() -> PathBuf { codex_sdk_dir().join("codex-summary.py") }

/// Установлен ли Codex-SDK сайдкар: venv с `openai_codex` + скрипт на месте.
/// Источник правды для гейта настроек «Под капотом» (кнопка «Установить»).
pub fn codex_sdk_sidecar_present() -> bool {
    codex_sdk_python().exists() && codex_sdk_script().exists()
}

/// Установить Codex-SDK сайдкар: codex-summary.py + venv + `openai-codex`.
/// Идемпотентно; fail-safe (ошибка → Err, демон не падает). Зависимость одна —
/// `openai-codex` (тащит pinned codex-бинарь). Авторизация переиспользует
/// существующий `codex login` (~/.codex/auth.json) — ключ API не нужен.
pub fn install_codex_sdk_sidecar(progress: &Progress, proxy: Option<&str>) -> Result<(), String> {
    fs::create_dir_all(codex_sdk_dir()).map_err(|e| format!("mkdir codex-sdk: {e}"))?;
    atomic_write(&codex_sdk_script(), CODEX_SUMMARY_SRC);
    progress(Step::info("Codex-SDK", "codex-summary.py установлен"));

    if !codex_sdk_python().exists() {
        progress(Step::info("Codex-SDK", "создаю Python-venv…"));
        run_inherit(
            "python3 -m venv (codex-sdk)",
            Command::new("python3")
                .env("PATH", augmented_path())
                .arg("-m")
                .arg("venv")
                .arg(codex_sdk_venv()),
        )?;
    }

    let dep_ok = Command::new(codex_sdk_python())
        .args(["-c", "import openai_codex"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !dep_ok {
        progress(Step::info(
            "Codex-SDK",
            "ставлю openai-codex (Python Codex SDK + pinned codex-бинарь)…",
        ));
        let mut up = Command::new(codex_sdk_pip());
        up.args(["install", "--upgrade", "pip"]);
        set_proxy(&mut up, proxy);
        run_inherit("pip install --upgrade pip (codex-sdk)", &mut up)?;
        let proxy_arg = match proxy {
            Some(p) if !p.is_empty() => format!("--proxy '{p}' "),
            _ => String::new(),
        };
        let cmd = format!(
            "'{}' install --progress-bar raw {}openai-codex",
            codex_sdk_pip().display(),
            proxy_arg,
        );
        run_streamed("pip install openai-codex", &cmd, proxy, progress, "Codex-SDK")?;
    } else {
        progress(Step::info("Codex-SDK", "openai-codex уже установлен"));
    }

    progress(Step::done(
        "Codex-SDK",
        "сайдкар установлен (авторизация — существующий codex login)",
    ));
    Ok(())
}

/* ================= Silero: Python-sidecar (venv + torch + модель) ================= */

fn silero_dir() -> PathBuf { jarvis_dir().join("silero") }

/* ===== MediaRemote-адаптер: пауза чужого медиа на время озвучки ===== */
fn mra_dir() -> PathBuf { jarvis_dir().join("mediaremote-adapter") }

/// Положить perl-скрипт + фреймворк адаптера в ~/.jarvis. Идемпотентно.
fn install_mediaremote() {
    let fw = mra_dir().join("MediaRemoteAdapter.framework");
    if fs::create_dir_all(&fw).is_err() {
        return;
    }
    atomic_write(&mra_dir().join("mediaremote-adapter.pl"), MRA_PL_SRC);
    let bin = fw.join("MediaRemoteAdapter");
    if fs::write(&bin, MRA_FW_SRC).is_ok() {
        let _ = fs::set_permissions(&bin, fs::Permissions::from_mode(0o755));
    }
}
fn silero_server_py() -> PathBuf { silero_dir().join("silero-server.py") }
fn silero_venv() -> PathBuf { silero_dir().join("venv") }
fn silero_python() -> PathBuf { silero_venv().join("bin/python") }
fn silero_pip() -> PathBuf { silero_venv().join("bin/pip") }

/// Прогнать команду, наследуя stdout/stderr. Ошибка/ненулевой код → Err.
fn run_inherit(what: &str, cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("запуск {what}: {e}"))?;
    if !status.success() {
        return Err(format!("{what} вернул код {}", status.code().unwrap_or(-1)));
    }
    Ok(())
}

/// Проставить proxy-окружение команде (для pip/torch), если задан.
fn set_proxy(cmd: &mut Command, proxy: Option<&str>) {
    if let Some(p) = proxy {
        if !p.is_empty() {
            cmd.env("HTTP_PROXY", p).env("HTTPS_PROXY", p);
        }
    }
}

/// Запустить shell-команду, слив stdout+stderr, и стримить прогресс скачивания.
/// pip с `--progress-bar raw` печатает строки `Progress <done> of <total>` —
/// парсим их в проценты и шлём минималистичный `Step::info`.
fn run_streamed(label: &str, shell_cmd: &str, proxy: Option<&str>, progress: &Progress, what: &str) -> Result<(), String> {
    use std::io::{BufRead, BufReader};
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(format!("{shell_cmd} 2>&1"));
    set_proxy(&mut cmd, proxy);
    cmd.stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("запуск {label}: {e}"))?;
    let out = child.stdout.take().ok_or_else(|| format!("{label}: нет stdout"))?;
    let mut last: i16 = -10;
    for line in BufReader::new(out).lines().map_while(Result::ok) {
        if let Some(rest) = line.strip_prefix("Progress ") {
            let nums: Vec<u64> = rest.split(" of ").filter_map(|s| s.trim().parse::<u64>().ok()).collect();
            if nums.len() == 2 && nums[1] > 0 {
                let pct = ((nums[0] as f64 / nums[1] as f64) * 100.0).round() as i16;
                if (pct - last).abs() >= 4 || pct >= 100 {
                    last = pct;
                    progress(Step::info("Голос", format!("{what} — {pct}%")));
                }
            }
        }
    }
    let status = child.wait().map_err(|e| format!("wait {label}: {e}"))?;
    if !status.success() {
        return Err(format!("{label} код {}", status.code().unwrap_or(-1)));
    }
    Ok(())
}

/// Установить Silero-сайдкар: server.py + venv с torch(CPU)/fastapi/uvicorn/numpy
/// + прогрев модели. Веса PyTorch — сотни МБ–ГБ, ставятся один раз. Идемпотентно.
/// `proxy` (если задан) идёт в окружение pip/torch. `progress` стримит проценты.
fn install_silero(progress: &Progress, proxy: Option<&str>) -> Result<(), String> {
    fs::create_dir_all(silero_dir()).map_err(|e| format!("mkdir silero: {e}"))?;

    // 1. server.py — атомарная запись поверх (держим актуальным).
    atomic_write(&silero_server_py(), SILERO_SERVER_SRC);

    // 2. venv — если ещё нет интерпретатора.
    if !silero_python().exists() {
        progress(Step::info("Голос", "создаю Python-venv…"));
        run_inherit(
            "python3 -m venv",
            Command::new("python3").env("PATH", augmented_path()).arg("-m").arg("venv").arg(silero_venv()),
        )?;
    }

    // 3. Зависимости — идемпотентно: пропускаем, если уже импортируются.
    let deps_ok = Command::new(silero_python())
        .args(["-c", "import torch, fastapi, uvicorn, numpy, certifi, omegaconf"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !deps_ok {
        progress(Step::info("Голос", "ставлю PyTorch CPU — сотни МБ–ГБ, это надолго…"));
        let mut up = Command::new(silero_pip());
        up.args(["install", "--upgrade", "pip"]);
        set_proxy(&mut up, proxy);
        run_inherit("pip install --upgrade pip", &mut up)?;

        // основная установка — со стримом процентов (pip --progress-bar raw)
        let proxy_arg = match proxy {
            Some(p) if !p.is_empty() => format!("--proxy '{p}' "),
            _ => String::new(),
        };
        let cmd = format!(
            "'{}' install --progress-bar raw {}torch fastapi uvicorn numpy certifi omegaconf",
            silero_pip().display(),
            proxy_arg,
        );
        run_streamed("pip install torch+deps", &cmd, proxy, progress, "Скачиваю PyTorch")?;
    }

    // 4. Прогрев + скачивание модели через torch.hub. SSL_CERT_FILE из certifi —
    //    иначе torch.hub падает на верификации HTTPS. Прокси — в окружение.
    progress(Step::info("Голос", "скачиваю модель Silero…"));
    let ca = Command::new(silero_python())
        .args(["-c", "import certifi; print(certifi.where())"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let mut warm = Command::new(silero_python());
    warm.env("SSL_CERT_FILE", &ca);
    set_proxy(&mut warm, proxy);
    warm.args([
        "-c",
        "import torch; torch.hub.load('snakers4/silero-models','silero_tts',language='ru',speaker='v4_ru',trust_repo=True)",
    ]);
    run_inherit("прогрев модели Silero", &mut warm)?;

    Ok(())
}

/// Активный STT-движок из ~/.jarvis/settings.json (stt.engine), дефолт "qwen3-0.6b".
fn stt_engine() -> String {
    let raw = match fs::read_to_string(jarvis_settings_path()) {
        Ok(raw) => raw,
        Err(_) => return "qwen3-0.6b".into(),
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v.pointer("/stt/engine").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| "qwen3-0.6b".into())
}

/// Активный голосовой движок из ~/.jarvis/settings.json (voice.engine), дефолт "silero".
fn voice_engine() -> String {
    let raw = match fs::read_to_string(jarvis_settings_path()) {
        Ok(raw) => raw,
        Err(_) => return "silero".into(),
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v.pointer("/voice/engine").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| "silero".into())
}

/* ================= managed-блок в rc-файлах ================= */

pub const BEGIN: &str = "# >>> jarvis >>>";
pub const END: &str = "# <<< jarvis <<<";

fn block_body(shims_dir: &str) -> String {
    format!(
        "{BEGIN}\n# Управляется Jarvis (npm run setup/teardown) — не редактируй вручную\nexport PATH=\"{shims_dir}:$PATH\"\n{END}"
    )
}

fn has_block(content: &str) -> bool {
    content.contains(BEGIN) && content.contains(END)
}

/// Вставить или заменить блок. Идемпотентно.
fn merge_block(content: &str, shims_dir: &str) -> String {
    let block = block_body(shims_dir);
    if has_block(content) {
        let re = regex::Regex::new(&format!(
            "{}[\\s\\S]*?{}",
            regex::escape(BEGIN),
            regex::escape(END)
        ))
        .unwrap();
        return re.replace_all(content, regex::NoExpand(block.as_str())).into_owned();
    }
    let sep = if !content.is_empty() && !content.ends_with('\n') { "\n" } else { "" };
    format!("{content}{sep}\n{block}\n")
}

/// Убрать блок вместе с окружающими пустыми строками.
fn remove_block(content: &str) -> String {
    if !has_block(content) {
        return content.to_string();
    }
    let re = regex::Regex::new(&format!(
        "\\n*{}[\\s\\S]*?{}\\n?",
        regex::escape(BEGIN),
        regex::escape(END)
    ))
    .unwrap();
    let out = re.replace_all(content, "\n").into_owned();
    regex::Regex::new("\n{3,}").unwrap().replace_all(&out, "\n\n").into_owned()
}

/* ================= helpers ================= */

/// Прочитать файл регистрации хуков (settings.json claude / hooks.json codex).
/// (есть_ли_файл, json). Битый/нечитаемый → Err (НЕ выходим — зовётся в демоне).
fn read_hooks_file(path: &Path) -> Result<(bool, Value), String> {
    if !path.exists() {
        return Ok((false, json!({})));
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("не смог прочитать {}: {e}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok((true, json!({})));
    }
    serde_json::from_str(&raw)
        .map(|v| (true, v))
        .map_err(|_| format!("{} содержит невалидный JSON — не трогаю", path.display()))
}

/// Прочитать ~/.claude/settings.json.
fn read_settings() -> Result<(bool, Value), String> {
    read_hooks_file(&settings_path())
}

/// Смержить наши хуки (метка = claude|codex) в файл регистрации агента.
///
/// Идемпотентно И самолечаще: для каждого события держим РОВНО один наш хук с
/// актуальной командой (`<hook_dst> <label> <arg>`). Чужие хуки сохраняем.
///
/// Важно: `is_ours` матчит лишь по подстроке "bin/jarvis-hook" — без пути и
/// метки. Поэтому устаревший наш хук (старый prod-путь ~/.jarvis вместо живого
/// JARVIS_DIR, или метка `claude` у codex-файла) ВЫГЛЯДИТ «уже установленным».
/// Старая версия его пропускала → codex дёргал несуществующий бинарь и хуки
/// молчали. Теперь устаревшие наши записи снимаем и переписываем на верную —
/// файл правится, только если что-то реально изменилось (без лишних бэкапов на
/// каждом старте демона).
fn install_hooks_into(path: &Path, label: &str, events: &[(&str, &str)], progress: &Progress) {
    match read_hooks_file(path) {
        Ok((exists, mut json)) => {
            if !json.is_object() {
                json = json!({});
            }
            if json.get("hooks").map(|h| !h.is_object()).unwrap_or(true) {
                json["hooks"] = json!({});
            }
            let mut added = Vec::new();
            let mut healed = Vec::new();
            for (event, arg) in events {
                let want = format!("{} {label} {arg}", hook_dst().display());
                let hooks = json["hooks"].as_object_mut().unwrap();
                let arr = hooks.entry(*event).or_insert_with(|| json!([]));
                if !arr.is_array() {
                    *arr = json!([]);
                }
                let arr = arr.as_array_mut().unwrap();

                let has_correct = arr.iter().any(|g| group_has_cmd(g, &want));
                let stale_present = arr.iter().any(|g| group_has_stale_ours(g, &want));
                if has_correct && !stale_present {
                    continue; // уже верно — не трогаем (иначе бэкап+запись на каждом старте)
                }

                // Снимаем ВСЕ наши хуки (любой путь/метка), чужие — оставляем.
                for group in arr.iter_mut() {
                    if let Some(gh) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                        gh.retain(|h| !is_ours(h));
                    }
                }
                arr.retain(|g| g.get("hooks").and_then(Value::as_array).is_some_and(|h| !h.is_empty()));

                // Ставим единственный правильный.
                arr.push(json!({
                    "hooks": [{ "type": "command", "command": want, "timeout": 5 }],
                }));
                if stale_present {
                    healed.push(*event);
                } else {
                    added.push(*event);
                }
            }
            if added.is_empty() && healed.is_empty() {
                progress(Step::done("Хуки", format!("{label}: уже установлены")));
                return;
            }
            if exists {
                backup(path);
            }
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            atomic_write(path, &(serde_json::to_string_pretty(&json).unwrap() + "\n"));
            let mut msg = String::new();
            if !added.is_empty() {
                msg += &format!("добавлены {}", added.join(", "));
            }
            if !healed.is_empty() {
                if !msg.is_empty() {
                    msg += "; ";
                }
                msg += &format!("исправлены {}", healed.join(", "));
            }
            progress(Step::done("Хуки", format!("{label}: {msg}")));
        }
        Err(e) => progress(Step::warn("Хуки", format!("{e} — пропускаю хуки {label}"))),
    }
}

/// Снять наши хуки (любой метки — MARKER агент-агностичен) из файла агента.
fn uninstall_hooks_from(path: &Path, progress: &Progress) {
    match read_hooks_file(path) {
        Ok((true, mut json)) if json.get("hooks").and_then(Value::as_object).is_some() => {
            backup(path);
            let mut removed = Vec::new();
            let hooks = json["hooks"].as_object_mut().unwrap();
            let events: Vec<String> = hooks.keys().cloned().collect();
            for event in events {
                let Some(arr) = hooks.get_mut(&event).and_then(Value::as_array_mut) else { continue };
                let before = arr.len();
                for group in arr.iter_mut() {
                    if let Some(gh) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                        gh.retain(|h| !is_ours(h));
                    }
                }
                arr.retain(|g| g.get("hooks").and_then(Value::as_array).is_some_and(|h| !h.is_empty()));
                if arr.len() != before {
                    removed.push(event.clone());
                }
                if arr.is_empty() {
                    hooks.remove(&event);
                }
            }
            if hooks.is_empty() {
                json.as_object_mut().unwrap().remove("hooks");
            }
            atomic_write(path, &(serde_json::to_string_pretty(&json).unwrap() + "\n"));
            if !removed.is_empty() {
                progress(Step::done("Хуки", format!("{}: удалены {}", path.display(), removed.join(", "))));
            }
        }
        Ok(_) => {} // файла/хуков нет — тихо
        Err(e) => progress(Step::warn("Хуки", e)),
    }
}

fn atomic_write(file: &Path, content: &str) {
    let tmp = file.with_file_name(format!(
        ".{}.tmp-{}",
        file.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&tmp, content).expect("запись tmp-файла");
    fs::rename(&tmp, file).expect("rename tmp-файла");
}

fn backup(file: &Path) -> Option<PathBuf> {
    if !file.exists() {
        return None;
    }
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ");
    let dst = PathBuf::from(format!("{}.bak-{stamp}", file.display()));
    fs::copy(file, &dst).ok()?;
    Some(dst)
}

fn is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(MARKER))
}

fn group_has_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| hooks.iter().any(is_ours))
}

/// Содержит ли группа хук ровно с этой командой (наш правильный, актуальный).
fn group_has_cmd(group: &Value, cmd: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks
                .iter()
                .any(|h| h.get("command").and_then(Value::as_str) == Some(cmd))
        })
}

/// Содержит ли группа УСТАРЕВШИЙ наш хук — наш по MARKER, но команда не совпадает
/// с актуальной (другой путь/метка). Именно его нужно вылечить.
fn group_has_stale_ours(group: &Value, want: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks
                .iter()
                .any(|h| is_ours(h) && h.get("command").and_then(Value::as_str) != Some(want))
        })
}

/// Все ли наши хуки в файле стоят с АКТУАЛЬНОЙ командой (`<hook_dst> <label> <arg>`).
/// Строже `event_installed` (тот матчит лишь подстроку "bin/jarvis-hook"): ловит
/// дрейф пути/метки — старый prod-путь ~/.jarvis или метку `claude` в codex-файле,
/// из-за чего «хуки молчат». Источник правды для health-самопроверки на старте.
fn hooks_all_correct(path: &Path, label: &str, events: &[(&str, &str)]) -> bool {
    let Ok((true, json)) = read_hooks_file(path) else {
        return false;
    };
    events.iter().all(|(event, arg)| {
        let want = format!("{} {label} {arg}", hook_dst().display());
        json.pointer(&format!("/hooks/{event}"))
            .and_then(Value::as_array)
            .is_some_and(|arr| arr.iter().any(|g| group_has_cmd(g, &want)))
    })
}

/// Health-снимок интеграции (read-only, без сети) — для самопроверки на старте
/// демона и для раздела «Интеграция» в настройках.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationHealth {
    /// Активный корень установки (~/.jarvis или ~/.jarvis-dev) — на него нацелены хуки.
    pub jarvis_dir: String,
    /// Бинарь `<jarvis_dir>/bin/jarvis-hook` на месте (иначе хуки = exit 127).
    pub hook_bin: bool,
    /// Сокет демона существует (события есть куда слать).
    pub socket: bool,
    /// Все хуки Claude в ~/.claude/settings.json — с актуальной командой.
    pub claude_hooks_ok: bool,
    /// `codex` найден в PATH (иначе codex-интеграция неприменима).
    pub codex_present: bool,
    /// Все хуки Codex в ~/.codex/hooks.json — с актуальной командой (n/a → true).
    pub codex_hooks_ok: bool,
    /// Шим `claude` установлен в shims-каталог.
    pub claude_shim: bool,
    /// Шим `codex` установлен (нужен для bypass-hook-trust при запуске codex).
    pub codex_shim: bool,
}

impl IntegrationHealth {
    /// Всё критичное на месте: бинарь хука + корректные регистрации обоих агентов
    /// (codex учитывается, только если установлен).
    pub fn ok(&self) -> bool {
        self.hook_bin && self.claude_hooks_ok && (!self.codex_present || self.codex_hooks_ok)
    }
}

/// Снять health-снимок интеграции. `codex_found()` дёргает `command -v` — несколько мс.
pub fn integration_health() -> IntegrationHealth {
    let codex_present = codex_found();
    IntegrationHealth {
        jarvis_dir: jarvis_dir().display().to_string(),
        hook_bin: hook_dst().exists(),
        socket: jarvis_dir().join("run.sock").exists(),
        claude_hooks_ok: hooks_all_correct(&settings_path(), "claude", &EVENTS),
        codex_present,
        codex_hooks_ok: !codex_present
            || hooks_all_correct(&codex_hooks_path(), "codex", &CODEX_EVENTS),
        claude_shim: shim_dst().exists(),
        codex_shim: codex_shim_dst().exists(),
    }
}

/// Самолечение регистраций хуков на старте демона: приводит ~/.claude/settings.json
/// и (если codex установлен) ~/.codex/hooks.json к актуальному JARVIS_DIR и метке.
/// Дёшево (только файлы + `command -v`), ничего не качает; идемпотентно — если всё
/// верно, файлы не переписываются. Лечит главный баг: stale prod-путь/метка после
/// смены dev↔prod профиля, из-за которого codex дёргал несуществующий бинарь.
pub fn reconcile_hooks(progress: &Progress) {
    install_hooks_into(&settings_path(), "claude", &EVENTS, progress);
    if codex_found() {
        install_hooks_into(&codex_hooks_path(), "codex", &CODEX_EVENTS, progress);
    }
}

/// Починить/обновить ТОЛЬКО интеграцию агентов: хуки (claude+codex) + транспорт-шим
/// (claude+codex с bypass-hook-trust) + PATH-блок. БЕЗ тяжёлых шагов (Silero/STT/
/// модели) — в отличие от `install`. Идемпотентно. Доустанавливает codex-шим, если
/// codex появился после первичной установки, и лечит дрейф путей/меток в хуках.
/// Для `jarvis-setup repair` и кнопки «Починить интеграцию» в настройках.
pub fn repair(progress: &Progress) {
    progress(Step::start("Хуки"));
    reconcile_hooks(progress);
    progress(Step::start("Транспорт"));
    install_tmux_transport(progress);
    let h = integration_health();
    progress(Step::info(
        "Интеграция",
        format!(
            "dir={} hook_bin={} claude_hooks={} codex={} codex_hooks={} codex_shim={} → {}",
            h.jarvis_dir,
            h.hook_bin,
            h.claude_hooks_ok,
            h.codex_present,
            h.codex_hooks_ok,
            h.codex_shim,
            if h.ok() { "OK" } else { "НЕПОЛНО" },
        ),
    ));
}

fn event_installed(json: &Value, event: &str) -> bool {
    json.pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(group_has_ours))
}

fn claude_found() -> bool {
    Command::new("/bin/sh")
        .args(["-c", "command -v claude"])
        .env("PATH", augmented_path())
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tmux_found() -> bool {
    Command::new("tmux")
        .arg("-V")
        .env("PATH", augmented_path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// rc-файлы, которые правим: zsh всегда, bash — если он login shell.
fn rc_files() -> Vec<PathBuf> {
    let mut files = vec![home().join(".zshrc")];
    if std::env::var("SHELL").unwrap_or_default().ends_with("bash") {
        files.push(home().join(".bashrc"));
        files.push(home().join(".bash_profile"));
    }
    files
}

fn live_tmux_sessions() -> Vec<String> {
    Command::new("tmux")
        .args(["-L", "jarvis", "list-sessions", "-F", "#{session_name}"])
        .env("PATH", augmented_path())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(String::from)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn write_executable(dst: &Path, content: &str) {
    fs::create_dir_all(dst.parent().unwrap()).expect("mkdir");
    fs::write(dst, content).expect("запись шима");
    fs::set_permissions(dst, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/* ================= публичный API: status / install / uninstall ================= */

/// Что из интеграции стоит сейчас (для онбординга и `status`).
pub fn status() -> Status {
    let hooks = match read_settings() {
        Ok((true, json)) => EVENTS.iter().all(|(e, _)| event_installed(&json, e)),
        _ => false,
    };
    Status {
        hooks,
        shim: shim_dst().exists(),
        tmux_conf: tmux_conf_dst().exists(),
        path_block: rc_files()
            .iter()
            .any(|rc| rc.exists() && has_block(&fs::read_to_string(rc).unwrap_or_default())),
        silero: silero_python().exists() && silero_server_py().exists(),
        whisper_model: whisper_model_path().exists(),
        whisper_native_built: cfg!(feature = "whisper-native"),
        qwen3_sidecar: stt_python().exists() && stt_server_py().exists(),
        codex_sdk_sidecar: codex_sdk_sidecar_present(),
        stt_engine_active: stt_engine(),
        wakeword_models: wakeword_models_present(),
    }
}

/// Полный текстовый статус (для CLI `status`).
pub fn status_report() -> String {
    let mark = |ok: bool| if ok { "✓" } else { "✗" };
    let mut out = String::new();
    out += &format!(
        "Шим:      {}\n",
        if hook_dst().exists() { format!("✓ {}", hook_dst().display()) } else { "✗ не установлен".into() }
    );
    out += &format!(
        "Сокет:    {}\n",
        if jarvis_dir().join("run.sock").exists() { "✓ демон, похоже, запущен" } else { "✗ демон не запущен" }
    );
    out += &format!("claude:   {}\n", if claude_found() { "✓ найден в PATH" } else { "✗ не найден" });
    out += "tmux-транспорт:\n";
    out += &format!("  {}\n", if tmux_found() { "✓ tmux в PATH" } else { "✗ tmux не установлен (brew install tmux)" });
    out += &format!("  {} шим claude ({})\n", mark(shim_dst().exists()), shim_dst().display());
    out += &format!("  {} конфиг ({})\n", mark(tmux_conf_dst().exists()), tmux_conf_dst().display());
    for rc in rc_files() {
        let ok = rc.exists() && has_block(&fs::read_to_string(&rc).unwrap_or_default());
        out += &format!("  {} PATH-блок в {}\n", mark(ok), rc.display());
    }
    let live = live_tmux_sessions();
    if !live.is_empty() {
        out += &format!("  • живые сессии: {}\n", live.join(", "));
    }
    let engine = voice_engine();
    let yn = |b: bool| if b { "да" } else { "нет" };
    let silero_installed = silero_python().exists() && silero_server_py().exists();
    out += "Голос:\n";
    out += &format!(
        "  silero: установлен={} (venv + server.py), активен={} (voice.engine={engine})\n",
        yn(silero_installed), yn(engine == "silero"),
    );
    let stt_eng = stt_engine();
    let whisper_ok = whisper_model_path().exists();
    let qwen3_ok = stt_python().exists() && stt_server_py().exists();
    out += "STT (диктовка, инкр. 9):\n";
    out += &format!("  whisper-turbo: модель={} ({})\n", yn(whisper_ok), whisper_model_path().display());
    out += &format!("  qwen3-mlx-сайдкар: установлен={} ({})\n", yn(qwen3_ok), stt_mlx_dir().display());
    out += &format!("  активный движок: stt.engine={stt_eng}\n");
    out += "Wake-word (инкр. 10):\n";
    out += &format!(
        "  модели openWakeWord: {} ({})\n",
        yn(wakeword_models_present()),
        wakeword_dir().display()
    );
    match read_settings() {
        Ok((true, json)) => {
            out += &format!("Settings: {}\n", settings_path().display());
            for (event, _) in EVENTS {
                out += &format!("  {} {event}\n", mark(event_installed(&json, event)));
            }
        }
        Ok((false, _)) => out += &format!("Settings: ✗ {} не существует\n", settings_path().display()),
        Err(e) => out += &format!("Settings: ⚠ {e}\n"),
    }
    out += &format!("Codex:    {}\n", if codex_found() { "✓ найден в PATH" } else { "✗ не найден" });
    if codex_found() {
        match read_hooks_file(&codex_hooks_path()) {
            Ok((true, json)) => {
                out += &format!("  hooks.json: {}\n", codex_hooks_path().display());
                for (event, _) in CODEX_EVENTS {
                    out += &format!("    {} {event}\n", mark(event_installed(&json, event)));
                }
            }
            Ok((false, _)) => out += &format!("  hooks.json: ✗ {} не существует\n", codex_hooks_path().display()),
            Err(e) => out += &format!("  hooks.json: ⚠ {e}\n"),
        }
        out += &format!("  {} шим codex ({})\n", mark(codex_shim_dst().exists()), codex_shim_dst().display());
    }
    out
}

/// Установить интеграцию. Шлёт шаги в `progress`. `proxy` (если задан) идёт в
/// окружение pip/torch — чтобы скачивать модели из-под прокси. Каждая фаза
/// fail-safe: сбой одной (например, голоса) не валит остальные и не паникует.
pub fn install(progress: &Progress, proxy: Option<&str>) {
    // --- Фаза «Хуки» ---
    progress(Step::start("Хуки"));
    write_executable(&hook_dst(), HOOK_SRC);

    // R5: мост агента (jarvis-mcp) + токен + MCP-конфиг. Fail-safe: сбой не валит
    // установку интеграции — просто агент будет недоступен. jarvis-mcp — это
    // компилируемый бинарь-сиблинг текущего exe (в dev и в бандле .app).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let src = dir.join("jarvis-mcp");
            if src.exists() {
                let _ = fs::create_dir_all(mcp_dst().parent().unwrap());
                if fs::copy(&src, mcp_dst()).is_ok() {
                    let _ = fs::set_permissions(&mcp_dst(), fs::Permissions::from_mode(0o755));
                }
                let token = ensure_agent_token();
                let cfg = build_mcp_config(&mcp_dst().to_string_lossy(), &token);
                atomic_write(&mcp_config_dst(), &(serde_json::to_string_pretty(&cfg).unwrap() + "\n"));
            } else {
                eprintln!("[jarvis:install] jarvis-mcp рядом с exe не найден — агент будет недоступен");
            }
        }
    }

    if !claude_found() {
        progress(Step::info("Хуки", "claude не найден в PATH — хуки подхватятся, когда появится"));
    }
    // Claude — всегда (хуки ждут появления claude). Codex — только если установлен
    // (иначе незачем создавать ~/.codex/hooks.json для несуществующего CLI).
    install_hooks_into(&settings_path(), "claude", &EVENTS, progress);
    if codex_found() {
        install_hooks_into(&codex_hooks_path(), "codex", &CODEX_EVENTS, progress);
    }

    // --- Фаза «Транспорт» (шим claude + tmux.conf + PATH-блок) ---
    progress(Step::start("Транспорт"));
    install_tmux_transport(progress);

    // медиа-адаптер для паузы чужого звука (мгновенно, тихо)
    install_mediaremote();

    // --- Фаза «Голос» (Silero) — не-фатально ---
    progress(Step::start("Голос"));
    match install_silero(progress, proxy) {
        Ok(()) => progress(Step::done("Голос", "Silero установлен (venv + модель)")),
        Err(e) => progress(Step::warn("Голос", format!("Silero не установлен ({e}); демон не затронут"))),
    }

    // --- Фаза «STT: Whisper» (инкр. 9) — не-фатально ---
    // Скачивание НЕ выполняется в автоматическом install — модель ~574 МБ.
    // install_whisper() вызывается отдельно из UI/setup по запросу пользователя.
    // Здесь только логируем статус.
    progress(Step::info(
        "STT",
        &format!(
            "Whisper-модель: {}; Qwen3-сайдкар: {} (запустить setup для установки)",
            if whisper_model_path().exists() { "установлена" } else { "не установлена" },
            if stt_python().exists() { "установлен" } else { "не установлен" },
        ),
    ));

    // --- Фаза «STT: Qwen3-MLX сайдкар» (инкр. 9) — не-фатально, ставим venv+deps ---
    progress(Step::start("STT-MLX"));
    match install_stt_sidecar(progress, proxy) {
        Ok(()) => {} // Step::done уже послан внутри
        Err(e) => progress(Step::warn("STT-MLX", format!("сайдкар не установлен ({e}); STT недоступен, остальное не затронуто"))),
    }
}

fn install_tmux_transport(progress: &Progress) {
    if !tmux_found() {
        progress(Step::warn("Транспорт", "tmux не найден (brew install tmux) — ввод-транспорт пропущен; уведомления работают"));
        return;
    }
    // Запекаем актуальный JARVIS_DIR в шим: в рантайме (обычный терминал) env
    // JARVIS_DIR не выставлен, а dev-сборка живёт в ~/.jarvis-dev. Без подмены
    // дефолта шим искал бы tmux.conf в ~/.jarvis и падал (No such file).
    let mut shim = SHIM_SRC.replacen(
        "JARVIS_DIR=\"${JARVIS_DIR:-$HOME/.jarvis}\"",
        &format!("JARVIS_DIR=\"${{JARVIS_DIR:-{}}}\"", jarvis_dir().display()),
        1,
    );
    // Codex: запекаем флаг bypass-hook-trust, если он поддерживается (feature-detect
    // один раз при установке — чтобы шим не дёргал `codex --help` на каждый запуск).
    let bypass = if codex_found() && codex_supports_bypass_hook_trust() {
        "--dangerously-bypass-hook-trust"
    } else {
        ""
    };
    shim = shim.replacen("CODEX_BYPASS=''", &format!("CODEX_BYPASS='{bypass}'"), 1);
    write_executable(&shim_dst(), &shim); // ~/.jarvis/shims/claude
    if codex_found() {
        // тот же скрипт под именем codex — поведение выбирается по basename "$0".
        write_executable(&codex_shim_dst(), &shim);
    }
    fs::write(tmux_conf_dst(), TMUX_CONF_SRC).expect("запись tmux.conf");

    let shims = shims_dir().display().to_string();
    for rc in rc_files() {
        let existed = rc.exists();
        let content = if existed { fs::read_to_string(&rc).unwrap_or_default() } else { String::new() };
        let merged = merge_block(&content, &shims);
        if merged != content {
            if existed {
                backup(&rc);
            }
            atomic_write(&rc, &merged);
        }
    }
    progress(Step::done(
        "Транспорт",
        if codex_found() { "шим claude+codex + tmux.conf + PATH-блок" } else { "шим claude + tmux.conf + PATH-блок" },
    ));
}

/// Снять интеграцию. Шлёт шаги в `progress`.
pub fn uninstall(progress: &Progress) {
    progress(Step::start("Хуки"));
    uninstall_hooks_from(&settings_path(), progress);
    uninstall_hooks_from(&codex_hooks_path(), progress);
    progress(Step::done("Хуки", "записи Jarvis сняты (claude + codex)"));

    progress(Step::start("Транспорт"));
    for f in [hook_dst(), jarvis_dir().join("run.sock"), shim_dst(), codex_shim_dst(), tmux_conf_dst()] {
        let _ = fs::remove_file(&f);
    }
    let _ = fs::remove_dir(shims_dir());
    for rc in rc_files() {
        if !rc.exists() {
            continue;
        }
        let content = fs::read_to_string(&rc).unwrap_or_default();
        let cleaned = remove_block(&content);
        if cleaned != content {
            backup(&rc);
            atomic_write(&rc, &cleaned);
        }
    }
    progress(Step::done("Транспорт", "шим/конфиг/PATH-блок убраны"));

    let live = live_tmux_sessions();
    if !live.is_empty() {
        progress(Step::info("Транспорт", format!("живые tmux-сессии не тронуты: {}", live.join(", "))));
    }
}

/* ================= модели голоса: учёт места и удаление ================= */

/// Артефакт на диске (модель/окружение) с занятым местом.
#[derive(Debug, Clone, Serialize)]
pub struct Artifact {
    pub id: String,
    pub label: String,
    pub hint: String,
    pub bytes: u64,
}

fn torch_hub_dir() -> PathBuf {
    home().join(".cache/torch/hub")
}

/// Рекурсивный размер каталога в байтах (по файлам, без раздувания на симлинках).
fn dir_size(p: &Path) -> u64 {
    let mut total = 0;
    let Ok(rd) = fs::read_dir(p) else { return 0 };
    for e in rd.flatten() {
        match e.file_type() {
            Ok(ft) if ft.is_dir() => total += dir_size(&e.path()),
            Ok(ft) if ft.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => {}
        }
    }
    total
}

/// Кэш размеров тяжёлых каталогов (venv ~21k файлов, silero/torch-hub) — чтобы НЕ
/// обходить десятки тысяч файлов на КАЖДОМ открытии настроек (был ~1с лаг).
/// TTL 30с: первый расчёт честный, повторные — мгновенно из кэша.
fn dir_size_cached(p: &Path) -> u64 {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<HashMap<PathBuf, (u64, Instant)>>> = Mutex::new(None);
    const TTL: Duration = Duration::from_secs(30);
    let mut g = match CACHE.lock() {
        Ok(g) => g,
        Err(_) => return dir_size(p),
    };
    let map = g.get_or_insert_with(HashMap::new);
    if let Some((sz, at)) = map.get(p) {
        if at.elapsed() < TTL {
            return *sz;
        }
    }
    let sz = dir_size(p);
    map.insert(p.to_path_buf(), (sz, Instant::now()));
    sz
}

/// Голосовые артефакты на диске (что есть — то и показываем).
pub fn model_artifacts() -> Vec<Artifact> {
    let mut v = Vec::new();
    let s = silero_dir();
    if s.exists() {
        v.push(Artifact {
            id: "silero".into(),
            label: "Silero + PyTorch (venv)".into(),
            hint: s.display().to_string(),
            bytes: dir_size(&s),
        });
    }
    let t = torch_hub_dir();
    if t.exists() {
        v.push(Artifact {
            id: "torch-hub".into(),
            label: "Кэш моделей torch".into(),
            hint: t.display().to_string(),
            bytes: dir_size(&t),
        });
    }
    v
}

/* ================= единый инвентарь моделей (раздел «Модели») ================= */

/// Одна модель для раздела «Модели» в настройках. Только filesystem-срез.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    /// Стабильный id (он же ключ движка для STT): "whisper-turbo", "qwen3-0.6b", …
    pub id: String,
    /// Категория: "stt" | "voice" | "wake" | "runtime".
    pub kind: String,
    /// Человекочитаемое имя.
    pub label: String,
    /// Занятое место на диске (0, если не скачана).
    pub bytes: u64,
    /// Скачана/установлена и готова к использованию (по наличию файлов, без health).
    pub present: bool,
    /// Активна сейчас (для STT — текущий движок; для wake — единственная модель).
    pub active: bool,
}

/// Каталог локальных весов Qwen для ключа движка (qwen3-0.6b → …/stt-mlx/models/qwen3-0.6b).
/// Совпадает с тем, что ищет `stt-server.py` (`models/<--model>/config.json`).
pub fn qwen_weights_dir(key: &str) -> PathBuf {
    stt_mlx_dir().join("models").join(key)
}

/// Веса Qwen на месте, если есть `config.json` (тот же признак, что у сайдкара).
pub fn qwen_weights_present(key: &str) -> bool {
    qwen_weights_dir(key).join("config.json").exists()
}

/// Готов ли STT-движок к активации — честный гейт для настроек (источник «правды
/// по модели»). До этого whisper-turbo считался готовым по одному наличию файла,
/// и переключение на него приводило к ошибке распознавания (стаб без нативной сборки).
/// - whisper-turbo: нужна И модель на диске, И вкомпилированная фича `whisper-native`.
/// - qwen3-*: нужны И локальные веса, И установленный MLX-сайдкар (venv + server.py).
pub fn stt_engine_ready(
    engine: &str,
    whisper_model: bool,
    whisper_native_built: bool,
    qwen_weights: bool,
    qwen_sidecar: bool,
) -> bool {
    match engine {
        "whisper-turbo" => whisper_model && whisper_native_built,
        "qwen3-0.6b" | "qwen3-1.7b" => qwen_weights && qwen_sidecar,
        _ => false,
    }
}

/// Полный инвентарь моделей (STT + голос + wake + runtime) для UI.
/// Только filesystem — без сетевых/HTTP-проверок, мгновенный срез.
pub fn model_inventory() -> Vec<ModelInfo> {
    let active_stt = stt_engine();
    let mut v = Vec::new();

    // STT: Whisper (один файл).
    let wmp = whisper_model_path();
    v.push(ModelInfo {
        id: "whisper-turbo".into(),
        kind: "stt".into(),
        label: "Whisper large-v3-turbo (q5)".into(),
        bytes: fs::metadata(&wmp).map(|m| m.len()).unwrap_or(0),
        present: wmp.exists(),
        active: active_stt == "whisper-turbo",
    });

    // STT: Qwen3 веса (локальная папка сайдкара).
    for (key, label) in [
        ("qwen3-0.6b", "Qwen3-ASR 0.6B (8bit)"),
        ("qwen3-1.7b", "Qwen3-ASR 1.7B (4bit)"),
    ] {
        let dir = qwen_weights_dir(key);
        v.push(ModelInfo {
            id: key.into(),
            kind: "stt".into(),
            label: label.into(),
            bytes: if dir.exists() { dir_size(&dir) } else { 0 },
            present: qwen_weights_present(key),
            active: active_stt == key,
        });
    }

    // STT runtime: Qwen MLX-окружение (venv) — показываем, только если установлено.
    let venv = stt_venv();
    if venv.exists() {
        v.push(ModelInfo {
            id: "qwen3-runtime".into(),
            kind: "runtime".into(),
            label: "Qwen3 MLX-окружение (venv)".into(),
            bytes: dir_size_cached(&venv),
            present: stt_python().exists(),
            active: false,
        });
    }

    // Голос: Silero (venv/модель + torch-hub кэш).
    let sd = silero_dir();
    let tor = torch_hub_dir();
    let silero_bytes = (if sd.exists() { dir_size_cached(&sd) } else { 0 })
        + (if tor.exists() { dir_size_cached(&tor) } else { 0 });
    v.push(ModelInfo {
        id: "silero".into(),
        kind: "voice".into(),
        label: "Silero TTS (v4_ru)".into(),
        bytes: silero_bytes,
        present: silero_python().exists() && silero_server_py().exists(),
        active: voice_engine() == "silero",
    });

    // Wake-word: openWakeWord «Hey Jarvis» (3 ONNX).
    let wbytes: u64 = WAKEWORD_MODELS
        .iter()
        .filter_map(|(f, _)| fs::metadata(wakeword_dir().join(f)).ok().map(|m| m.len()))
        .sum();
    v.push(ModelInfo {
        id: "hey_jarvis".into(),
        kind: "wake".into(),
        label: "openWakeWord «Hey Jarvis»".into(),
        bytes: wbytes,
        present: wakeword_models_present(),
        active: true,
    });

    v
}

/// Является ли id ключом STT-движка (whisper/qwen).
fn is_stt_engine_id(id: &str) -> bool {
    matches!(id, "whisper-turbo" | "qwen3-0.6b" | "qwen3-1.7b")
}

/// Удалить модель/артефакт по id и освободить место. Запрещает удаление активного
/// STT-движка (иначе диктовка останется без модели). После удаления соответствующая
/// функция недоступна до повторной загрузки. Идемпотентно (нет файлов — не ошибка).
pub fn delete_model(id: &str) -> Result<(), String> {
    // не сносим активный STT-движок — сначала пользователь должен выбрать другой
    if is_stt_engine_id(id) && id == stt_engine() {
        return Err(format!("{id} — активный движок; сначала выберите другой"));
    }
    let rm_dir = |p: PathBuf| -> Result<(), String> {
        if p.exists() {
            fs::remove_dir_all(&p).map_err(|e| format!("удаление {}: {e}", p.display()))?;
        }
        Ok(())
    };
    match id {
        "whisper-turbo" => {
            let f = whisper_model_path();
            if f.exists() {
                fs::remove_file(&f).map_err(|e| format!("удаление whisper: {e}"))?;
            }
        }
        "qwen3-0.6b" | "qwen3-1.7b" => rm_dir(qwen_weights_dir(id))?,
        "qwen3-runtime" => rm_dir(stt_venv())?,
        "hey_jarvis" => {
            for (name, _) in WAKEWORD_MODELS {
                let _ = fs::remove_file(wakeword_dir().join(name));
            }
        }
        "silero" => rm_dir(silero_dir())?,
        "torch-hub" => rm_dir(torch_hub_dir())?,
        other => return Err(format!("неизвестная модель: {other}")),
    }
    Ok(())
}

/// Сколько в ~/.claude/settings.json ЧУЖИХ хуков (не наших) — их откат сохранит.
pub fn foreign_hook_count() -> usize {
    let Ok((true, json)) = read_settings() else { return 0 };
    let Some(hooks) = json.get("hooks").and_then(Value::as_object) else { return 0 };
    let mut n = 0;
    for arr in hooks.values() {
        let Some(groups) = arr.as_array() else { continue };
        for g in groups {
            if let Some(gh) = g.get("hooks").and_then(Value::as_array) {
                n += gh.iter().filter(|h| !is_ours(h)).count();
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "/Users/test/.jarvis/shims";

    // download_channels: сначала прямой, прокси — фолбэк (если задан); без прокси — только прямой.
    #[test]
    fn download_channels_direct_then_proxy() {
        assert_eq!(super::download_channels(true), vec![true, false]);
        assert_eq!(super::download_channels(false), vec![true]);
    }

    #[test]
    fn mcp_config_has_token_and_command() {
        let cfg = super::build_mcp_config("/x/.jarvis/bin/jarvis-mcp", "feedface");
        assert_eq!(cfg["mcpServers"]["jarvis"]["command"], "/x/.jarvis/bin/jarvis-mcp");
        assert_eq!(cfg["mcpServers"]["jarvis"]["env"]["JARVIS_TOKEN"], "feedface");
    }

    #[test]
    fn merge_into_empty() {
        let merged = merge_block("", DIR);
        assert!(has_block(&merged), "вставка в пустой файл");
        assert!(merged.contains(&format!("export PATH=\"{DIR}:$PATH\"")), "PATH правильный");
    }

    #[test]
    fn merge_preserves_existing_and_is_idempotent() {
        let existing = "# мой zshrc\nexport FOO=bar\n";
        let merged = merge_block(existing, DIR);
        assert!(merged.starts_with(existing), "существующее содержимое не тронуто");
        assert_eq!(merge_block(&merged, DIR), merged, "повторный merge идемпотентен");
    }

    #[test]
    fn merge_replaces_stale_block() {
        let merged = merge_block("export FOO=bar\n", DIR);
        let stale = merged.replace(DIR, "/old/path");
        let refreshed = merge_block(&stale, DIR);
        assert!(refreshed.contains(DIR) && !refreshed.contains("/old/path"), "устаревший блок заменяется");
        assert_eq!(refreshed.matches(BEGIN).count(), 1, "блок ровно один");
    }

    #[test]
    fn remove_block_keeps_foreign_content() {
        let merged = merge_block("# мой zshrc\nexport FOO=bar\n", DIR);
        let removed = remove_block(&merged);
        assert!(!has_block(&removed), "демёрж убирает блок");
        assert!(removed.contains("export FOO=bar"), "демёрж сохраняет чужое");
        assert_eq!(remove_block(&removed), removed, "повторный демёрж идемпотентен");
    }

    #[test]
    fn ours_detection() {
        let ours = json!({ "command": "/Users/x/.jarvis/bin/jarvis-hook claude stop" });
        let foreign = json!({ "command": "/usr/local/bin/other-hook" });
        assert!(is_ours(&ours));
        assert!(!is_ours(&foreign));
    }

    #[test]
    fn embedded_assets_look_sane() {
        assert!(HOOK_SRC.starts_with("#!/bin/sh"));
        assert!(HOOK_SRC.contains("JARVIS_IGNORE"));
        assert!(SHIM_SRC.contains("tmux"));
        assert!(TMUX_CONF_SRC.contains("status off"));
    }

    #[test]
    fn agent_shim_is_generalized() {
        // SHIM_SRC теперь bin/agent-shim: basename-диспатч + bypass-слот.
        assert!(SHIM_SRC.contains("BIN_NAME=$(basename"), "шим выбирает поведение по basename");
        assert!(SHIM_SRC.contains("CODEX_BYPASS"), "есть слот для bypass-hook-trust");
        assert!(SHIM_SRC.contains("command -v \"$BIN_NAME\""), "резолвит реальный бинарь по имени");
    }

    #[test]
    fn codex_events_shape() {
        assert_eq!(CODEX_EVENTS.len(), 8);
        assert!(CODEX_EVENTS.iter().any(|(e, a)| *e == "Stop" && *a == "stop"));
        assert!(CODEX_EVENTS.iter().any(|(e, a)| *e == "PermissionRequest" && *a == "permission"));
        assert!(CODEX_EVENTS.iter().any(|(e, _)| *e == "SubagentStart"));
        // у Codex нет Notification/StopFailure/SessionEnd
        assert!(!CODEX_EVENTS.iter().any(|(e, _)| *e == "Notification"));
    }

    #[test]
    fn codex_hooks_writer_uses_codex_label() {
        let dir = std::env::temp_dir().join(format!("jarvis-codex-hk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("hooks.json");
        let noop = |_s: Step| {};
        install_hooks_into(&path, "codex", &CODEX_EVENTS, &noop);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let stop = v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap().to_string();
        assert!(stop.contains(" codex stop"), "метка codex в команде: {stop}");
        assert!(v["hooks"]["PermissionRequest"].is_array(), "есть PermissionRequest");
        assert!(v["hooks"].get("Notification").is_none(), "у codex нет Notification");
        // идемпотентность: второй проход не дублирует
        install_hooks_into(&path, "codex", &CODEX_EVENTS, &noop);
        let v2: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v2["hooks"]["Stop"].as_array().unwrap().len(), 1, "Stop не дублируется");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Собрать все command-строки события в плоский список (для проверок ниже).
    fn event_commands(v: &Value, event: &str) -> Vec<String> {
        v["hooks"][event]
            .as_array()
            .map(|groups| {
                groups
                    .iter()
                    .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                    .filter_map(|h| h["command"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    // --- Регрессия бага «хуки ничего не отправляют» -------------------------
    // ~/.codex/hooks.json остался написанным от руки со СТАРЫМ путём (prod
    // ~/.jarvis вместо живого JARVIS_DIR=~/.jarvis-dev) и меткой `claude`.
    // is_ours матчит по подстроке "bin/jarvis-hook" (без пути и метки), поэтому
    // event_installed считал событие «уже установленным» и НЕ лечил его —
    // codex дёргал несуществующий бинарь (exit 127) и хуки молчали навсегда.
    #[test]
    fn install_heals_stale_jarvis_hook_path_and_label() {
        let dir = std::env::temp_dir().join(format!("jarvis-heal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        // Похоже на реальный сломанный файл: чужой prod-путь + метка claude.
        let stale = json!({
            "hooks": { "PreToolUse": [{ "hooks": [{
                "type": "command",
                "command": "/Users/somebody/.jarvis/bin/jarvis-hook claude pre-tool",
                "timeout": 5
            }]}]}
        });
        std::fs::write(&path, serde_json::to_string_pretty(&stale).unwrap()).unwrap();

        let noop = |_s: Step| {};
        install_hooks_into(&path, "codex", &CODEX_EVENTS, &noop);

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let want = format!("{} codex pre-tool", hook_dst().display());
        let cmds = event_commands(&v, "PreToolUse");
        assert!(cmds.contains(&want), "вылечено на верный путь+метку codex: {cmds:?}");
        assert!(
            !cmds.iter().any(|c| c.contains("claude pre-tool")),
            "старый claude-хук убран: {cmds:?}"
        );
        assert!(
            !cmds.iter().any(|c| c.contains("/Users/somebody/.jarvis")),
            "старый prod-путь убран: {cmds:?}"
        );
        assert_eq!(
            cmds.iter().filter(|c| c.contains(MARKER)).count(),
            1,
            "ровно один наш хук, без дублей: {cmds:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Лечение НЕ должно сносить чужие хуки в том же событии.
    #[test]
    fn heal_preserves_foreign_hooks() {
        let dir = std::env::temp_dir().join(format!("jarvis-heal-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        let mixed = json!({
            "hooks": { "PreToolUse": [{ "hooks": [
                { "type": "command", "command": "/opt/other/guard pre", "timeout": 5 },
                { "type": "command", "command": "/Users/x/.jarvis/bin/jarvis-hook claude pre-tool" }
            ]}]}
        });
        std::fs::write(&path, serde_json::to_string_pretty(&mixed).unwrap()).unwrap();

        let noop = |_s: Step| {};
        install_hooks_into(&path, "codex", &CODEX_EVENTS, &noop);

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let cmds = event_commands(&v, "PreToolUse");
        assert!(cmds.iter().any(|c| c == "/opt/other/guard pre"), "чужой хук сохранён: {cmds:?}");
        assert!(cmds.contains(&format!("{} codex pre-tool", hook_dst().display())), "наш вылечен: {cmds:?}");
        assert!(!cmds.iter().any(|c| c.contains("claude pre-tool")), "старый снят: {cmds:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Строгий детектор дрейфа: стейл-файл (старый путь/метка) НЕ «ок»; после
    // лечения — «ок». Это источник правды для health-самопроверки на старте.
    #[test]
    fn hooks_all_correct_detects_stale_then_healed() {
        let dir = std::env::temp_dir().join(format!("jarvis-allok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        let stale = json!({
            "hooks": { "PreToolUse": [{ "hooks": [{
                "type": "command",
                "command": "/Users/x/.jarvis/bin/jarvis-hook claude pre-tool"
            }]}]}
        });
        std::fs::write(&path, serde_json::to_string_pretty(&stale).unwrap()).unwrap();
        assert!(!hooks_all_correct(&path, "codex", &CODEX_EVENTS), "стейл-файл не «ок»");

        let noop = |_s: Step| {};
        install_hooks_into(&path, "codex", &CODEX_EVENTS, &noop);
        assert!(hooks_all_correct(&path, "codex", &CODEX_EVENTS), "после лечения — все события ок");

        // отсутствующий файл — тоже не «ок»
        assert!(!hooks_all_correct(&dir.join("nope.json"), "codex", &CODEX_EVENTS));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Когда всё уже верно — повторный проход НЕ переписывает файл (иначе бэкап и
    // запись на каждом старте демона). Проверяем по байтовой идентичности.
    #[test]
    fn install_no_rewrite_when_already_correct() {
        let dir = std::env::temp_dir().join(format!("jarvis-idem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        let noop = |_s: Step| {};
        install_hooks_into(&path, "codex", &CODEX_EVENTS, &noop); // первичная установка
        let first = std::fs::read(&path).unwrap();
        install_hooks_into(&path, "codex", &CODEX_EVENTS, &noop); // повтор — должен быть no-op
        let second = std::fs::read(&path).unwrap();
        assert_eq!(first, second, "повторный проход не меняет уже верный файл");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Phase 8: STT-SERVER_SRC встроен и является валидным Python-скриптом (начало).
    #[test]
    fn stt_server_src_embedded() {
        assert!(!STT_SERVER_SRC.is_empty(), "stt-server.py должен быть встроен");
        assert!(STT_SERVER_SRC.contains("transcribe"), "stt-server.py содержит /transcribe");
        assert!(STT_SERVER_SRC.contains("uvicorn") || STT_SERVER_SRC.contains("fastapi"),
            "stt-server.py содержит uvicorn или fastapi");
    }

    // Phase 8: URL Whisper-модели соответствует ожидаемому.
    #[test]
    fn whisper_model_url_is_correct() {
        assert!(WHISPER_MODEL_URL.contains("huggingface.co"), "URL на HuggingFace");
        assert!(WHISPER_MODEL_URL.contains("ggml-large-v3-turbo-q5_0.bin"), "имя модели в URL");
    }

    // Phase 8: пути STT изолированы от silero (не пересекаются).
    #[test]
    fn stt_paths_are_separate_from_silero() {
        // При JARVIS_DIR не заданном — дефолт ~/.jarvis
        // stt_dir() = ~/.jarvis/stt, silero_dir() = ~/.jarvis/silero — разные каталоги.
        let stt = stt_dir();
        let silero = silero_dir();
        assert_ne!(stt, silero, "stt и silero — разные каталоги");
        assert!(stt.ends_with("stt"), "stt_dir заканчивается на 'stt'");
        assert!(silero.ends_with("silero"), "silero_dir заканчивается на 'silero'");
    }

    // Phase 8: whisper_model_path() = stt_dir() / ggml-large-v3-turbo-q5_0.bin
    #[test]
    fn whisper_model_path_is_inside_stt_dir() {
        let model = whisper_model_path();
        assert!(model.starts_with(stt_dir()), "модель внутри stt_dir()");
        assert_eq!(
            model.file_name().unwrap().to_str().unwrap(),
            "ggml-large-v3-turbo-q5_0.bin"
        );
    }

    // Phase 8: stt_server_py() = stt_mlx_dir() / stt-server.py
    #[test]
    fn stt_server_py_path_is_inside_stt_mlx_dir() {
        let server = stt_server_py();
        assert!(server.starts_with(stt_mlx_dir()), "stt-server.py внутри stt_mlx_dir()");
        assert_eq!(server.file_name().unwrap().to_str().unwrap(), "stt-server.py");
    }

    // Phase 8: status() возвращает корректные дефолты когда ничего не установлено.
    #[test]
    fn status_stt_fields_default_false() {
        // Используем изолированный JARVIS_DIR через переменную окружения.
        // В CI / чистом рабочем дереве модели точно нет.
        // Тест только проверяет типы и что поля существуют — не делает filesystem calls.
        let s = Status::default();
        assert!(!s.whisper_model, "по умолчанию whisper_model=false");
        assert!(!s.qwen3_sidecar, "по умолчанию qwen3_sidecar=false");
        assert_eq!(s.stt_engine_active, "", "по умолчанию stt_engine_active=empty");
    }

    // Phase 8: idempotency — install_stt_sidecar записывает stt-server.py атомарно
    // (мы не тестируем реальный venv — только что server.py = STT_SERVER_SRC).
    #[test]
    fn stt_server_py_content_matches_embedded() {
        // Если файл уже создан (в другом тесте или на диске), его содержимое
        // должно совпадать с встроенной константой. Тест атомарной записи:
        let tmp = std::env::temp_dir().join("jarvis-test-stt-server.py");
        atomic_write(&tmp, STT_SERVER_SRC);
        let content = fs::read_to_string(&tmp).unwrap();
        assert_eq!(content, STT_SERVER_SRC, "атомарная запись не изменяет содержимое");
        let _ = fs::remove_file(&tmp);
    }

    // --- Инкр. «Модели»: единый инвентарь ---

    #[test]
    fn model_inventory_has_core_models() {
        let inv = model_inventory();
        let ids: Vec<&str> = inv.iter().map(|m| m.id.as_str()).collect();
        for id in ["whisper-turbo", "qwen3-0.6b", "qwen3-1.7b", "silero", "hey_jarvis"] {
            assert!(ids.contains(&id), "инвентарь должен содержать {id}");
        }
    }

    #[test]
    fn model_inventory_kinds_correct() {
        let inv = model_inventory();
        let kind_of = |id: &str| inv.iter().find(|m| m.id == id).map(|m| m.kind.clone());
        assert_eq!(kind_of("whisper-turbo").as_deref(), Some("stt"));
        assert_eq!(kind_of("qwen3-0.6b").as_deref(), Some("stt"));
        assert_eq!(kind_of("qwen3-1.7b").as_deref(), Some("stt"));
        assert_eq!(kind_of("silero").as_deref(), Some("voice"));
        assert_eq!(kind_of("hey_jarvis").as_deref(), Some("wake"));
    }

    #[test]
    fn model_inventory_at_most_one_active_stt() {
        let inv = model_inventory();
        let active = inv.iter().filter(|m| m.kind == "stt" && m.active).count();
        assert!(active <= 1, "активным может быть не более одного STT-движка, найдено {active}");
    }

    #[test]
    fn qwen_weights_dir_layout() {
        let d = qwen_weights_dir("qwen3-0.6b");
        assert!(d.ends_with("qwen3-0.6b"), "каталог заканчивается на ключ модели");
        let s = d.to_string_lossy();
        assert!(s.contains("stt-mlx") && s.contains("models"), "путь внутри stt-mlx/models: {s}");
    }

    // --- Инкр.2: гибридная загрузка (теперь: прокси-на-всё + фолбэк, см.
    // download_channels_proxy_then_direct выше) ---

    #[test]
    fn qwen_repo_mapping() {
        assert_eq!(qwen_repo("qwen3-0.6b"), Some("mlx-community/Qwen3-ASR-0.6B-8bit"));
        assert_eq!(qwen_repo("qwen3-1.7b"), Some("mlx-community/Qwen3-ASR-1.7B-4bit"));
        assert_eq!(qwen_repo("whisper-turbo"), None);
        assert_eq!(qwen_repo("nonsense"), None);
    }

    // Сетевой смоук: реально дёргает HF tree + качает мелкий файл гибридом.
    // Не герметичен (нужны сеть/прокси) → #[ignore]; запуск вручную:
    //   HTTP_PROXY=… cargo test --bin jarvis smoke_hf_hybrid -- --ignored --nocapture
    #[test]
    #[ignore]
    fn smoke_hf_hybrid() {
        let proxy = std::env::var("HTTP_PROXY").ok();
        let files = hf_tree("mlx-community/Qwen3-ASR-0.6B-8bit", proxy.as_deref())
            .expect("hf_tree должен вернуть список файлов");
        assert!(files.iter().any(|(p, _)| p == "config.json"), "в дереве есть config.json");
        assert!(files.iter().any(|(p, _)| p == "model.safetensors"), "в дереве есть веса");
        let tmp = std::env::temp_dir().join("jarvis-smoke-config.json");
        let _ = fs::remove_file(&tmp);
        fetch_to_file(
            "https://huggingface.co/mlx-community/Qwen3-ASR-0.6B-8bit/resolve/main/config.json",
            &tmp,
            proxy.as_deref(),
            &|_| {},
            "smoke",
            None,
        )
        .expect("fetch config.json через прокси");
        assert!(fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0) > 1000, "config.json скачан");
        let _ = fs::remove_file(&tmp);
    }

    // --- Инкр.3: удаление ---

    #[test]
    fn delete_unknown_model_errs() {
        assert!(delete_model("nonsense").is_err(), "неизвестный id → Err");
    }

    #[test]
    fn delete_active_stt_engine_is_blocked() {
        // активный движок (из settings, дефолт qwen3-0.6b) удалять нельзя
        let active = stt_engine();
        if is_stt_engine_id(&active) {
            let r = delete_model(&active);
            assert!(r.is_err(), "активный движок {active} нельзя удалить");
            assert!(r.unwrap_err().contains("активный"), "ошибка про активность");
        }
    }

    #[test]
    fn is_stt_engine_id_classifies() {
        assert!(is_stt_engine_id("whisper-turbo"));
        assert!(is_stt_engine_id("qwen3-0.6b"));
        assert!(is_stt_engine_id("qwen3-1.7b"));
        assert!(!is_stt_engine_id("silero"));
        assert!(!is_stt_engine_id("hey_jarvis"));
        assert!(!is_stt_engine_id("qwen3-runtime"));
    }

    // --- Step::progress несёт pct: UI (settings2.js) читает step.pct и рисует полосу ---
    #[test]
    fn step_progress_carries_pct_as_number() {
        let s = Step::progress("Модель", "qwen3-1.7b — 42%", 42);
        assert_eq!(s.pct, Some(42));
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["pct"], 42, "pct сериализуется числом — иначе прогрессбар не появится");
    }

    #[test]
    fn step_info_omits_pct_field() {
        let s = Step::info("X", "msg");
        assert_eq!(s.pct, None);
        let j = serde_json::to_value(&s).unwrap();
        assert!(j.get("pct").is_none(), "без прогресса поле pct не сериализуется");
    }

    // --- stt_engine_ready: честный гейт. Главный баг «правды по модели»: whisper
    // показывался готовым по наличию файла, но без нативной сборки стаб падал. ---
    #[test]
    fn whisper_ready_requires_native_build_and_model() {
        // модель есть, но фича не вкомпилирована → НЕ готов (иначе стаб = ошибка распознавания)
        assert!(!stt_engine_ready("whisper-turbo", true, false, false, false));
        // модель есть и фича вкомпилирована → готов
        assert!(stt_engine_ready("whisper-turbo", true, true, false, false));
        // фича есть, но модель не скачана → не готов
        assert!(!stt_engine_ready("whisper-turbo", false, true, false, false));
    }

    #[test]
    fn qwen_ready_requires_weights_and_sidecar() {
        assert!(stt_engine_ready("qwen3-1.7b", false, false, true, true));
        assert!(!stt_engine_ready("qwen3-1.7b", false, false, false, true), "нет весов → не готов");
        assert!(!stt_engine_ready("qwen3-0.6b", false, false, true, false), "нет сайдкара → не готов");
    }

    #[test]
    fn unknown_engine_never_ready() {
        assert!(!stt_engine_ready("banana", true, true, true, true));
    }
}
