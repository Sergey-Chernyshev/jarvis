//! Узкий LLM-tie-break: один вызов `claude -p` (без MCP-тулзов, без мультитёрна)
//! на близких кандидатах. Это НЕ агент-луп — одноразовая классификация
//! «реплика → session_id». Любая ошибка/таймаут → None (падаем в пикер).
//!
//! Логика (сборка промпта, парс) — в `prompt.rs` (чистая, юнит-тесты). Здесь —
//! только тонкий subprocess-вызов через общий хардненный `claude_bin::run_claude`.

use std::time::Duration;

pub use super::prompt::Candidate;
use super::prompt::{build_classify_prompt, parse_choice};

/// Таймаут одного tie-break вызова. ~12с (а не 6) — холодный `claude -p`
/// поднимается 11–20с; со срезанным boot (флаги ниже) укладывается, иначе
/// упирался бы в таймаут и tie-break не срабатывал бы НИКОГДА (SEC-1).
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(12);

/// Выбрать сессию среди близких кандидатов узким вызовом Клода.
/// Возвращает (session_id, confidence) или None (нет бинаря / ошибка / таймаут /
/// модель не выбрала). Решение «слать ли» принимает `decide_action` по порогу.
pub async fn classify_ambiguous(transcript: &str, candidates: &[Candidate]) -> Option<(String, f32)> {
    if candidates.is_empty() {
        return None;
    }
    let prompt = build_classify_prompt(transcript, candidates);

    // Те же срезы boot-оверхеда, что у служебного run_haiku: без MCP-серверов,
    // скилов, user-настроек/хуков. КРИТИЧНО для безопасности — недоверенный
    // текст с открытого микрофона НЕ должен тащить MCP-тулзы/хуки (SEC-1) — и
    // для латентности (cold boot 11–20с → быстро). --output-format json — чтобы
    // parse_choice разворачивал конверт. Позиционный prompt — последним.
    let args = [
        "-p",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--setting-sources",
        "project,local",
        "--model",
        "haiku",
        "--output-format",
        "json",
        prompt.as_str(),
    ];

    // run_claude сам применяет таймаут, temp-cwd, kill_on_drop и сохраняет прокси.
    let out = crate::claude_bin::run_claude(&args, CLASSIFY_TIMEOUT).await?;

    // защита: модель обязана выбрать id ИЗ списка кандидатов (анти-инъекция)
    match parse_choice(&out) {
        Some((sid, conf)) if candidates.iter().any(|c| c.session_id == sid) => Some((sid, conf)),
        _ => None,
    }
}
