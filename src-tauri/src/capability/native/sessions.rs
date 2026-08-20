//! Капабилити сессий. Read-часть (list/get/wait) — фаза 2; control-часть
//! (reply/queue/control/launch/interrupt) — фаза 3. Делегирует в реестр
//! сессий демона (`daemon.rs`), ничего не дублируя.
//!
//! `sessions.wait` — наблюдение, а не действие (класс Read), но единственная
//! капабилити, которая по своей природе висит минутами: у неё собственный
//! дедлайн гейта (`register_with_timeout`), общих 30с ей мало.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;
use crate::model::{Session, Status};

use super::arg_str;

/// Шаг опроса — как у `Daemon::await_prompt_ack`: возвращаемся сразу, как только
/// дождались, а не спим весь бюджет (ответ обычно приходит за секунды).
const POLL_STEP: Duration = Duration::from_millis(200);
const DEFAULT_WAIT: Duration = Duration::from_secs(120);
/// Потолок для `timeout_sec`. Меньше дедлайна капабилити: гейт не должен убивать
/// вызов ровно в тот момент, когда тот сам сдаётся, — иначе агент вместо честного
/// «timeout» получит невнятное «failed:timeout» без причины.
const MAX_WAIT: Duration = Duration::from_secs(280);
const WAIT_DEADLINE: Duration = Duration::from_secs(300);

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "sessions.list",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Список живых сессий Claude Code с их статусом (что сейчас запущено, что работает/ждёт/закончило).",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        make_handler(|d: Arc<Daemon>, _args: Value| async move {
            serde_json::to_value(d.snapshot()).map_err(|e| e.to_string())
        }),
    );

    reg.register(
        CapabilityMeta {
            id: "sessions.get",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Состояние одной сессии по её id.",
            input_schema: json!({
                "type": "object",
                "properties": { "session_id": { "type": "string", "description": "id сессии" } },
                "required": ["session_id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = arg_str(&args, "session_id")?;
            match d.session(&sid) {
                Some(s) => serde_json::to_value(s).map_err(|e| e.to_string()),
                None => Err(format!("сессия не найдена: {sid}")),
            }
        }),
    );

    reg.register_with_timeout(
        CapabilityMeta {
            id: "sessions.wait",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Дождаться, пока сессия придёт в нужное состояние — обычно после sessions.reply, \
чтобы узнать результат отправленного промпта. Опрашивает состояние и возвращается СРАЗУ, как только дождался \
(обычно секунды), максимум timeout_sec (по умолчанию 120, потолок 280). \
ОЖИДАНИЕ НЕ БЕСПЛАТНО: пока этот вызов висит, ты не делаешь ничего другого — не зови «на всякий случай» \
и не ставь большой таймаут без нужды; если ждать нечего, хватит sessions.get. \
Поле outcome в ответе: 'reached' — дождались; 'blocked' — сессия встала и сама до цели не дойдёт \
(waiting = спрашивает человека или просит разрешения, limit = упёрлась в лимит, idle = не работает: \
перезапуск или промпт не дошёл); 'timeout' — не успела, работа продолжается; 'gone' — сессия исчезла. \
Флаг immediate=true означает, что сессия была в этом состоянии уже на первом опросе — перехода мы не видели.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "id сессии" },
                    "until": {
                        "type": "string",
                        "enum": ["done", "waiting", "settled"],
                        "description": "чего ждём: done — сессия закончила ответ (по умолчанию); waiting — сессия спросила человека; settled — сессия перестала работать (любое состояние, кроме working)"
                    },
                    "timeout_sec": {
                        "type": "number",
                        "description": "сколько максимум ждать, секунд (1..280, по умолчанию 120)"
                    }
                },
                "required": ["session_id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move {
            let sid = arg_str(&args, "session_id")?;
            let until = Until::parse(args.get("until").and_then(|v| v.as_str()).unwrap_or("done"))?;
            let budget = budget_of(args.get("timeout_sec"));
            let (probe_d, probe_sid) = (d.clone(), sid.clone());
            let rep = wait_loop(
                move || probe_d.session(&probe_sid).map(|s| s.status),
                until,
                budget,
                POLL_STEP,
            )
            .await;
            Ok(report_json(&sid, until, budget, &rep, d.session(&sid).as_ref()))
        }),
        WAIT_DEADLINE,
    );
}

/* ================= ждущая логика (чистая, тестируется без демона) ================= */

/// Чего ждём. Ждать осмысленно только то, что наступает само: `idle` и `limit` —
/// не цели, а преграды, они приходят исходом `blocked`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Until {
    Done,
    Waiting,
    Settled,
}

impl Until {
    pub(crate) fn parse(s: &str) -> Result<Until, String> {
        match s {
            "done" => Ok(Until::Done),
            "waiting" => Ok(Until::Waiting),
            "settled" => Ok(Until::Settled),
            other => Err(format!(
                "until='{other}' не бывает; допустимо: done | waiting | settled"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Until::Done => "done",
            Until::Waiting => "waiting",
            Until::Settled => "settled",
        }
    }

    fn matches(self, st: Status) -> bool {
        match self {
            Until::Done => st == Status::Done,
            Until::Waiting => st == Status::Waiting,
            Until::Settled => st != Status::Working,
        }
    }
}

/// Исход ожидания. Разные исходы требуют от агента разных действий, поэтому
/// молчаливого «не дождались» тут нет: причина есть у каждого.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Outcome {
    Reached(Status),
    Blocked(Status),
    Gone,
    Timeout(Option<Status>),
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Reached(_) => "reached",
            Outcome::Blocked(_) => "blocked",
            Outcome::Gone => "gone",
            Outcome::Timeout(_) => "timeout",
        }
    }

    fn status(self) -> Option<Status> {
        match self {
            Outcome::Reached(st) | Outcome::Blocked(st) => Some(st),
            Outcome::Timeout(st) => st,
            Outcome::Gone => None,
        }
    }
}

pub(crate) struct Report {
    pub outcome: Outcome,
    pub waited_ms: u128,
    /// Сколько раз опросили состояние. 1 — исход был виден сразу, перехода не было.
    pub polls: u32,
}

/// Решение по одному опросу: `None` — ждём дальше.
fn decide(cur: Option<Status>, until: Until) -> Option<Outcome> {
    match cur {
        None => Some(Outcome::Gone),
        Some(st) if until.matches(st) => Some(Outcome::Reached(st)),
        // Работа встала и сама не поедет: нужен человек (вопрос/разрешение),
        // сброс лимита провайдера — или сессия вообще не работает. Ждать до
        // конца бюджета тут бессмысленно, а для агента это разные действия.
        Some(st @ (Status::Waiting | Status::Limit | Status::Idle)) => Some(Outcome::Blocked(st)),
        _ => None,
    }
}

/// Опрос с шагом, возврат сразу по факту. `probe` — источник статуса (в бою —
/// реестр демона, в тестах — заготовленная лента).
pub(crate) async fn wait_loop<P>(probe: P, until: Until, budget: Duration, step: Duration) -> Report
where
    P: Fn() -> Option<Status>,
{
    let t0 = Instant::now();
    let mut polls = 0u32;
    loop {
        let cur = probe();
        polls += 1;
        if let Some(outcome) = decide(cur, until) {
            return Report { outcome, waited_ms: t0.elapsed().as_millis(), polls };
        }
        let left = budget.saturating_sub(t0.elapsed());
        if left.is_zero() {
            return Report {
                outcome: Outcome::Timeout(cur),
                waited_ms: t0.elapsed().as_millis(),
                polls,
            };
        }
        tokio::time::sleep(step.min(left)).await;
    }
}

/// Бюджет ожидания из аргумента: мусор — дефолт, число — в рамках [1с, MAX_WAIT].
fn budget_of(arg: Option<&Value>) -> Duration {
    match arg.and_then(|v| v.as_f64()) {
        Some(sec) if sec.is_finite() => {
            Duration::from_secs_f64(sec.max(0.0)).clamp(Duration::from_secs(1), MAX_WAIT)
        }
        _ => DEFAULT_WAIT,
    }
}

/// Человекочитаемая причина исхода — то, по чему агент поймёт, что делать дальше.
fn reason_of(outcome: Outcome, until: Until, budget: Duration) -> String {
    match outcome {
        Outcome::Reached(_) => format!("дождались состояния '{}'", until.as_str()),
        Outcome::Blocked(Status::Waiting) => {
            "сессия ждёт человека (вопрос или запрос разрешения) — сама дальше не пойдёт, нужен ответ"
                .into()
        }
        Outcome::Blocked(Status::Limit) => {
            "сессия упёрлась в лимит провайдера — продолжит после сброса лимита".into()
        }
        Outcome::Blocked(_) => {
            "сессия ничего не делает (перезапуск или промпт до неё не дошёл) — ждать нечего".into()
        }
        Outcome::Gone => "сессия исчезла из реестра (закрыта или убита) — ждать нечего".into(),
        Outcome::Timeout(_) => format!(
            "время вышло ({}с), сессия всё ещё работает — можно подождать ещё",
            budget.as_secs()
        ),
    }
}

fn report_json(
    sid: &str,
    until: Until,
    budget: Duration,
    rep: &Report,
    snap: Option<&Session>,
) -> Value {
    let mut v = json!({
        "ok": matches!(rep.outcome, Outcome::Reached(_)),
        "outcome": rep.outcome.as_str(),
        "sessionId": sid,
        "until": until.as_str(),
        "waitedMs": rep.waited_ms as u64,
        "timeoutSec": budget.as_secs(),
        "reason": reason_of(rep.outcome, until, budget),
    });
    if let Some(st) = rep.outcome.status() {
        v["status"] = json!(st);
    }
    // Исход был виден на первом же опросе — перехода мы не наблюдали. Для агента
    // это разница между «сессия ответила на мой промпт» и «она и была готова».
    if rep.polls == 1 && !matches!(rep.outcome, Outcome::Timeout(_)) {
        v["immediate"] = json!(true);
    }
    if let Some(s) = snap {
        v["detail"] = json!(s.detail);
        if let Some(q) = &s.question {
            v["question"] = serde_json::to_value(q).unwrap_or(Value::Null);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const STEP: Duration = Duration::from_millis(5);

    /// Лента статусов: каждый опрос отдаёт следующий, последний повторяется.
    fn tape(items: Vec<Option<Status>>) -> impl Fn() -> Option<Status> {
        let i = AtomicUsize::new(0);
        move || {
            let n = i.fetch_add(1, Ordering::SeqCst);
            items[n.min(items.len() - 1)]
        }
    }

    #[tokio::test]
    async fn returns_as_soon_as_status_reached() {
        let probe = tape(vec![
            Some(Status::Working),
            Some(Status::Working),
            Some(Status::Done),
        ]);
        let rep = wait_loop(probe, Until::Done, Duration::from_secs(5), STEP).await;
        assert_eq!(rep.outcome, Outcome::Reached(Status::Done));
        assert_eq!(rep.polls, 3, "вернулись на третьем опросе, а не по бюджету");
        assert!(rep.waited_ms < 1000, "ждали доли секунды, не весь бюджет");
    }

    #[tokio::test]
    async fn times_out_and_reports_last_status() {
        let probe = tape(vec![Some(Status::Working)]);
        let rep = wait_loop(probe, Until::Done, Duration::from_millis(40), STEP).await;
        assert_eq!(rep.outcome, Outcome::Timeout(Some(Status::Working)));
        assert!(rep.polls > 1);
    }

    #[tokio::test]
    async fn vanished_session_is_gone_not_timeout() {
        let probe = tape(vec![Some(Status::Working), None]);
        let rep = wait_loop(probe, Until::Done, Duration::from_secs(5), STEP).await;
        assert_eq!(rep.outcome, Outcome::Gone);
    }

    // Ключевое различие: вместо ответа сессия попросила человека.
    #[tokio::test]
    async fn question_instead_of_answer_is_blocked() {
        let probe = tape(vec![Some(Status::Working), Some(Status::Waiting)]);
        let rep = wait_loop(probe, Until::Done, Duration::from_secs(5), STEP).await;
        assert_eq!(rep.outcome, Outcome::Blocked(Status::Waiting));
        let reason = reason_of(rep.outcome, Until::Done, Duration::from_secs(5));
        assert!(reason.contains("ждёт человека"), "причина названа: {reason}");
    }

    #[tokio::test]
    async fn limit_is_blocked_too() {
        let probe = tape(vec![Some(Status::Limit)]);
        let rep = wait_loop(probe, Until::Done, Duration::from_secs(5), STEP).await;
        assert_eq!(rep.outcome, Outcome::Blocked(Status::Limit));
        assert_eq!(rep.polls, 1);
    }

    // until=waiting: ждём именно вопроса — и он же считается достигнутым, а не преградой.
    #[tokio::test]
    async fn waiting_target_is_reached_not_blocked() {
        let probe = tape(vec![Some(Status::Working), Some(Status::Waiting)]);
        let rep = wait_loop(probe, Until::Waiting, Duration::from_secs(5), STEP).await;
        assert_eq!(rep.outcome, Outcome::Reached(Status::Waiting));
    }

    // until=settled: любое состояние, кроме working, — цель.
    #[tokio::test]
    async fn settled_accepts_any_non_working() {
        for st in [Status::Done, Status::Waiting, Status::Idle, Status::Limit] {
            let rep = wait_loop(tape(vec![Some(st)]), Until::Settled, Duration::from_secs(5), STEP)
                .await;
            assert_eq!(rep.outcome, Outcome::Reached(st), "settled должен ловить {st:?}");
        }
        let rep = wait_loop(
            tape(vec![Some(Status::Working)]),
            Until::Settled,
            Duration::from_millis(20),
            STEP,
        )
        .await;
        assert_eq!(rep.outcome, Outcome::Timeout(Some(Status::Working)));
    }

    #[test]
    fn until_parse_rejects_garbage() {
        assert_eq!(Until::parse("done"), Ok(Until::Done));
        assert!(Until::parse("working").is_err(), "working ждать бессмысленно");
        assert!(Until::parse("").unwrap_err().contains("done | waiting | settled"));
    }

    #[test]
    fn budget_is_clamped_and_defaulted() {
        assert_eq!(budget_of(None), DEFAULT_WAIT);
        assert_eq!(budget_of(Some(&json!("три минуты"))), DEFAULT_WAIT);
        assert_eq!(budget_of(Some(&json!(30))), Duration::from_secs(30));
        assert_eq!(budget_of(Some(&json!(100000))), MAX_WAIT, "потолок ожидания");
        assert_eq!(budget_of(Some(&json!(-5))), Duration::from_secs(1), "минимум — секунда");
        assert!(MAX_WAIT < WAIT_DEADLINE, "у гейта должен остаться зазор");
    }

    // Ответ агенту: причина названа в каждом исходе, ok=true только у reached.
    #[test]
    fn report_names_the_reason_for_every_outcome() {
        let budget = Duration::from_secs(60);
        for (outcome, name, ok) in [
            (Outcome::Reached(Status::Done), "reached", true),
            (Outcome::Blocked(Status::Waiting), "blocked", false),
            (Outcome::Gone, "gone", false),
            (Outcome::Timeout(Some(Status::Working)), "timeout", false),
        ] {
            let rep = Report { outcome, waited_ms: 12, polls: 3 };
            let v = report_json("sid-1", Until::Done, budget, &rep, None);
            assert_eq!(v["outcome"], name);
            assert_eq!(v["ok"], ok);
            assert_eq!(v["sessionId"], "sid-1");
            assert_eq!(v["waitedMs"], 12);
            assert!(!v["reason"].as_str().unwrap().is_empty(), "причина есть у {name}");
            assert!(v.get("immediate").is_none(), "перехода ждали — не immediate");
        }
    }

    #[test]
    fn report_marks_immediate_and_carries_session_detail() {
        let mut s = Session::new("sid-1".into(), 0);
        s.status = Status::Done;
        s.detail = "Ответ готов".into();
        let rep = Report { outcome: Outcome::Reached(Status::Done), waited_ms: 0, polls: 1 };
        let v = report_json("sid-1", Until::Done, Duration::from_secs(60), &rep, Some(&s));
        assert_eq!(v["immediate"], true, "состояние было таким уже на первом опросе");
        assert_eq!(v["status"], "done");
        assert_eq!(v["detail"], "Ответ готов");
    }
}
