//! Узкий LLM-tie-break: один вызов `claude -p` (без MCP-тулзов, без мультитёрна)
//! на близких кандидатах. Это НЕ агент-луп — одноразовая классификация
//! «реплика → session_id». Любая ошибка/таймаут → None (падаем в пикер).
//!
//! Логика (сборка промпта, парс) — в `prompt.rs` (чистая, юнит-тесты). Здесь —
//! только тонкий subprocess-вызов.

use std::time::Duration;

pub use super::prompt::Candidate;
use super::prompt::{build_classify_prompt, parse_choice};

/// Таймаут одного tie-break вызова. Дольше — не ждём, отдаём в пикер.
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(6);

/// Выбрать сессию среди близких кандидатов узким вызовом Клода.
/// Возвращает (session_id, confidence) или None (нет бинаря / ошибка / таймаут /
/// модель не выбрала). Решение «слать ли» принимает `decide_action` по порогу.
pub async fn classify_ambiguous(transcript: &str, candidates: &[Candidate]) -> Option<(String, f32)> {
    if candidates.is_empty() {
        return None;
    }
    let bin = crate::claude_bin::resolve_claude_bin()?;
    let prompt = build_classify_prompt(transcript, candidates);

    let out = match tokio::time::timeout(CLASSIFY_TIMEOUT, run_claude(&bin, &prompt)).await {
        Ok(Some(s)) => s,
        Ok(None) => return None,
        Err(_) => {
            crate::log::line("[route] tie-break: таймаут, в пикер");
            return None;
        }
    };
    let choice = parse_choice(&out);
    // защита: модель обязана выбрать id ИЗ списка кандидатов
    match choice {
        Some((sid, conf)) if candidates.iter().any(|c| c.session_id == sid) => Some((sid, conf)),
        _ => None,
    }
}

/// Запустить `claude -p <prompt>` в temp-папке (без проектного .mcp/.claude),
/// собрать stdout. None на ошибке запуска/ненулевом коде.
async fn run_claude(bin: &std::path::Path, prompt: &str) -> Option<String> {
    use tokio::process::Command;
    let output = Command::new(bin)
        .arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("json")
        .current_dir(std::env::temp_dir())
        .env("JARVIS_IGNORE", "1")
        .env("DISABLE_NON_ESSENTIAL_MODEL_CALLS", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}
