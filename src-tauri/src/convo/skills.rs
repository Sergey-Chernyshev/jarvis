//! Реестр голосовых скилов: меню для промпта + fail-closed валидация аргументов.
//! Reads → данные; route/control → consent. Чистая часть (меню, валидация)
//! юнит-тестируема; `dispatch` (исполнение) добавляется в оркестраторе.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::convo::plan::Action;
use crate::daemon::Daemon;

/// Исход исполнения скила (для НЕ-route скилов; route оркестрируется в convo,
/// т.к. ему нужен single-flight guard и он сам владеет HUD staged→sent).
#[derive(Debug, Clone, PartialEq)]
pub enum SkillOutcome {
    /// read → данные (для опц. 2-го вызова, чтобы сфразить устно).
    Data(Value),
    /// control: подтверждён и применён → озвучить короткое подтверждение.
    Controlled,
    /// пользователь сам отменил confirm / таймаут — HUD уже показал «Отменено».
    Cancelled,
    /// нелистовой скил / провал валидации / провал ядра → переспрос/сообщить.
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

/// Резолв id-подсказки от планировщика в ПОЛНЫЙ id живой сессии: точное
/// совпадение, иначе уникальный префикс (снапшот показывает 8 симв.), иначе
/// ошибка. Неоднозначность НЕ угадываем — сайд-эффект в чужую сессию недопустим.
pub fn resolve_sid(d: &Arc<Daemon>, hint: &str) -> Result<String, String> {
    if d.session(hint).is_some() {
        return Ok(hint.to_string());
    }
    let ids: Vec<String> = d
        .snapshot()
        .into_iter()
        .filter(|s| s.renamed_to.is_none())
        .map(|s| s.id)
        .collect();
    match_prefix(&ids, hint)
}

/// Уникальный префиксный матч (чистый, тестируемый). Ноль → не найдено;
/// больше одного → неоднозначно (не угадываем — чужой сайд-эффект недопустим).
fn match_prefix(ids: &[String], hint: &str) -> Result<String, String> {
    let m: Vec<&String> = ids.iter().filter(|id| id.starts_with(hint)).collect();
    match m.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err("сессия не найдена".into()),
        _ => Err("несколько сессий с таким id — уточни".into()),
    }
}

/// Прочитать хвост транскрипта сессии (как `chats.read`, in-process). `id` — полный.
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

/// Проверить результат ядра управления: ok:true → Controlled, иначе Rejected с
/// внятной причиной (не «Готово» при провале — VR-2).
fn outcome_from_core(res: &Value) -> SkillOutcome {
    if res.get("ok").and_then(Value::as_bool) == Some(true) {
        SkillOutcome::Controlled
    } else if res.get("needsTmux").and_then(Value::as_bool) == Some(true) {
        SkillOutcome::Rejected("сессия не в tmux".into())
    } else {
        let why = res.get("error").and_then(Value::as_str).unwrap_or("не вышло").to_string();
        SkillOutcome::Rejected(why)
    }
}

/// Исполнить НЕ-route действие плана. reads → Data; control → confirm → ядро.
/// route обрабатывается в `convo::converse_once` (нужен single-flight guard).
pub async fn dispatch(d: &Arc<Daemon>, action: &Action) -> SkillOutcome {
    match action.skill.as_str() {
        "time" => SkillOutcome::Data(json!({ "now": crate::convo::now_string() })),
        "session_chat" => match action.args.get("id").and_then(Value::as_str) {
            Some(id) => match resolve_sid(d, id) {
                Ok(full) => read_chat(d, &full),
                Err(e) => SkillOutcome::Rejected(e),
            },
            None => SkillOutcome::Rejected("нет id".into()),
        },

        // ── CONTROL: сайд-эффект → ПОЗИТИВНЫЙ confirm + валидация + проверка ядра ──
        "set_model" => {
            let (Some(id), Some(model)) = (
                action.args.get("id").and_then(Value::as_str),
                action.args.get("model").and_then(Value::as_str),
            ) else {
                return SkillOutcome::Rejected("нужны id и model".into());
            };
            let sid = match resolve_sid(d, id) {
                Ok(s) => s,
                Err(e) => return SkillOutcome::Rejected(e),
            };
            if let Err(e) = validate_model(model) {
                return SkillOutcome::Rejected(e);
            }
            if crate::convo::confirm(d, &format!("Переключить {} на {model}?", d.session_label(&sid))).await {
                outcome_from_core(&crate::ipc::set_model_core(d, &sid, model).await)
            } else {
                SkillOutcome::Cancelled
            }
        }
        "set_effort" => {
            let (Some(id), Some(level)) = (
                action.args.get("id").and_then(Value::as_str),
                action.args.get("level").and_then(Value::as_str),
            ) else {
                return SkillOutcome::Rejected("нужны id и level".into());
            };
            let sid = match resolve_sid(d, id) {
                Ok(s) => s,
                Err(e) => return SkillOutcome::Rejected(e),
            };
            if let Err(e) = validate_effort(level) {
                return SkillOutcome::Rejected(e);
            }
            if crate::convo::confirm(d, &format!("Поставить {} effort {level}?", d.session_label(&sid))).await {
                outcome_from_core(&crate::ipc::set_effort_core(d, &sid, level).await)
            } else {
                SkillOutcome::Cancelled
            }
        }
        "keep_awake" => {
            if action.args.get("off").and_then(Value::as_bool).unwrap_or(false) {
                if crate::convo::confirm(d, "Выключить режим «не спать»?").await {
                    // "off" = мастер-выкл (set_auto(false)+stop_manual+persist), а не
                    // "stop" (чистит лишь ручной слот, авто-грант остаётся) — L1/VR-4.
                    outcome_from_core(&crate::power::Power::cmd(d, "keep-awake", "off", &json!({})).await)
                } else {
                    SkillOutcome::Cancelled
                }
            } else {
                let m = action.args.get("minutes").and_then(Value::as_i64).unwrap_or(0);
                if let Err(e) = validate_minutes(m) {
                    return SkillOutcome::Rejected(e);
                }
                if crate::convo::confirm(d, &format!("Не давать маку уснуть {m} минут?")).await {
                    outcome_from_core(
                        &crate::power::Power::cmd(d, "keep-awake", "start-timer", &json!({ "minutes": m })).await,
                    )
                } else {
                    SkillOutcome::Cancelled
                }
            }
        }
        "mute" => {
            // mute{on} глушит звуковой аудит-след → ТОЛЬКО через confirm (как и off)
            let on = action.args.get("on").and_then(Value::as_bool).unwrap_or(false);
            let q = if on { "Выключить звук Джарвиса?" } else { "Включить звук Джарвиса?" };
            if crate::convo::confirm(d, q).await {
                d.voice.set_mute(on); // инфолибельно (void)
                SkillOutcome::Controlled
            } else {
                SkillOutcome::Cancelled
            }
        }

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

    #[test]
    fn match_prefix_unique_none_ambiguous() {
        let ids = vec!["aaaa1111".to_string(), "aaaa2222".to_string(), "bbbb3333".to_string()];
        assert_eq!(match_prefix(&ids, "bbbb").unwrap(), "bbbb3333"); // уникальный префикс
        assert_eq!(match_prefix(&ids, "aaaa1111").unwrap(), "aaaa1111"); // полный
        assert!(match_prefix(&ids, "zzzz").is_err()); // нет
        assert!(match_prefix(&ids, "aaaa").is_err()); // неоднозначно → не угадываем
    }
}
