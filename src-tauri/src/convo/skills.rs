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
    /// готовый ответ для озвучки verbatim (внешний ассистент: веб-поиск/Q&A).
    Answer(String),
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
- mute{on|off} — звук Джарвиса. args: {\"on\":<true|false>}\n\
- media{action} — управление воспроизведением (любой плеер). args: {\"action\":\"play|pause|toggle|next|prev\"}\n\
- system_volume{...} — системная громкость. args: {\"set\":0..100} | {\"delta\":±N} | {\"mute\":true|false} | {\"action\":\"up|down|mute|unmute\"}\n\
- open_app{name} — открыть приложение macOS. args: {\"name\":\"Safari\"}\n\
- session_detail{id} — детали сессии (ветка/модель/effort/последний промпт). args: {\"id\":\"<id>\"}\n\
- search_chats{query} — найти текст по чатам всех сессий. args: {\"query\":\"<текст>\"}\n\
- metrics — расход токенов/денег за сегодня. args: {}\n\
- limits — статус лимита провайдера. args: {}\n\
- assistant{query} — ответить на ЛЮБОЙ вопрос о внешнем мире, поиск в интернете, \
расчёты, переводы, «что такое…», «как…». Всё, что НЕ про сессии/агентов/OS. \
args: {\"query\":\"<суть вопроса своими словами>\"}"
        .to_string()
}

/// Найти совпадения `query` в тексте элементов чата (без учёта регистра). Берём
/// только текстовые реплики; возвращаем до `limit` НАИБОЛЕЕ СВЕЖИХ сниппетов
/// (роль + усечённый текст). Чистая — тестируема без I/O.
fn search_items(items: &[crate::transcript::ChatItem], query: &str, limit: usize) -> Vec<String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let mut hits: Vec<String> = items
        .iter()
        .filter(|it| it.kind == "text" && it.text.to_lowercase().contains(&q))
        .map(|it| format!("{}: {}", it.role, crate::util::ellipsize(it.text.trim(), 120)))
        .collect();
    if hits.len() > limit {
        hits = hits.split_off(hits.len() - limit); // последние limit = самые свежие
    }
    hits
}

/// Прочитать элементы чата сессии из её транскрипта (хвост 512K).
fn session_chat_items(transcript: &str) -> Vec<crate::transcript::ChatItem> {
    crate::transcript::chain_from_entries(crate::transcript::read_recent_entries(
        Path::new(transcript),
        512 * 1024,
    ))
    .iter()
    .flat_map(crate::transcript::to_chat_items)
    .collect()
}

/// Поиск по чатам ВСЕХ живых сессий → агрегированные совпадения (Data).
fn search_chats(d: &Arc<Daemon>, query: &str) -> SkillOutcome {
    let q = query.trim();
    if q.is_empty() {
        return SkillOutcome::Rejected("пустой запрос".into());
    }
    let mut results = Vec::new();
    for s in d.snapshot().into_iter().filter(|s| s.renamed_to.is_none()) {
        let Some(tr) = s.transcript.as_deref() else { continue };
        let hits = search_items(&session_chat_items(tr), q, 3);
        if !hits.is_empty() {
            results.push(json!({
                "session_id": s.id.chars().take(8).collect::<String>(),
                "project": s.project,
                "matches": hits,
            }));
        }
    }
    SkillOutcome::Data(json!({ "query": q, "results": results }))
}

/// Краткий RU-статус сессии (как в снапшоте).
fn status_ru(st: crate::model::Status) -> &'static str {
    use crate::model::Status::*;
    match st {
        Waiting => "ждёт",
        Working => "работает",
        Done => "готово",
        Limit => "лимит",
        Idle => "простаивает",
    }
}

/// Детали одной сессии (Data) — ветка/модель/effort/последний промпт.
fn session_detail(d: &Arc<Daemon>, id: &str) -> SkillOutcome {
    let Some(s) = d.session(id) else {
        return SkillOutcome::Rejected("сессия не найдена".into());
    };
    SkillOutcome::Data(json!({
        "id": s.id.chars().take(8).collect::<String>(),
        "project": s.project,
        "task": s.task,
        "status": status_ru(s.status),
        "branch": s.branch,
        "model": s.model,
        "effort": s.effort,
        "last_prompt": s.last_prompt.as_deref().map(|p| crate::util::ellipsize(p, 120)),
    }))
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

        // ── READS (free, без confirm) ──
        "session_detail" => match action.args.get("id").and_then(Value::as_str) {
            Some(id) => match resolve_sid(d, id) {
                Ok(full) => session_detail(d, &full),
                Err(e) => SkillOutcome::Rejected(e),
            },
            None => SkillOutcome::Rejected("нет id".into()),
        },
        "search_chats" => match action.args.get("query").and_then(Value::as_str) {
            Some(q) => search_chats(d, q),
            None => SkillOutcome::Rejected("нет query".into()),
        },
        "metrics" => SkillOutcome::Data(d.usage.stats("today")),
        "limits" => SkillOutcome::Data(serde_json::to_value(d.limits.state()).unwrap_or(Value::Null)),

        // ── ВНЕШНИЙ АССИСТЕНТ: веб-поиск / общие вопросы / «думать» (read-only) ──
        "assistant" => match action.args.get("query").and_then(Value::as_str) {
            Some(q) if !q.trim().is_empty() => {
                crate::route::hud::emit(
                    d,
                    crate::route::hud::Phase::Thinking { text: "ищу ответ…".into() },
                );
                match crate::agent::assistant::AssistantHost::run(
                    q.trim(),
                    crate::agent::assistant::ASSISTANT_TIMEOUT,
                )
                .await
                {
                    Some(ans) => SkillOutcome::Answer(ans),
                    None => SkillOutcome::Rejected("не смогла найти ответ".into()),
                }
            }
            _ => SkillOutcome::Rejected("нет запроса".into()),
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

        // ── OS-CONTROL: benign-reversible → AUTO (без confirm), строгая валидация ──
        "media" => match action.args.get("action").and_then(Value::as_str) {
            Some(a) => match crate::convo::os::run_media(a) {
                Ok(()) => SkillOutcome::Controlled,
                Err(e) => SkillOutcome::Rejected(e),
            },
            None => SkillOutcome::Rejected("нет action".into()),
        },
        "system_volume" => match crate::convo::os::run_volume(&action.args) {
            Ok(()) => SkillOutcome::Controlled,
            Err(e) => SkillOutcome::Rejected(e),
        },
        "open_app" => match action.args.get("name").and_then(Value::as_str) {
            Some(n) => match crate::convo::os::run_open_app(n) {
                Ok(()) => SkillOutcome::Controlled,
                Err(e) => SkillOutcome::Rejected(e),
            },
            None => SkillOutcome::Rejected("нет name".into()),
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
        for s in [
            "route", "set_model", "set_effort", "keep_awake", "mute", "session_chat", "time",
            "media", "system_volume", "open_app", "session_detail", "search_chats", "metrics",
            "limits", "assistant",
        ] {
            assert!(m.contains(s), "меню без {s}");
        }
    }

    fn item(role: &'static str, kind: &'static str, text: &str) -> crate::transcript::ChatItem {
        crate::transcript::ChatItem { role, kind, text: text.into(), ts: 0 }
    }

    #[test]
    fn search_items_case_insensitive_text_only() {
        let items = vec![
            item("user", "text", "Почини сборку фронта"),
            item("assistant", "tool", "сборка через cargo build"), // kind=tool → пропускаем
            item("assistant", "text", "Сборка готова"),
        ];
        let hits = search_items(&items, "сборк", 5);
        assert_eq!(hits.len(), 2, "оба текстовых совпадения (без учёта регистра)");
        assert!(hits.iter().all(|h| !h.contains("cargo")), "tool-реплики не ищем");
    }

    #[test]
    fn search_items_keeps_most_recent_within_limit() {
        let items = vec![
            item("user", "text", "тест 1"),
            item("user", "text", "тест 2"),
            item("user", "text", "тест 3"),
        ];
        let hits = search_items(&items, "тест", 2);
        assert_eq!(hits.len(), 2);
        assert!(hits[1].contains("тест 3"), "последний = самый свежий");
        assert!(!hits.iter().any(|h| h.contains("тест 1")), "старейший вытеснен");
    }

    #[test]
    fn search_items_empty_query_no_hits() {
        let items = vec![item("user", "text", "что-то")];
        assert!(search_items(&items, "   ", 5).is_empty());
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
