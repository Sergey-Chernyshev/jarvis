//! Question identity, strict answer validation and at-most-once delivery.
//! A successful tmux command acknowledges input injection, not agent acceptance.
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::model::{Question, QuestionItem, QuestionOption};
use serde_json::{json, Value};

pub fn fingerprint(text: &str) -> String {
    // Stable across processes and platforms (unlike DefaultHasher).
    let hash = text.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

pub fn request_id(q: &Question) -> String {
    if q.request_id.is_empty() {
        format!("legacy-{}", q.at)
    } else {
        q.request_id.clone()
    }
}

pub fn identify(q: &mut Question, preferred: Option<&str>) {
    if q.request_id.is_empty() {
        q.request_id = preferred
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "question-{}-{}",
                    q.at,
                    fingerprint(&serde_json::to_string(&q.questions).unwrap_or_default())
                )
            });
    }
    if q.revision == 0 {
        // Importing the same provider request with changed options must expire
        // existing answers even when a source reuses its call/item identifier.
        let content = fingerprint(&serde_json::to_string(&q.questions).unwrap_or_default());
        q.revision = u64::from_str_radix(&content[..8], 16).unwrap_or(0) + 1;
    }
    if q.transport.is_empty() {
        q.transport = "tmux".into();
    }
    for (i, item) in q.questions.iter_mut().enumerate() {
        if item.id.is_empty() {
            item.id = format!("q{}", i + 1);
        }
        for (j, option) in item.options.iter_mut().enumerate() {
            if option.id.is_empty() {
                option.id = format!("o{}", j + 1);
            }
        }
    }
}

pub fn same_request(a: &Question, b: &Question) -> bool {
    request_id(a) == request_id(b) && a.revision == b.revision
}

/// Parses the public Codex ToolRequestUserInputParams, without claiming ownership
/// of an RPC merely because a rollout contains a copy of its question.
pub fn from_codex_request(
    params: &Value,
    rpc_id: Value,
    connected: bool,
) -> Result<Question, String> {
    let field = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("Codex question: missing {key}"))
    };
    let thread_id = field("threadId")?;
    let turn_id = field("turnId")?;
    let item_id = field("itemId")?;
    let rows = params
        .get("questions")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty() && r.len() <= 32)
        .ok_or("Codex question: invalid questions")?;
    let mut ids = HashSet::new();
    let mut questions = Vec::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("Codex question: missing id")?;
        if !ids.insert(id.to_string()) {
            return Err("Codex question: duplicate id".into());
        }
        let question = row
            .get("question")
            .and_then(Value::as_str)
            .ok_or("Codex question: missing text")?
            .to_string();
        let mut options = Vec::new();
        if let Some(raw) = row.get("options").filter(|v| !v.is_null()) {
            let raw = raw
                .as_array()
                .filter(|r| r.len() <= 100)
                .ok_or("Codex question: invalid options")?;
            for (i, option) in raw.iter().enumerate() {
                options.push(QuestionOption {
                    id: format!("o{}", i + 1),
                    label: option
                        .get("label")
                        .and_then(Value::as_str)
                        .ok_or("Codex question: missing option label")?
                        .into(),
                    description: option
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                });
            }
        }
        questions.push(QuestionItem {
            id: id.into(),
            question,
            header: row
                .get("header")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            options,
            multi_select: false,
            // Codex accepts a freeform notes answer, including with options.
            custom_allowed: Some(true),
            custom_mode: "alternative".into(),
            is_secret: row
                .get("isSecret")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    let mut q = Question {
        at: crate::util::now_ms(),
        questions,
        transport: if connected { "codex-rpc" } else { "external" }.into(),
        provider_turn_id: Some(turn_id.clone()),
        provider_item_id: Some(item_id.clone()),
        rpc_request_id: Some(rpc_id),
        ..Question::default()
    };
    identify(
        &mut q,
        Some(&format!("codex-{thread_id}-{turn_id}-{item_id}")),
    );
    Ok(q)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedAnswers {
    pub submission_id: String,
    pub answers: Vec<Vec<u32>>,
    pub texts: Vec<Option<String>>,
}

pub fn custom_allowed(q: &Question, item: &QuestionItem, agent: crate::backend::Agent) -> bool {
    item.custom_allowed
        .unwrap_or(!q.from_screen && agent == crate::backend::Agent::Claude)
}

pub fn validate(
    q: &Question,
    choice: &Value,
    agent: crate::backend::Agent,
) -> Result<ValidatedAnswers, String> {
    if choice.get("requestId").and_then(Value::as_str) != Some(request_id(q).as_str())
        || choice.get("revision").and_then(Value::as_u64) != Some(q.revision)
    {
        return Err("Вопрос изменился. Открой текущий вопрос; прежний ответ сохранён.".into());
    }
    let submission_id = choice
        .get("submissionId")
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_:.".contains(c))
        })
        .ok_or("Не указан идентификатор отправки")?
        .to_string();
    let rows = choice
        .get("answers")
        .and_then(Value::as_array)
        .ok_or("Неверный формат ответов")?;
    if q.questions.is_empty() || rows.len() != q.questions.len() {
        return Err("Нужен ответ на каждый вопрос".into());
    }
    let legacy_texts = choice
        .get("texts")
        .map(|v| v.as_array().ok_or("Неверный формат текста"))
        .transpose()?;
    if legacy_texts.is_some_and(|t| t.len() != rows.len()) {
        return Err("Неверное количество текстовых ответов".into());
    }
    let mut answers = Vec::new();
    let mut texts = Vec::new();
    for (i, (item, row)) in q.questions.iter().zip(rows).enumerate() {
        let mut picks = Vec::new();
        let text_value;
        if let Some(raw) = row.as_array() {
            for n in raw {
                let n = n
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0 && (*n as usize) <= item.options.len())
                    .ok_or("Выбран несуществующий вариант")?;
                picks.push(n);
            }
            text_value = legacy_texts.and_then(|t| t.get(i));
        } else {
            let expected_id = if item.id.is_empty() {
                format!("q{}", i + 1)
            } else {
                item.id.clone()
            };
            if row.get("questionId").and_then(Value::as_str) != Some(expected_id.as_str()) {
                return Err("Ответ относится к другому вопросу".into());
            }
            let ids = row
                .get("optionIds")
                .and_then(Value::as_array)
                .ok_or("Неверный формат вариантов")?;
            for id in ids {
                let id = id.as_str().ok_or("Неверный идентификатор варианта")?;
                let index = item
                    .options
                    .iter()
                    .enumerate()
                    .position(|(j, o)| {
                        id == if o.id.is_empty() {
                            format!("o{}", j + 1)
                        } else {
                            o.id.clone()
                        }
                    })
                    .ok_or("Вариант больше не существует")?;
                picks.push(index as u32 + 1);
            }
            text_value = row.get("text");
        }
        let text = match text_value {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => {
                if text.len() > 32_768 || text.contains('\0') {
                    return Err("Ответ слишком длинный или содержит недопустимый символ".into());
                }
                let text = text.trim();
                (!text.is_empty()).then(|| text.to_string())
            }
            _ => return Err("Текст ответа должен быть строкой".into()),
        };
        let unique: HashSet<u32> = picks.iter().copied().collect();
        if unique.len() != picks.len() {
            return Err("Вариант выбран повторно".into());
        }
        if !item.multi_select && picks.len() > 1 {
            return Err("Здесь можно выбрать только один вариант".into());
        }
        if text.is_some() && !custom_allowed(q, item, agent) {
            return Err("Этот вопрос принимает только предложенные варианты".into());
        }
        if picks.is_empty() && text.is_none() {
            return Err(format!("Ответь на вопрос {}", i + 1));
        }
        // A custom single answer is explicit, rather than silently ignoring a selected option.
        if !item.multi_select && item.custom_mode != "notes" && text.is_some() && !picks.is_empty()
        {
            return Err("Выбери вариант или напиши свой ответ".into());
        }
        if item.custom_mode == "notes" && !item.options.is_empty() && picks.is_empty() {
            return Err("Выбери вариант, к которому добавляешь пояснение".into());
        }
        picks.sort_unstable();
        answers.push(picks);
        texts.push(text);
    }
    Ok(ValidatedAnswers {
        submission_id,
        answers,
        texts,
    })
}

pub fn encode_codex_response(q: &Question, answer: &ValidatedAnswers) -> Value {
    let answers: serde_json::Map<String, Value> = q
        .questions
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mut values: Vec<String> = answer.answers[i]
                .iter()
                .map(|n| item.options[*n as usize - 1].label.clone())
                .collect();
            if let Some(text) = &answer.texts[i] {
                values.push(text.clone());
            }
            (item.id.clone(), json!({ "answers": values }))
        })
        .collect();
    json!({ "answers": answers })
}

pub async fn answer(d: &std::sync::Arc<crate::daemon::Daemon>, sid: &str, choice: &Value) -> Value {
    let fail = |message: &str| json!({"ok":false,"delivery":"failed","error":message});
    let Some(session) = d.session(sid) else {
        return fail("Сессия не найдена");
    };
    let Some(q) = session.question.clone() else {
        return fail("Вопрос уже закрыт");
    };
    let agent = crate::backend::Agent::from_opt(session.agent.as_deref());
    let input = match validate(&q, choice, agent) {
        Ok(input) => input,
        Err(message) => return fail(&message),
    };
    if matches!(q.transport.as_str(), "external" | "codex-rpc") {
        return json!({"ok":false,"delivery":"unavailable","openInAgent":true,"error":"Этот вопрос принадлежит другому подключению. Открой его в Codex; черновик сохранён."});
    }
    let Some(pane) = session.tmux_pane.as_deref() else {
        return fail("Сессия вне управляемого терминала — открой приложение агента");
    };
    let target = match d.pane_target(&session) {
        Ok(target) => target,
        Err(message) => return fail(&message),
    };
    let mut claim = match Claim::begin(sid, &q, &input) {
        Ok(claim) => claim,
        Err(result) => return result,
    };
    let before = match target.screen(pane).await {
        Ok(screen) => screen,
        Err(message) => return claim.complete(fail(&message)),
    };
    let Some(parsed) = crate::screen_prompt::parse_capture(&before) else {
        return claim.complete(fail(
            "Экран вопроса не читается. Проверь терминал; ответ не отправлен.",
        ));
    };
    if !crate::tmux::screen_matches_item(&parsed, &q.questions[0])
        || q.screen
            .as_ref()
            .is_some_and(|s| s.fingerprint != parsed.state.fingerprint)
    {
        return claim.complete(fail(
            "В терминале уже другой вопрос. Открой текущий вопрос; ответ сохранён.",
        ));
    }
    if let Err(message) = crate::tmux::question_item_keys(
        agent,
        &q,
        0,
        &input.answers[0],
        input.texts[0].as_deref(),
        &parsed,
    ) {
        return claim.complete(fail(&message));
    }
    if !d
        .session(sid)
        .and_then(|s| s.question)
        .is_some_and(|current| same_request(&current, &q))
    {
        return claim.complete(fail(
            "Вопрос изменился во время проверки. Ответ не отправлен.",
        ));
    }
    claim.mark_started();
    if let Err(message) = target
        .answer_question_checked(pane, agent, &q, &input.answers, &input.texts)
        .await
    {
        return claim.complete(json!({"ok":false,"delivery":"unknown","error":format!("{message} Часть ввода могла дойти; автоматического повтора не будет.")}));
    }
    // Remote rollout observation runs every seven seconds. Keep the submission
    // pending long enough for its correlated tool result without replaying it.
    for attempt in 0..100 {
        let mut received = confirmed(sid, &q);
        if q.from_screen && !received {
            if let Ok(screen) = target.screen(pane).await {
                received = match crate::screen_prompt::parse_capture(&screen) {
                    Some(next) => next.state.fingerprint != parsed.state.fingerprint,
                    None => crate::screen_prompt::is_idle_screen(&screen),
                };
            }
        }
        if received {
            if q.from_screen {
                let mut removed = false;
                d.with_session(sid, |s| {
                    if s.question
                        .as_ref()
                        .is_some_and(|current| same_request(current, &q))
                    {
                        s.question = None;
                        s.status = crate::model::Status::Waiting;
                        s.updated_at = crate::util::now_ms();
                        removed = true;
                    }
                });
                if removed {
                    crate::windows::toast_remove(d, &format!("q-{sid}"));
                    d.push();
                }
                crate::screen_prompt::detect_stuck_prompt(d, sid).await;
                // A next question remains Waiting. When the picker has gone,
                // the agent is processing the accepted answer.
                d.with_session(sid, |s| {
                    if s.question.is_none() && s.status == crate::model::Status::Waiting {
                        s.status = crate::model::Status::Working;
                        s.updated_at = crate::util::now_ms();
                    }
                });
                d.push();
            }
            return claim.complete(json!({"ok":true,"delivery":"confirmed","channel":"tmux"}));
        }
        if attempt < 99 {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        }
    }
    claim.complete(json!({"ok":false,"delivery":"unknown","error":"Клавиши отправлены, но агент ещё не подтвердил ответ. Проверь терминал; черновик сохранён, повторная отправка остановлена."}))
}

struct Record {
    request: String,
    revision: u64,
    submission: String,
    payload: String,
    result: Option<Value>,
    at: i64,
    confirmed: bool,
}
fn records() -> &'static Mutex<HashMap<String, Record>> {
    static RECORDS: OnceLock<Mutex<HashMap<String, Record>>> = OnceLock::new();
    RECORDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One sequence per owning session. Cache unknown outcomes so a lost response
/// can never replay a plan into a different UI state.
pub struct Claim {
    session: String,
    submission: String,
    finished: bool,
    started: bool,
}
impl Claim {
    pub fn begin(session: &str, q: &Question, answer: &ValidatedAnswers) -> Result<Self, Value> {
        let payload = fingerprint(&json!([answer.answers, answer.texts]).to_string());
        let mut records = records().lock().unwrap();
        if let Some(record) = records.get(session) {
            if record.result.is_none() {
                return Err(
                    json!({"ok":false,"delivery":"sending","error":"Ответ уже отправляется"}),
                );
            }
            if record.request == request_id(q) && record.revision == q.revision {
                if record.submission == answer.submission_id && record.payload == payload {
                    return Err(record.result.clone().unwrap());
                }
                return Err(
                    json!({"ok":false,"delivery":"unknown","error":"Ответ на этот вопрос уже отправлен. Проверь состояние агента; повторный ввод остановлен."}),
                );
            }
        }
        if records.len() >= 512 {
            if let Some(oldest) = records
                .iter()
                .filter(|(_, r)| r.result.is_some())
                .min_by_key(|(_, r)| r.at)
                .map(|(k, _)| k.clone())
            {
                records.remove(&oldest);
            }
        }
        records.insert(
            session.into(),
            Record {
                request: request_id(q),
                revision: q.revision,
                submission: answer.submission_id.clone(),
                payload,
                result: None,
                at: crate::util::now_ms(),
                confirmed: false,
            },
        );
        Ok(Self {
            session: session.into(),
            submission: answer.submission_id.clone(),
            finished: false,
            started: false,
        })
    }
    pub fn mark_started(&mut self) {
        self.started = true;
    }
    pub fn complete(mut self, mut result: Value) -> Value {
        let mut records = records().lock().unwrap();
        if self.started {
            if let Some(record) = records.get_mut(&self.session) {
                if record.confirmed {
                    result = json!({"ok":true,"delivery":"confirmed","channel":"tmux"});
                }
                record.result = Some(result.clone());
            }
        } else {
            records.remove(&self.session);
        }
        self.finished = true;
        result
    }
}

pub fn confirm(session: &str, q: &Question) {
    if let Some(record) = records()
        .lock()
        .unwrap()
        .get_mut(session)
        .filter(|r| r.request == request_id(q) && r.revision == q.revision)
    {
        record.confirmed = true;
        if record.result.is_some() {
            record.result = Some(json!({"ok":true,"delivery":"confirmed","channel":"tmux"}));
        }
    }
}
pub fn confirmed(session: &str, q: &Question) -> bool {
    records()
        .lock()
        .unwrap()
        .get(session)
        .is_some_and(|r| r.confirmed && r.request == request_id(q) && r.revision == q.revision)
}
impl Drop for Claim {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut records = records().lock().unwrap();
        if self.started {
            if let Some(record) = records
                .get_mut(&self.session)
                .filter(|r| r.submission == self.submission)
            {
                record.result = Some(
                    json!({"ok":false,"delivery":"unknown","error":"Доставка прервана. Проверь терминал: ответ мог дойти."}),
                );
            }
        } else {
            records.remove(&self.session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Agent;
    fn fixture() -> Question {
        from_codex_request(&json!({"threadId":"thread","turnId":"turn","itemId":"item","questions":[
            {"id":"storage","question":"Где хранить?","options":[{"label":"Local","description":""},{"label":"VM","description":""}]},
            {"id":"notes","question":"Поясни","options":null}
        ]}), json!(42), true).unwrap()
    }
    fn payload(q: &Question) -> Value {
        json!({"requestId":q.request_id,"revision":q.revision,"submissionId":"submit-1","answers":[
            {"questionId":"storage","optionIds":["o2"],"text":null},
            {"questionId":"notes","optionIds":[],"text":"Первая строка\nВторая строка"}
        ]})
    }
    #[test]
    fn codex_schema_preserves_ids_and_multiline_answers() {
        let q = fixture();
        let answer = validate(&q, &payload(&q), Agent::Codex).unwrap();
        assert_eq!(
            encode_codex_response(&q, &answer),
            json!({"answers":{"storage":{"answers":["VM"]},"notes":{"answers":["Первая строка\nВторая строка"]}}})
        );
        assert_eq!(q.rpc_request_id, Some(json!(42)));
    }
    #[test]
    fn stale_revision_and_missing_identity_cannot_deliver() {
        let q = fixture();
        let mut p = payload(&q);
        p["revision"] = json!(0);
        assert!(validate(&q, &p, Agent::Codex).is_err());
        p.as_object_mut().unwrap().remove("requestId");
        assert!(validate(&q, &p, Agent::Codex).is_err());
    }

    #[test]
    fn reused_provider_id_with_changed_options_gets_a_new_revision() {
        let original = fixture();
        let mut changed = original.clone();
        changed.revision = 0;
        changed.questions[0].options[0].label = "Different meaning".into();
        identify(&mut changed, None);
        assert_eq!(original.request_id, changed.request_id);
        assert_ne!(original.revision, changed.revision);
        assert!(validate(&changed, &payload(&original), Agent::Codex).is_err());
    }
    #[test]
    fn strict_cardinality_types_and_option_identity() {
        let q = fixture();
        for bad in [
            json!(["o1", "o1"]),
            json!(["o1", "o2"]),
            json!(["gone"]),
            json!([2]),
        ] {
            let mut p = payload(&q);
            p["answers"][0]["optionIds"] = bad;
            assert!(validate(&q, &p, Agent::Codex).is_err());
        }
        let mut p = payload(&q);
        p["answers"] = json!([[4294967297u64], []]);
        p["texts"] = json!([null, "notes"]);
        assert!(validate(&q, &p, Agent::Codex).is_err());
    }
    #[test]
    fn duplicate_unknown_submission_never_replays_keys() {
        let q = fixture();
        let a = validate(&q, &payload(&q), Agent::Codex).unwrap();
        let sid = "qa-question-duplicate";
        let mut claim = Claim::begin(sid, &q, &a).ok().unwrap();
        assert!(Claim::begin(sid, &q, &a).is_err());
        claim.mark_started();
        let unknown = json!({"ok":false,"delivery":"unknown"});
        claim.complete(unknown.clone());
        assert_eq!(Claim::begin(sid, &q, &a).err(), Some(unknown));
        let mut next = q.clone();
        next.revision += 1;
        assert!(Claim::begin(sid, &next, &a).is_ok());
    }
    #[test]
    fn disconnected_rpc_import_is_explicitly_read_only() {
        let q = from_codex_request(&json!({"threadId":"t","turnId":"u","itemId":"i","questions":[{"id":"x","question":"Question"}]}),json!("rpc"),false).unwrap();
        assert_eq!(q.transport, "external");
    }

    #[test]
    fn correlated_ack_wins_a_timeout_race_and_is_cached() {
        let q = fixture();
        let a = validate(&q, &payload(&q), Agent::Codex).unwrap();
        let sid = "qa-question-ack-race";
        let mut claim = Claim::begin(sid, &q, &a).ok().unwrap();
        claim.mark_started();
        confirm(sid, &q);
        let result = claim.complete(json!({"ok":false,"delivery":"unknown"}));
        assert_eq!(result["delivery"], "confirmed");
        assert_eq!(Claim::begin(sid, &q, &a).err(), Some(result));
    }
}
