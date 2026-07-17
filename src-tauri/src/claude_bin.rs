//! Запуск настоящего бинаря `claude` для служебных нужд демона
//! (haiku-переводы, саммари, официальный /usage, effort-уровни).
//!
//! Все вызовы идут с JARVIS_IGNORE=1 — шим-хук видит переменную и не шлёт
//! события, иначе служебные запуски засоряли бы реестр сессий.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::RwLock;
use std::time::Duration;

use crate::util::jarvis_dir;

/// Настоящий claude в PATH (плюс типовые каталоги), минуя наш tmux-шим.
pub fn resolve_claude_bin() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    for extra in [
        crate::util::home_dir().join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    let shims = jarvis_dir().join("shims");
    for d in dirs {
        if d == shims {
            continue; // настоящий бинарь, не наш шим
        }
        let p = d.join("claude");
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

/// `claude <args>` с таймаутом; stdout при нулевом коде выхода.
/// Ошибка/таймаут → None: без сети и квоты демон живёт на локальных данных.
pub async fn run_claude(args: &[&str], timeout: Duration) -> Option<String> {
    let bin = resolve_claude_bin()?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .current_dir(std::env::temp_dir())
        .env("JARVIS_IGNORE", "1")
        // ВАЖНО: прокси НЕ убирать. Прямой заход на api.anthropic.com с этой
        // сети режется на эдже (403 «Request not allowed»); HTTP(S)_PROXY —
        // обязательная точка egress, без неё haiku всегда падает в фолбэк.
        //
        // Пропускаем user-настройки (--setting-sources ниже), поэтому полезный
        // perf-флаг возвращаем как настоящую env-переменную (читается отдельно
        // от settings): не делать необязательных служебных модельных вызовов.
        .env("DISABLE_NON_ESSENTIAL_MODEL_CALLS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    apply_claude_auth(&mut cmd); // подключённый аккаунт Claude (ключ/подписка), если есть
    apply_proxy(&mut cmd); // egress-прокси из настроек (или env), HTTP+HTTPS
    let child = cmd.output();
    let out = tokio::time::timeout(timeout, child).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Системный промпт служебных haiku-вызовов: `claude -p` — это полноценный
/// агент (с окружением, cwd, git), и на наши запросы он порой отвечает «по-
/// агентски» по-английски («I'm in a temporary directory…»). Здесь жёстко
/// переводим его в режим чистой текст-функции с ответом только на русском.
const HAIKU_SYSTEM: &str = "Ты — функция обработки текста, а не ассистент и не агент. \
Выполни ровно то, что сказано в сообщении пользователя, и верни ТОЛЬКО готовый результат на русском языке. \
Запрещено: задавать вопросы, просить уточнений, здороваться, добавлять пояснения и преамбулы, \
упоминать рабочую папку, git, репозиторий, проект, контекст или их отсутствие, использовать английский язык. \
Если входных данных мало — всё равно дай максимально короткий разумный ответ строго по присланному тексту.";

fn service_request_metadata(backend: &str, prompt: &str) -> String {
    format!(
        "[{backend}] request prompt_chars={}",
        prompt.chars().count()
    )
}

fn service_response_metadata(backend: &str, response: Option<&str>) -> String {
    match response {
        Some(text) => format!(
            "[{backend}] response status=ok response_chars={}",
            text.chars().count()
        ),
        None => format!("[{backend}] response status=unavailable response_chars=0"),
    }
}

/// Headless-вызов haiku одним промптом — общий путь переводов и саммари.
pub async fn run_haiku(prompt: &str, timeout: Duration) -> Option<String> {
    crate::log::line(&service_request_metadata("haiku", prompt));
    let out = run_claude(
        &[
            "-p",
            "--no-session-persistence",
            // Служебному вызову не нужны ни MCP, ни плагины, ни скилы, ни хуки —
            // а `claude -p` иначе грузит всё это на КАЖДЫЙ вызов (boot CLI и есть
            // главный оверхед, 11–20с). Срезаем:
            //  • --strict-mcp-config        — ноль MCP-серверов;
            //  • --disable-slash-commands   — отключить все скилы;
            //  • --setting-sources project,local — пропустить user-настройки,
            //    где лежит огромный enabledPlugins и хуки (в temp-папке демона
            //    нет project/local → не грузится ничего лишнего).
            // Auth (OAuth/keychain) читается независимо от sources — НЕ ломается
            // (в отличие от --bare, который keychain не читает).
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--setting-sources",
            "project,local",
            "--append-system-prompt",
            HAIKU_SYSTEM,
            "--model",
            "haiku",
            prompt,
        ],
        timeout,
    )
    .await;
    crate::log::line(&service_response_metadata("haiku", out.as_deref()));
    out
}

/// Доступен ли ХОТЬ КАКОЙ-ТО служебный бэкенд (claude или codex) — для гейтов
/// саммари/переводов: на codex-only машине они тоже должны работать.
pub fn any_service_bin() -> bool {
    resolve_claude_bin().is_some() || crate::backend::codex::resolve_codex_bin().is_some()
}

/// Codex как «функция текста»: `codex exec --json --ignore-user-config` (без
/// чужих MCP), read-only, дешёвый reasoning; system-промпт вшит в начало (у Codex
/// нет --append-system-prompt). Возвращает последний agent_message из потока.
pub async fn run_codex_summary(prompt: &str, timeout: Duration) -> Option<String> {
    let bin = crate::backend::codex::resolve_codex_bin()?;
    let full = format!("{HAIKU_SYSTEM}\n\n{prompt}");
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args([
        "exec",
        "--json",
        "--ephemeral", // НЕ писать rollout — служебные вызовы не засоряют history/usage
        "--ignore-user-config", // ноль чужих MCP/скиллов
        "-s",
        "read-only",
        "-c",
        "model_reasoning_effort=\"low\"", // не minimal: 400 при image_gen/web_search
        &full,
    ])
    .current_dir(std::env::temp_dir())
    .env("JARVIS_IGNORE", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
    apply_proxy(&mut cmd); // Codex → OpenAI по HTTPS: без HTTPS_PROXY висит в таймаут
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut last: Option<String> = None;
    for line in text.lines() {
        if let crate::backend::codex_agent::CodexLine::Events(evs) =
            crate::backend::codex_agent::classify_codex_line(line)
        {
            for ev in evs {
                if let crate::agent::AgentEvent::Delta { text } = ev {
                    if !text.trim().is_empty() {
                        last = Some(text);
                    }
                }
            }
        }
    }
    last
}

/// Выбор бэкенда служебного LLM из настроек (раздел «Под капотом»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceBackend {
    /// Авто: Claude (haiku) → Codex-SDK → codex exec. Историческое поведение.
    Auto,
    /// Принудительно Claude (с фолбэком на Codex, чтобы саммари не пропадали).
    Claude,
    /// Принудительно Codex (SDK → exec, с фолбэком на Claude).
    Codex,
}

impl ServiceBackend {
    pub fn from_str(s: &str) -> ServiceBackend {
        match s {
            "claude" => ServiceBackend::Claude,
            "codex" => ServiceBackend::Codex,
            _ => ServiceBackend::Auto,
        }
    }
}

/// Конкретный исполнитель служебного вызова (после фильтра по доступности).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// claude -p --model haiku
    Claude,
    /// Python Codex-SDK сайдкар (codex-summary.py) — выбранная модель/effort
    CodexSdk,
    /// codex exec --json — лёгкий фолбэк, если SDK-сайдкар не установлен
    CodexExec,
}

/// Приоритетный список исполнителей под выбранный бэкенд, отфильтрованный по
/// доступности. Пустой → ни одного бэкенда нет (саммари выключены гейтом).
/// Инвариант: при любом доступном бинаре список НЕ пуст (саммари не пропадают).
pub fn service_order(
    backend: ServiceBackend,
    claude_avail: bool,
    codex_sdk_avail: bool,
    codex_exec_avail: bool,
) -> Vec<Backend> {
    let claude = [Backend::Claude];
    let codex = [Backend::CodexSdk, Backend::CodexExec];
    // Базовый порядок предпочтения по выбору пользователя; фолбэк — остальное.
    let prefs: Vec<Backend> = match backend {
        ServiceBackend::Auto | ServiceBackend::Claude => claude.into_iter().chain(codex).collect(),
        ServiceBackend::Codex => codex.into_iter().chain(claude).collect(),
    };
    prefs
        .into_iter()
        .filter(|b| match b {
            Backend::Claude => claude_avail,
            Backend::CodexSdk => codex_sdk_avail,
            Backend::CodexExec => codex_exec_avail,
        })
        .collect()
}

/// Служебный LLM-вызов (саммари/перевод/диктовка/голос-план), бэкенд-агностично.
/// Порядок исполнителей берётся из настроек («Под капотом»): Claude-haiku или
/// Codex (Python-SDK сайдкар → codex exec), с фолбэком, чтобы вызовы не пропадали.
pub async fn run_service_llm(prompt: &str, timeout: Duration) -> Option<String> {
    let cfg = service_config();
    let order = service_order(
        cfg.backend,
        resolve_claude_bin().is_some(),
        cfg.codex_sdk_ready(),
        crate::backend::codex::resolve_codex_bin().is_some(),
    );
    for backend in order {
        let out = match backend {
            Backend::Claude => run_haiku(prompt, timeout).await,
            Backend::CodexSdk => {
                run_codex_sdk(prompt, &cfg.codex_model, &cfg.codex_effort, timeout).await
            }
            Backend::CodexExec => run_codex_summary(prompt, timeout).await,
        };
        if out.is_some() {
            return out;
        }
    }
    None
}

/* ================= конфиг служебного LLM (раздел «Под капотом») ================= */

/// Каталог Python-сайдкара Codex-SDK (venv + codex-summary.py) — рядом со
/// stt-mlx, изоляция dev-сборки через JARVIS_DIR.
fn codex_sdk_dir() -> PathBuf {
    jarvis_dir().join("codex-sdk")
}
/// Python из venv сайдкара (тот, куда поставлен `openai-codex`).
pub fn codex_sdk_python() -> PathBuf {
    codex_sdk_dir().join("venv/bin/python")
}
/// Скрипт-обёртка одношагового вызова Codex-SDK.
pub fn codex_sdk_script() -> PathBuf {
    codex_sdk_dir().join("codex-summary.py")
}

/// Чистый CODEX_HOME для сайдкара: только auth (симлинк на ~/.codex/auth.json) +
/// минимальный config. БЕЗ чужих MCP и ~142 пользовательских скиллов — они грузятся
/// 20+ секунд на КАЖДЫЙ холодный старт app-server. С чистым home холодный старт
/// падает с ~30с до ~11с → вписывается в таймауты, codex перестаёт молча уходить
/// в фолбэк на haiku. Симлинк на auth всегда отражает живой `codex login`.
fn codex_sdk_home() -> PathBuf {
    codex_sdk_dir().join("home")
}
fn ensure_codex_clean_home() -> PathBuf {
    let home = codex_sdk_home();
    let _ = std::fs::create_dir_all(&home);
    let real_auth = crate::util::home_dir().join(".codex/auth.json");
    let auth_link = home.join("auth.json");
    if real_auth.exists() {
        let cur = std::fs::read_link(&auth_link).ok();
        if cur.as_deref() != Some(real_auth.as_path()) {
            let _ = std::fs::remove_file(&auth_link);
            let _ = std::os::unix::fs::symlink(&real_auth, &auth_link);
        }
    }
    let cfg = home.join("config.toml");
    if !cfg.exists() {
        let _ = std::fs::write(&cfg, "model = \"gpt-5.5\"\n");
    }
    home
}

/// Текущий выбор бэкенда служебного LLM + параметры Codex. Демон обновляет его из
/// настроек на старте и при изменении (через `set_service_config`), чтобы свободные
/// функции `run_service_llm` не таскали настройки через все места вызова.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub backend: ServiceBackend,
    /// Модель Codex для служебных вызовов (пусто → дефолт SDK/конфига).
    pub codex_model: String,
    /// Reasoning effort Codex: minimal|low|medium|high|xhigh (по умолчанию low).
    pub codex_effort: String,
    /// Подключённый аккаунт Claude: ""|"key"|"subscription". Определяет, какую
    /// переменную окружения впрыснуть в `claude` (ANTHROPIC_API_KEY vs
    /// CLAUDE_CODE_OAUTH_TOKEN). Пусто → используется собственный логин CLI.
    pub claude_auth_mode: String,
    /// Секрет аккаунта Claude (API-ключ sk-ant-api… или OAuth-токен подписки).
    pub claude_secret: String,
    /// Egress-прокси для служебных вызовов (HTTP_PROXY+HTTPS_PROXY). Пусто →
    /// наследуется из env процесса. Codex ходит к OpenAI по HTTPS, и без явного
    /// HTTPS_PROXY на этой сети его запрос висит в таймаут — поэтому прокси можно
    /// задать отдельно в настройках («Под капотом»).
    pub proxy: String,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        ServiceConfig {
            backend: ServiceBackend::Auto,
            codex_model: String::new(),
            codex_effort: "low".into(),
            claude_auth_mode: String::new(),
            claude_secret: String::new(),
            proxy: String::new(),
        }
    }
}

impl ServiceConfig {
    /// SDK-сайдкар готов к запуску: venv-python и скрипт на месте.
    pub fn codex_sdk_ready(&self) -> bool {
        codex_sdk_python().exists() && codex_sdk_script().exists()
    }

    /// Собрать из блока `service` настроек (~/.jarvis/settings.json).
    pub fn from_settings(all: &serde_json::Value) -> ServiceConfig {
        let s = all.get("service");
        let g = |k: &str| {
            s.and_then(|s| s.get(k))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        };
        let effort = g("codexEffort");
        ServiceConfig {
            backend: ServiceBackend::from_str(g("backend")),
            codex_model: g("codexModel").to_string(),
            codex_effort: if effort.is_empty() {
                "low".into()
            } else {
                effort.into()
            },
            claude_auth_mode: g("claudeAuthMode").to_string(),
            claude_secret: g("claudeSecret").to_string(),
            proxy: g("proxy").to_string(),
        }
    }
}

/// Впрыснуть подключённую учётку Claude (из настроек) в команду `claude`. По
/// исследованию Anthropic: API-ключ → ANTHROPIC_API_KEY (работает везде); OAuth-
/// токен подписки (`claude setup-token`) → CLAUDE_CODE_OAUTH_TOKEN (только через
/// CLI). Никогда не ставим обе сразу — API-ключ перебивает токен по приоритету,
/// поэтому вторую переменную явно снимаем.
pub fn apply_claude_auth(cmd: &mut tokio::process::Command) {
    let cfg = service_config();
    match cfg.claude_auth_mode.as_str() {
        "key" if !cfg.claude_secret.is_empty() => {
            cmd.env("ANTHROPIC_API_KEY", &cfg.claude_secret);
            cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
        }
        "subscription" if !cfg.claude_secret.is_empty() => {
            cmd.env("CLAUDE_CODE_OAUTH_TOKEN", &cfg.claude_secret);
            cmd.env_remove("ANTHROPIC_API_KEY");
        }
        _ => {}
    }
}

/// Проверить учётку Claude крошечным `claude -p` (1 слово). true → ключ/токен
/// валиден и `claude` доступен. Не зависит от глобального конфига — env ставим
/// явно из переданных mode/secret (вызывается ДО сохранения настроек).
pub async fn validate_claude_auth(mode: &str, secret: &str, timeout: Duration) -> bool {
    let Some(bin) = resolve_claude_bin() else {
        return false;
    };
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args([
        "-p",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--setting-sources",
        "project,local",
        "--model",
        "haiku",
        "ответь одним словом: ок",
    ])
    .current_dir(std::env::temp_dir())
    .env("JARVIS_IGNORE", "1")
    .env("DISABLE_NON_ESSENTIAL_MODEL_CALLS", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
    match mode {
        "key" => {
            cmd.env("ANTHROPIC_API_KEY", secret);
            cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
        }
        "subscription" => {
            cmd.env("CLAUDE_CODE_OAUTH_TOKEN", secret);
            cmd.env_remove("ANTHROPIC_API_KEY");
        }
        _ => return false,
    }
    let Ok(Ok(out)) = tokio::time::timeout(timeout, cmd.output()).await else {
        return false;
    };
    out.status.success() && !out.stdout.is_empty()
}

static SERVICE_CONFIG: RwLock<Option<ServiceConfig>> = RwLock::new(None);

/// Обновить процесс-глобальный конфиг служебного LLM (зовётся демоном из настроек).
pub fn set_service_config(cfg: ServiceConfig) {
    *SERVICE_CONFIG.write().unwrap() = Some(cfg);
}

/// Текущий конфиг (или дефолт `Auto`, если ещё не выставлен).
pub fn service_config() -> ServiceConfig {
    SERVICE_CONFIG.read().unwrap().clone().unwrap_or_default()
}

/// Эффективный egress-прокси служебных вызовов: явный из настроек («Под капотом»),
/// иначе — то, что в env процесса (HTTPS_PROXY приоритетнее HTTP_PROXY). None →
/// прокси не настроен ни там, ни там (прямой выход).
fn effective_proxy() -> Option<String> {
    let cfg = service_config();
    let from_cfg = cfg.proxy.trim();
    if !from_cfg.is_empty() {
        return Some(from_cfg.to_string());
    }
    for key in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Впрыснуть egress-прокси (и HTTP, и HTTPS) в служебную команду. Ключевой фикс
/// для Codex: его трафик к OpenAI идёт по HTTPS, а в env app выставлен только
/// HTTP_PROXY → без HTTPS_PROXY запрос висит до таймаута. Ставим обе переменные
/// (в верхнем и нижнем регистре — разные клиенты читают по-разному).
fn apply_proxy(cmd: &mut tokio::process::Command) {
    if let Some(p) = effective_proxy() {
        cmd.env("HTTP_PROXY", &p)
            .env("HTTPS_PROXY", &p)
            .env("http_proxy", &p)
            .env("https_proxy", &p);
    }
}

/// Codex как «функция текста» через официальный Python-SDK (`openai-codex`):
/// venv-python запускает codex-summary.py, JSON {prompt,model,effort,...} в stdin,
/// один JSON {ok,text} в stdout. read-only, web_search off, ephemeral — внутри
/// сайдкара. Выбранная модель/effort. None при любой ошибке (сработает фолбэк).
pub async fn run_codex_sdk(
    prompt: &str,
    model: &str,
    effort: &str,
    timeout: Duration,
) -> Option<String> {
    use tokio::io::AsyncWriteExt;
    let py = codex_sdk_python();
    let script = codex_sdk_script();
    if !py.exists() || !script.exists() {
        return None;
    }
    let req = serde_json::json!({
        "prompt": prompt,
        "model": if model.is_empty() { serde_json::Value::Null } else { serde_json::json!(model) },
        // minimal не поддерживают некоторые модели (spark → 400) — нормализуем в low
        "effort": match effort { "" | "minimal" => "low", e => e },
        "instructions": HAIKU_SYSTEM,
        "timeout": timeout.as_secs_f64(),
    })
    .to_string();
    crate::log::line(&service_request_metadata("codex-sdk", prompt));
    let mut cmd = tokio::process::Command::new(py);
    cmd.arg(script)
        .current_dir(std::env::temp_dir())
        .env("JARVIS_IGNORE", "1")
        .env("CODEX_HOME", ensure_codex_clean_home()) // без чужих MCP/скиллов → быстрый старт
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    apply_proxy(&mut cmd); // Codex → OpenAI по HTTPS: обязательный HTTPS_PROXY на этой сети
    let mut child = cmd.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(req.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    // Сайдкар на ошибке печатает {"ok":false,...} в stdout И выходит с кодом 1 —
    // поэтому stdout парсим в любом случае (не по success).
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed = parse_codex_sdk_output(&stdout);
    crate::log::line(&service_response_metadata("codex-sdk", parsed.as_deref()));
    parsed
}

/// Достать финальный текст из вывода codex-summary.py. Берём последнюю строку,
/// которая — валидный JSON с полем `ok`: `ok:true` → text; `ok:false`/мусор → None.
fn parse_codex_sdk_output(stdout: &str) -> Option<String> {
    for line in stdout.lines().rev() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v.get("ok").and_then(serde_json::Value::as_bool) {
            Some(true) => {
                let t = v
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                return (!t.is_empty()).then_some(t);
            }
            Some(false) => return None, // явная ошибка сайдкара
            None => continue,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_and_claude_prefer_claude_then_codex() {
        let all = service_order(ServiceBackend::Auto, true, true, true);
        assert_eq!(
            all,
            vec![Backend::Claude, Backend::CodexSdk, Backend::CodexExec]
        );
        let all_claude = service_order(ServiceBackend::Claude, true, true, true);
        assert_eq!(
            all_claude,
            vec![Backend::Claude, Backend::CodexSdk, Backend::CodexExec]
        );
    }

    #[test]
    fn codex_selected_prefers_codex_then_claude() {
        let o = service_order(ServiceBackend::Codex, true, true, true);
        assert_eq!(
            o,
            vec![Backend::CodexSdk, Backend::CodexExec, Backend::Claude]
        );
    }

    #[test]
    fn order_filters_unavailable_backends() {
        // codex выбран, но SDK не установлен → exec, потом claude-фолбэк
        let o = service_order(ServiceBackend::Codex, true, false, true);
        assert_eq!(o, vec![Backend::CodexExec, Backend::Claude]);
        // claude выбран, но claude нет → codex-фолбэк (саммари не пропадают)
        let o = service_order(ServiceBackend::Claude, false, true, true);
        assert_eq!(o, vec![Backend::CodexSdk, Backend::CodexExec]);
        // ничего нет → пусто
        assert!(service_order(ServiceBackend::Auto, false, false, false).is_empty());
    }

    #[test]
    fn backend_from_str_maps_known_and_defaults_auto() {
        assert_eq!(ServiceBackend::from_str("claude"), ServiceBackend::Claude);
        assert_eq!(ServiceBackend::from_str("codex"), ServiceBackend::Codex);
        assert_eq!(ServiceBackend::from_str("auto"), ServiceBackend::Auto);
        assert_eq!(ServiceBackend::from_str("чтоугодно"), ServiceBackend::Auto);
    }

    #[test]
    fn parse_sidecar_ok_returns_text() {
        let s = "{\"ok\": true, \"text\": \"привет мир\"}";
        assert_eq!(parse_codex_sdk_output(s), Some("привет мир".to_string()));
    }

    #[test]
    fn parse_sidecar_error_and_empty_return_none() {
        assert_eq!(
            parse_codex_sdk_output("{\"ok\": false, \"error\": \"boom\"}"),
            None
        );
        assert_eq!(
            parse_codex_sdk_output("{\"ok\": true, \"text\": \"   \"}"),
            None
        );
        assert_eq!(parse_codex_sdk_output("не json вовсе"), None);
        assert_eq!(parse_codex_sdk_output(""), None);
    }

    #[test]
    fn service_config_from_settings_reads_block_and_defaults() {
        let v = serde_json::json!({"service": {"backend":"codex","codexModel":"gpt-5.5","codexEffort":"minimal","proxy":"http://u:p@host:8080"}});
        let c = ServiceConfig::from_settings(&v);
        assert_eq!(c.backend, ServiceBackend::Codex);
        assert_eq!(c.codex_model, "gpt-5.5");
        assert_eq!(c.codex_effort, "minimal");
        assert_eq!(c.proxy, "http://u:p@host:8080");
        // пустой/отсутствующий блок → Auto + low + без модели + без прокси
        let d = ServiceConfig::from_settings(&serde_json::json!({}));
        assert_eq!(d.backend, ServiceBackend::Auto);
        assert_eq!(d.codex_effort, "low");
        assert_eq!(d.codex_model, "");
        assert_eq!(d.proxy, "");
    }

    #[test]
    fn parse_sidecar_picks_json_line_among_noise() {
        // stderr перенаправлен в сайдкаре, но на всякий — берём последнюю JSON-строку
        let s = "loading model...\nsome warning\n{\"ok\": true, \"text\": \"итог\"}\n";
        assert_eq!(parse_codex_sdk_output(s), Some("итог".to_string()));
    }

    #[test]
    fn service_request_metadata_contains_length_but_not_prompt() {
        let prompt = "секретный текст пользовательского запроса";

        let metadata = service_request_metadata("haiku", prompt);

        assert_eq!(
            metadata,
            format!("[haiku] request prompt_chars={}", prompt.chars().count())
        );
        assert!(!metadata.contains(prompt));
    }

    #[test]
    fn service_response_metadata_contains_status_and_length_but_not_text() {
        let response = "чувствительный ответ модели";

        let ok = service_response_metadata("codex-sdk", Some(response));
        let failed = service_response_metadata("codex-sdk", None);

        assert_eq!(
            ok,
            format!(
                "[codex-sdk] response status=ok response_chars={}",
                response.chars().count()
            )
        );
        assert!(!ok.contains(response));
        assert_eq!(
            failed,
            "[codex-sdk] response status=unavailable response_chars=0"
        );
    }
}
