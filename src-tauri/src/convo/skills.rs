//! Реестр голосовых скилов: меню для промпта + fail-closed валидация аргументов.
//! Reads → данные; route/control → consent. Чистая часть (меню, валидация)
//! юнит-тестируема; `dispatch` (исполнение) добавляется в оркестраторе.

use serde_json::Value;

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
