//! Запуск настоящего бинаря `claude` для служебных нужд демона
//! (haiku-переводы, саммари, официальный /usage, effort-уровни).
//!
//! Все вызовы идут с JARVIS_IGNORE=1 — шим-хук видит переменную и не шлёт
//! события, иначе служебные запуски засоряли бы реестр сессий.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
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

/// Headless-вызов haiku одним промптом — общий путь переводов и саммари.
pub async fn run_haiku(prompt: &str, timeout: Duration) -> Option<String> {
    crate::log::line(&format!(
        "[haiku] → {}",
        crate::util::ellipsize(&crate::util::one_line(prompt), 300)
    ));
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
    crate::log::line(&format!(
        "[haiku] ← {}",
        match &out {
            Some(s) => crate::util::ellipsize(&crate::util::one_line(s), 300),
            None => "<нет ответа / таймаут>".into(),
        }
    ));
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
        "--ignore-user-config", // ноль чужих MCP/скиллов
        "-s",
        "read-only",
        "-c",
        "model_reasoning_effort=\"low\"", // не minimal: 400 при image_gen/web_search
        &full,
    ])
    .current_dir(std::env::temp_dir())
    .env("JARVIS_IGNORE", "1") // egress-прокси наследуется из env (как у claude)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
    let out = tokio::time::timeout(timeout, cmd.output()).await.ok()?.ok()?;
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

/// Служебный LLM-вызов (саммари/перевод), бэкенд-агностично: предпочитаем Claude
/// (дешёвый haiku), иначе — Codex (`codex exec`). На машине без claude саммари
/// больше не пропадают.
pub async fn run_service_llm(prompt: &str, timeout: Duration) -> Option<String> {
    if resolve_claude_bin().is_some() {
        if let Some(s) = run_haiku(prompt, timeout).await {
            return Some(s);
        }
    }
    if crate::backend::codex::resolve_codex_bin().is_some() {
        return run_codex_summary(prompt, timeout).await;
    }
    None
}
