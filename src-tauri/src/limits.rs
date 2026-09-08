//! Лимит провайдера — состояние аккаунта, не сессии: упёрлась одна — встали все.
//!
//! Сигнал: хук StopFailure (ход умер об API). Время сброса — официальное
//! (claude -p "/usage"). После сброса ждавшим tmux-сессиям шлём «продолжай»
//! со стаггером, чтобы не сжечь свежее окно залпом.

use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use crate::daemon::Daemon;
use crate::model::{Session, Status};
use std::collections::HashMap;
use crate::util::{fmt_reset_in, now_ms};
use crate::windows;

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LimitState {
    pub active: bool,
    pub kind: String,
    pub plan: String,
    pub reset_at: i64,
    pub since: i64,
    pub session_id: Option<String>,
}

pub struct Limits {
    states: Mutex<HashMap<String, (LimitState, String)>>,
    resume_timers: Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>,
    generation: AtomicI64,
}

impl Limits {
    pub fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            resume_timers: Mutex::new(HashMap::new()),
            generation: AtomicI64::new(0),
        }
    }

    pub fn state(&self) -> LimitState {
        self.states.lock().unwrap().values().filter(|(state, _)| state.active)
            .min_by_key(|(state, _)| if state.reset_at > 0 { state.reset_at } else { i64::MAX })
            .map(|(state, _)| state.clone()).unwrap_or_default()
    }

    fn next_generation(&self, now: i64) -> i64 {
        // Removing a completed scope must not let a same-millisecond failure
        // reuse its old generation, even after a wall-clock correction.
        let previous = self.generation.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |previous| Some(now.max(previous.saturating_add(1)))).unwrap();
        now.max(previous.saturating_add(1))
    }
}

// A failure and its retry belong to a provider account on one machine.
// Unknown remote homes remain session-scoped rather than merging accounts.
fn account_scope(s: &Session) -> String {
    let agent = s.agent.as_deref().unwrap_or("claude");
    let machine = s.remote.as_deref().unwrap_or("local");
    let account = s.instance_id.as_deref().or(s.provider_home.as_deref())
        .unwrap_or(&s.id);
    serde_json::to_string(&(agent, machine, account)).unwrap()
}

pub fn snapshot(d: &Arc<Daemon>) -> Value {
    let state = d.limits.state();
    let session = state.session_id.as_deref().and_then(|sid| d.session(sid));
    let mut value = serde_json::to_value(&state).unwrap_or(Value::Null);
    value["sourceLabel"] = serde_json::json!(session.as_ref().map(|s| {
        let provider = if s.agent.as_deref() == Some("codex") { "Codex" } else { "Claude" };
        let label = s.instance_label.as_deref().unwrap_or(provider);
        s.remote.as_ref().map_or_else(|| label.to_string(), |machine| format!("{label} · {machine}"))
    }));
    value["autoResume"] = serde_json::json!(d.settings.bool("autoResume") && state.reset_at > 0 && session.is_some_and(|s| s.tmux_pane.is_some()));
    value["profileCount"] = serde_json::json!(d.limits.states.lock().unwrap().len());
    value
}

fn push_limit(d: &Arc<Daemon>) {
    windows::emit_to_panel(&d.app, "limit-state", &snapshot(d));
}

/// Точная классификация StopFailure: НЕ дефолтим в rate_limit (перегрузка/сбой
/// сети — частые и НЕ лимит аккаунта). Аккаунтный баннер — только при явном
/// rate-limit И подтверждении официальным usage.
pub fn classify_failure(payload: &Value) -> &'static str {
    let raw = serde_json::to_string(payload).unwrap_or_default().to_lowercase();
    let hit = |p: &str| regex::Regex::new(p).unwrap().is_match(&raw);
    if hit(r"billing|payment|insufficient|credit") {
        "billing"
    } else if hit(r"rate.?limit|usage limit|quota|429|limit reached|limit_exceeded") {
        "rate_limit"
    } else if hit(r"overload|503|529|capacity") {
        "overloaded"
    } else {
        "transient" // неизвестная ошибка хода — НЕ лимит
    }
}

pub fn on_stop_failure(d: &Arc<Daemon>, sid: &str, payload: &Value) {
    let kind = classify_failure(payload);
    let Some(session) = d.session(sid) else { return };
    let scope = account_scope(&session);
    let off = d.usage.official_info_for_session(&session);
    let plan = off
        .as_ref()
        .map(|o| o.account.plan.clone().unwrap_or_default())
        .unwrap_or_default();
    let sess_pct = off.as_ref().and_then(|o| o.session.as_ref()).map(|s| s.pct);

    // аккаунтный лимит подтверждаем официальным usage: если /usage знает и
    // показывает <85% — это НЕ упирание в стену, а транзиентный сбой
    let real_limit = kind == "rate_limit" && sess_pct.map_or(true, |p| p >= 85);

    let project = d
        .session(sid)
        .and_then(|s| s.project)
        .unwrap_or_else(|| "?".into());

    if !real_limit {
        // транзиент: помечаем только сессию, без аккаунтного баннера и авто-резюма
        d.with_session(sid, |s| {
            s.status = Status::Idle;
            s.detail = match kind {
                "overloaded" => "API перегружен — попробуй ещё раз",
                "billing" => "ошибка биллинга",
                _ => "ход прервался ошибкой",
            }
            .into();
        });
        println!("[jarvis] stop-failure ({project}): {kind}, sessPct={sess_pct:?} → транзиент, баннер не показываю");
        d.push();
        return;
    }

    let reset_at = off
        .as_ref()
        .and_then(|o| o.session.as_ref())
        .map(|s| s.reset_at)
        .filter(|&t| t > 0)
        .unwrap_or(0);

    {
        let mut states = d.limits.states.lock().unwrap();
        let generation = d.limits.next_generation(now_ms());
        states.insert(scope, (LimitState {
            active: true, kind: "rate_limit".into(), plan: plan.clone(), reset_at, since: generation, session_id: Some(sid.into()),
        }, sid.into()));
        d.with_session(sid, |s| {
            s.status = Status::Limit; s.limit_wait = true;
            s.detail = if reset_at > 0 { format!("лимит использования · сброс через {}", fmt_reset_in(reset_at)) }
                else { "лимит использования · время сброса пока неизвестно".into() };
        });
    }
    push_limit(d);
    d.usage.refresh_official_soon(d);
    schedule_auto_resume(d);

    let auto = d.settings.bool("autoResume") && reset_at > 0 && session.tmux_pane.is_some();
    d.notify(
        &format!("{}{} — лимит использования", if session.agent.as_deref() == Some("codex") { "Codex" } else { "Claude" }, if plan.is_empty() { String::new() } else { format!(" {plan}") }),
        &format!(
            "{} · {project} {}",
            if reset_at > 0 { format!("Сброс через {}", fmt_reset_in(reset_at)) } else { "Время сброса неизвестно".into() },
            if auto { "— продолжу сам" } else { "ждёт" }
        ),
        Some(sid),
        "limit",
    );
    println!("[jarvis] stop-failure ({project}): подтверждённый лимит (sessPct={sess_pct:?})");
    // голос лимита идёт сам через notify() выше (kind="limit") — отдельно не дублируем
    d.push();
}

/// Only official usage for this same provider/account may clear its limit.
pub fn reconcile(d: &Arc<Daemon>) {
    let states = d.limits.states.lock().unwrap().clone();
    for (scope, (state, sid)) in states {
        if !state.active { continue; }
        let Some(session) = d.session(&sid) else { continue };
        if account_scope(&session) != scope { continue; }
        let official = d.usage.official_info_for_session(&session);
        let pct = official.as_ref().and_then(|o| o.session.as_ref()).map(|s| s.pct);
        if pct.is_some_and(|p| p < 80) || (state.reset_at > 0 && now_ms() > state.reset_at) {
            // Keep waiting sessions until their scoped retry consumes them.
            if d.settings.bool("autoResume") {
                if let Some((current, _)) = d.limits.states.lock().unwrap().get_mut(&scope) {
                    reconcile_reset(current, state.since, now_ms(), true);
                }
                schedule_auto_resume(d);
            } else {
                clear_scope(d, &scope, state.since);
            }
        } else if let Some(reset_at) = official.and_then(|o| o.session).map(|s| s.reset_at).filter(|&t| t > now_ms()) {
            if let Some((current, _)) = d.limits.states.lock().unwrap().get_mut(&scope) {
                reconcile_reset(current, state.since, reset_at, false);
            }
            schedule_auto_resume(d);
        }
    }
}

/// Compare against the snapshot generation before applying a usage response.
/// Repeated low-usage polls must not keep moving an already elapsed reset.
fn reconcile_reset(state: &mut LimitState, generation: i64, reset_at: i64, ready: bool) -> bool {
    if !state.active || state.since != generation { return false; }
    if !ready || state.reset_at <= 0 || state.reset_at > reset_at { state.reset_at = reset_at; }
    true
}

fn clear_scope(d: &Arc<Daemon>, scope: &str, generation: i64) {
    let mut states = d.limits.states.lock().unwrap();
    if !states.get(scope).is_some_and(|(state, _)| state.since == generation) { return; }
    states.remove(scope);
    for s in d.sessions.lock().unwrap().values_mut().filter(|s| account_scope(s) == scope) {
        if s.status == Status::Limit { s.status = Status::Idle; }
        s.limit_wait = false;
    }
    drop(states); push_limit(d); d.push();
}

pub fn schedule_auto_resume(d: &Arc<Daemon>) {
    let mut timers = d.limits.resume_timers.lock().unwrap();
    if !d.settings.bool("autoResume") {
        for (_, timer) in timers.drain() { timer.abort(); }
        return;
    }
    for (scope, (state, _)) in d.limits.states.lock().unwrap().iter() {
        if !state.active || state.reset_at <= 0 || timers.contains_key(scope) { continue; }
        let scope = scope.clone(); let d = d.clone();
        let task_scope = scope.clone();
        timers.insert(scope, tauri::async_runtime::spawn(async move {
            // Recheck official reset changes and settings instead of firing
            // an obsolete timer against whichever account currently appears.
            loop {
                // Only schedule_auto_resume owns cancellation/removal. A
                // worker exiting on a temporarily absent state/off setting
                // could remove a replacement timer installed concurrently.
                if !d.settings.bool("autoResume") { tokio::time::sleep(Duration::from_secs(30)).await; continue; }
                let state = d.limits.states.lock().unwrap().get(&task_scope).cloned();
                let Some((state, _)) = state.filter(|(state,_)| state.active) else {
                    tokio::time::sleep(Duration::from_secs(30)).await; continue;
                };
                if state.reset_at <= 0 { tokio::time::sleep(Duration::from_secs(30)).await; continue; }
                let remaining = state.reset_at + 90_000 - now_ms();
                if remaining <= 0 {
                    run_auto_resume(&d, &task_scope, state.since).await;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(remaining.min(30_000) as u64)).await;
            }
        }));
    }
}

fn retry_is_current(state: &LimitState, generation: i64, now: i64) -> bool {
    state.active && state.since == generation && state.reset_at > 0 && state.reset_at <= now
}

async fn run_auto_resume(d: &Arc<Daemon>, scope: &str, generation: i64) {
    let waiters: Vec<_> = d.snapshot().into_iter()
        .filter(|s| s.limit_wait && s.tmux_pane.is_some() && account_scope(s) == scope)
        .map(|s| s.id).collect();
    for (i, sid) in waiters.iter().enumerate() {
        if i > 0 { tokio::time::sleep(Duration::from_secs(120)).await; }
        if !d.settings.bool("autoResume") { return; }
        let Some(s) = d.session(sid).filter(|s| s.limit_wait && account_scope(s) == scope) else { continue };
        // A newly reported limit must postpone this old retry batch.
        if !d.limits.states.lock().unwrap().get(scope).is_some_and(|(state, _)| retry_is_current(state, generation, now_ms())) { return; }
        let Some(pane) = s.tmux_pane.as_deref() else { continue };
        let Ok(target) = d.pane_target(&s) else { continue };
        if !target.pane_alive(pane).await { continue; }
        if !d.settings.bool("autoResume") { return; }
        // Claim before sending; uncertain transport delivery must not replay.
        {
            let states = d.limits.states.lock().unwrap();
            if !states.get(scope).is_some_and(|(state, _)| retry_is_current(state, generation, now_ms())) { return; }
            let mut claimed = false;
            d.with_session(sid, |s| {
                if s.limit_wait && account_scope(s) == scope && s.tmux_pane.as_deref() == Some(pane) {
                    s.limit_wait = false; claimed = true;
                }
            });
            if !claimed { continue; }
        }
        let result = target.reply(pane, "продолжай").await;
        let states = d.limits.states.lock().unwrap();
        if !states.get(scope).is_some_and(|(state, _)| state.since == generation) { return; }
        match result {
            Ok(()) => d.mark_prompt_sent(sid, "продолжай (авто после сброса лимита)"),
            Err(error) => { d.with_session(sid, |s| {
                s.status = Status::Idle;
                s.detail = format!("Автопродолжение не подтверждено: {error}. Проверь чат.");
            }); },
        }
    }
    if !d.snapshot().iter().any(|s| s.limit_wait && s.tmux_pane.is_some() && account_scope(s) == scope) {
        clear_scope(d, scope, generation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retries_never_cross_provider_machine_or_account() {
        let mut a = Session::new("one".into(), 1);
        a.agent = Some("codex".into()); a.instance_id = Some("personal".into());
        let mut b = a.clone(); b.id = "two".into();
        assert_eq!(account_scope(&a), account_scope(&b));
        b.instance_id = Some("work".into()); assert_ne!(account_scope(&a), account_scope(&b));
        b = a.clone(); b.remote = Some("vm".into()); assert_ne!(account_scope(&a), account_scope(&b));
        b = a.clone(); b.agent = Some("claude".into()); assert_ne!(account_scope(&a), account_scope(&b));
        a.remote = Some("vm".into()); a.instance_id = None;
        b = a.clone(); b.id = "unknown-other-account".into(); assert_ne!(account_scope(&a), account_scope(&b));
    }

    #[test]
    fn an_old_retry_cannot_consume_a_new_limit_even_without_a_reset_time() {
        let mut state = LimitState { active:true, since:7, reset_at:100, ..Default::default() };
        assert!(retry_is_current(&state, 7, 200));
        state.since = 8; assert!(!retry_is_current(&state, 7, 200));
        state.reset_at = 0; assert!(!retry_is_current(&state, 8, 200));
        state.reset_at = 300; assert!(!retry_is_current(&state, 8, 200));
    }

    #[test]
    fn stale_usage_snapshot_cannot_clear_or_reschedule_a_new_generation() {
        let mut newer = LimitState { active:true, since:8, reset_at:900, ..Default::default() };
        assert!(!reconcile_reset(&mut newer,7,100,true));
        assert!(!reconcile_reset(&mut newer,7,500,false));
        assert_eq!(newer.reset_at,900);
        assert!(reconcile_reset(&mut newer,8,100,true));
        assert!(reconcile_reset(&mut newer,8,110,true));
        assert_eq!(newer.reset_at,100,"repeated usage polls must not starve the ready timer");
    }

    #[test]
    fn generations_do_not_reuse_a_removed_scope_or_rewind_with_the_clock() {
        let limits = Limits::new();
        let first = limits.next_generation(100);
        let second = limits.next_generation(100);
        let after_clock_change = limits.next_generation(99);
        assert!(first < second && second < after_clock_change);
    }

    #[test]
    fn classification_is_conservative() {
        assert_eq!(classify_failure(&json!({"error": "Rate limit reached"})), "rate_limit");
        assert_eq!(classify_failure(&json!({"error": "429 too many"})), "rate_limit");
        assert_eq!(classify_failure(&json!({"error": "insufficient credit"})), "billing");
        assert_eq!(classify_failure(&json!({"error": "529 overloaded"})), "overloaded");
        assert_eq!(classify_failure(&json!({"error": "connection reset"})), "transient");
    }
}
