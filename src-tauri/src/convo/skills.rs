//! Реестр голосовых скилов: меню для промпта + fail-closed валидация аргументов.
//! Reads → данные; route/control → consent. Чистая часть (меню, валидация)
//! юнит-тестируема; `dispatch` (исполнение) добавляется в оркестраторе.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::convo::plan::Action;
use crate::daemon::Daemon;
use crate::route::SfGuard;

/// Исход исполнения скила.
#[derive(Debug, Clone, PartialEq)]
pub enum SkillOutcome {
    /// read → данные (для опц. 2-го вызова, чтобы сфразить устно).
    Data(Value),
    /// route/control → ушло в окно отмены / подтверждение.
    Staged,
    /// нелистовой скил / провал валидации → переспрос.
    Rejected(String),
}

/// Разрешённые модели и уровни effort (fail-closed аллоулисты).
pub const MODELS: &[&str] = &["opus", "sonnet", "haiku", "fable"];
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Значение «чистое»: непустое, без пробелов и control-символов (защита от
/// инъекции slash-команды в tmux-пану — туда уходит /model {x}, /effort {x}).
fn clean(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

pub fn validate_model(m: &str) -> Result<(), String> {
    if clean(m) && MODELS.contains(&m) {
        Ok(())
    } else {
        Err(format!("неизвестная модель: {m}"))
    }
}

pub fn validate_effort(e: &str) -> Result<(), String> {
    if clean(e) && EFFORTS.contains(&e) {
        Ok(())
    } else {
        Err(format!("неизвестный effort: {e}"))
    }
}

pub fn validate_minutes(m: i64) -> Result<(), String> {
    if (1..=600).contains(&m) {
        Ok(())
    } else {
        Err(format!("минуты вне диапазона: {m}"))
    }
}

/// Меню скилов для промпта (имя · что делает · аргументы).
pub fn skills_menu() -> String {
    "\
- time — текущие время/дата. args: {}\n\
- session_chat{id} — последние сообщения сессии. args: {\"id\":\"<id>\"}\n\
- route{prompt} — отправить промпт в подходящую сессию (выбор/уточнение автоматически). args: {\"prompt\":\"<текст>\"}\n\
- set_model{id,model} — сменить модель сессии. args: {\"id\":\"<id>\",\"model\":\"opus|sonnet|haiku|fable\"}\n\
- set_effort{id,level} — сменить effort сессии. args: {\"id\":\"<id>\",\"level\":\"low|medium|high|xhigh|max\"}\n\
- keep_awake{minutes|off} — не давать маку уснуть. args: {\"minutes\":<1..600>} или {\"off\":true}\n\
- mute{on|off} — звук Джарвиса. args: {\"on\":<true|false>}"
        .to_string()
}

/// Прочитать хвост транскрипта сессии (как `chats.read`, in-process).
fn read_chat(d: &Arc<Daemon>, id: &str) -> SkillOutcome {
    let Some(s) = d.session(id) else {
        return SkillOutcome::Rejected("сессия не найдена".into());
    };
    let Some(tr) = s.transcript else {
        return SkillOutcome::Rejected("нет транскрипта сессии".into());
    };
    let items: Vec<crate::transcript::ChatItem> = crate::transcript::chain_from_entries(
        crate::transcript::read_recent_entries(Path::new(&tr), 512 * 1024),
    )
    .iter()
    .flat_map(crate::transcript::to_chat_items)
    .collect();
    let start = items.len().saturating_sub(40);
    SkillOutcome::Data(json!({ "session_id": id, "project": s.project, "items": &items[start..] }))
}

/// Исполнить действие плана. reads → Data; route → Staged (через route::*).
/// control (set_model/set_effort/keep_awake/mute) — добавляется в Task 6.
/// `guard` уезжает в route (держит single-flight весь stage-window); для прочих
/// веток дропается по выходу.
pub async fn dispatch(d: &Arc<Daemon>, action: &Action, guard: SfGuard) -> SkillOutcome {
    match action.skill.as_str() {
        "time" => SkillOutcome::Data(json!({ "now": crate::convo::now_string() })),
        "session_chat" => match action.args.get("id").and_then(Value::as_str) {
            Some(id) => read_chat(d, id),
            None => SkillOutcome::Rejected("нет id".into()),
        },
        "route" => match action.args.get("prompt").and_then(Value::as_str) {
            Some(prompt) => {
                // полный путь п/п-1: скоринг → stage-then-send / пикер; guard внутрь
                crate::route::route_transcript(d.clone(), prompt.to_string(), guard).await;
                SkillOutcome::Staged
            }
            None => SkillOutcome::Rejected("нет prompt".into()),
        },
        other => SkillOutcome::Rejected(format!("неизвестный скил: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_lists_core_skills() {
        let m = skills_menu();
        for s in ["route", "set_model", "set_effort", "keep_awake", "mute", "session_chat", "time"] {
            assert!(m.contains(s), "меню без {s}");
        }
    }

    #[test]
    fn validate_model_allowlist() {
        assert!(validate_model("opus").is_ok());
        assert!(validate_model("sonnet").is_ok());
        assert!(validate_model("gpt-4").is_err());
        assert!(validate_model("opus; rm -rf").is_err());
    }

    #[test]
    fn validate_effort_enum() {
        assert!(validate_effort("high").is_ok());
        assert!(validate_effort("ultra").is_err());
    }

    #[test]
    fn validate_minutes_range() {
        assert!(validate_minutes(60).is_ok());
        assert!(validate_minutes(0).is_err());
        assert!(validate_minutes(100_000).is_err());
    }

    #[test]
    fn rejects_whitespace_control_chars() {
        assert!(validate_model("op us").is_err());
        assert!(validate_model("opus\n").is_err());
        assert!(validate_effort("hi gh").is_err());
    }
}
