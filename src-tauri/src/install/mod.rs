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
const SHIM_SRC: &str = include_str!("../../../bin/claude-shim");
const TMUX_CONF_SRC: &str = include_str!("../../../bin/jarvis-tmux.conf");
const SILERO_SERVER_SRC: &str = include_str!("../../../bin/silero-server.py");

/// Признак «это наша запись» — путь шима в команде.
const MARKER: &str = ".jarvis/bin/jarvis-hook";

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
}

impl Step {
    fn new(phase: &str, state: StepState, msg: impl Into<String>) -> Step {
        Step { phase: phase.into(), state, msg: msg.into() }
    }
    fn start(phase: &str) -> Step { Step::new(phase, StepState::Start, "") }
    fn done(phase: &str, msg: impl Into<String>) -> Step { Step::new(phase, StepState::Done, msg) }
    fn warn(phase: &str, msg: impl Into<String>) -> Step { Step::new(phase, StepState::Warn, msg) }
    fn info(phase: &str, msg: impl Into<String>) -> Step { Step::new(phase, StepState::Info, msg) }
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
fn hook_dst() -> PathBuf { jarvis_dir().join("bin/jarvis-hook") }
fn shims_dir() -> PathBuf { jarvis_dir().join("shims") }
fn shim_dst() -> PathBuf { shims_dir().join("claude") }
fn tmux_conf_dst() -> PathBuf { jarvis_dir().join("tmux.conf") }
fn settings_path() -> PathBuf { home().join(".claude/settings.json") }
fn jarvis_settings_path() -> PathBuf { jarvis_dir().join("settings.json") }

/* ================= Silero: Python-sidecar (venv + torch + модель) ================= */

fn silero_dir() -> PathBuf { jarvis_dir().join("silero") }
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

/// Установить Silero-сайдкар: server.py + venv с torch(CPU)/fastapi/uvicorn/numpy
/// + прогрев модели. Веса PyTorch — сотни МБ–ГБ, ставятся один раз. Идемпотентно.
/// `progress` зовётся для долгих под-шагов (чтобы UI не выглядел зависшим).
fn install_silero(progress: &Progress) -> Result<(), String> {
    fs::create_dir_all(silero_dir()).map_err(|e| format!("mkdir silero: {e}"))?;

    // 1. server.py — атомарная запись поверх (держим актуальным).
    atomic_write(&silero_server_py(), SILERO_SERVER_SRC);

    // 2. venv — если ещё нет интерпретатора.
    if !silero_python().exists() {
        progress(Step::info("Голос", "создаю Python-venv…"));
        run_inherit(
            "python3 -m venv",
            Command::new("python3").arg("-m").arg("venv").arg(silero_venv()),
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
        progress(Step::info("Голос", "ставлю PyTorch CPU в venv — сотни МБ–ГБ, это надолго…"));
        run_inherit(
            "pip install --upgrade pip",
            Command::new(silero_pip()).args(["install", "--upgrade", "pip"]),
        )?;
        run_inherit(
            "pip install torch+deps",
            Command::new(silero_pip()).args([
                "install", "torch", "fastapi", "uvicorn", "numpy", "certifi", "omegaconf",
            ]),
        )?;
    }

    // 4. Прогрев модели в torch-hub кэш. SSL_CERT_FILE из certifi — иначе
    //    torch.hub падает на верификации HTTPS.
    progress(Step::info("Голос", "прогрев модели Silero…"));
    let ca = Command::new(silero_python())
        .args(["-c", "import certifi; print(certifi.where())"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    run_inherit(
        "прогрев модели Silero",
        Command::new(silero_python())
            .env("SSL_CERT_FILE", &ca)
            .args([
                "-c",
                "import torch; torch.hub.load('snakers4/silero-models','silero_tts',language='ru',speaker='v4_ru',trust_repo=True)",
            ]),
    )?;

    Ok(())
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

/// Прочитать ~/.claude/settings.json. (есть_ли_файл, json).
/// Битый/нечитаемый файл → Err (НЕ выходим из процесса — зовётся в демоне).
fn read_settings() -> Result<(bool, Value), String> {
    let path = settings_path();
    if !path.exists() {
        return Ok((false, json!({})));
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("не смог прочитать {}: {e}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok((true, json!({})));
    }
    serde_json::from_str(&raw)
        .map(|v| (true, v))
        .map_err(|_| format!("{} содержит невалидный JSON — не трогаю", path.display()))
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

fn event_installed(json: &Value, event: &str) -> bool {
    json.pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(group_has_ours))
}

fn claude_found() -> bool {
    Command::new("/bin/sh")
        .args(["-c", "command -v claude"])
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tmux_found() -> bool {
    Command::new("tmux")
        .arg("-V")
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
    out
}

/// Установить интеграцию. Шлёт шаги в `progress`. Каждая фаза fail-safe —
/// сбой одной (например, голоса) не валит остальные и не паникует.
pub fn install(progress: &Progress) {
    // --- Фаза «Хуки» ---
    progress(Step::start("Хуки"));
    write_executable(&hook_dst(), HOOK_SRC);
    if !claude_found() {
        progress(Step::info("Хуки", "claude не найден в PATH — хуки подхватятся, когда появится"));
    }
    match read_settings() {
        Ok((exists, mut json)) => {
            if exists {
                backup(&settings_path());
            }
            let _ = fs::create_dir_all(home().join(".claude"));
            if !json.is_object() {
                json = json!({});
            }
            if json.get("hooks").map(|h| !h.is_object()).unwrap_or(true) {
                json["hooks"] = json!({});
            }
            let mut added = Vec::new();
            for (event, arg) in EVENTS {
                if event_installed(&json, event) {
                    continue;
                }
                let hooks = json["hooks"].as_object_mut().unwrap();
                let arr = hooks.entry(event).or_insert_with(|| json!([]));
                if !arr.is_array() {
                    *arr = json!([]);
                }
                arr.as_array_mut().unwrap().push(json!({
                    "hooks": [{
                        "type": "command",
                        "command": format!("{} claude {arg}", hook_dst().display()),
                        "timeout": 5,
                    }],
                }));
                added.push(event);
            }
            if !added.is_empty() {
                atomic_write(&settings_path(), &(serde_json::to_string_pretty(&json).unwrap() + "\n"));
                progress(Step::done("Хуки", format!("добавлены: {}", added.join(", "))));
            } else {
                progress(Step::done("Хуки", "уже установлены"));
            }
        }
        Err(e) => progress(Step::warn("Хуки", format!("{e} — пропускаю хуки"))),
    }

    // --- Фаза «Транспорт» (шим claude + tmux.conf + PATH-блок) ---
    progress(Step::start("Транспорт"));
    install_tmux_transport(progress);

    // --- Фаза «Голос» (Silero) — не-фатально ---
    progress(Step::start("Голос"));
    match install_silero(progress) {
        Ok(()) => progress(Step::done("Голос", "Silero установлен (venv + модель)")),
        Err(e) => progress(Step::warn("Голос", format!("Silero не установлен ({e}); демон не затронут"))),
    }
}

fn install_tmux_transport(progress: &Progress) {
    if !tmux_found() {
        progress(Step::warn("Транспорт", "tmux не найден (brew install tmux) — ввод-транспорт пропущен; уведомления работают"));
        return;
    }
    write_executable(&shim_dst(), SHIM_SRC);
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
    progress(Step::done("Транспорт", "шим claude + tmux.conf + PATH-блок"));
}

/// Снять интеграцию. Шлёт шаги в `progress`.
pub fn uninstall(progress: &Progress) {
    progress(Step::start("Хуки"));
    match read_settings() {
        Ok((true, mut json)) if json.get("hooks").and_then(Value::as_object).is_some() => {
            backup(&settings_path());
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
            atomic_write(&settings_path(), &(serde_json::to_string_pretty(&json).unwrap() + "\n"));
            progress(Step::done("Хуки", if removed.is_empty() { "наших хуков не нашлось".into() } else { format!("удалены: {}", removed.join(", ")) }));
        }
        Ok(_) => progress(Step::done("Хуки", "записей Jarvis нет")),
        Err(e) => progress(Step::warn("Хуки", e)),
    }

    progress(Step::start("Транспорт"));
    for f in [hook_dst(), jarvis_dir().join("run.sock"), shim_dst(), tmux_conf_dst()] {
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

/// Удалить голосовой артефакт по id. После удаления голос недоступен до переустановки.
pub fn delete_model(id: &str) -> Result<(), String> {
    let path = match id {
        "silero" => silero_dir(),
        "torch-hub" => torch_hub_dir(),
        other => return Err(format!("неизвестная модель: {other}")),
    };
    if path.exists() {
        fs::remove_dir_all(&path).map_err(|e| format!("удаление {}: {e}", path.display()))?;
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
}
