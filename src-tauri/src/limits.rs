//! Лимит провайдера — состояние аккаунта, не сессии: упёрлась одна — встали все.
//!
//! Сигнал: хук StopFailure (ход умер об API). Время сброса — официальное
//! (claude -p "/usage"). После сброса ждавшим tmux-сессиям шлём «продолжай»
//! со стаггером, чтобы не сжечь свежее окно залпом.

use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::daemon::Daemon;
use crate::model::Status;
use crate::util::{fmt_reset_in, now_ms};
use crate::{tmux, windows};

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LimitState {
    pub active: bool,
    pub kind: String,
    pub plan: String,
    pub reset_at: i64,
    pub since: i64,
}

pub struct Limits {
    state: Mutex<LimitState>,
    resume_timer: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl Limits {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(LimitState::default()),
            resume_timer: Mutex::new(None),
        }
    }

    pub fn state(&self) -> LimitState {
        self.state.lock().unwrap().clone()
    }
}

fn push_limit(d: &Arc<Daemon>) {
    windows::emit_to_panel(&d.app, "limit-state", &d.limits.state());
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
    let off = d.usage.official_info();
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
        .unwrap_or_else(|| now_ms() + 60 * 60_000);

    d.with_session(sid, |s| {
        s.status = Status::Limit;
        s.limit_wait = true;
        s.detail = format!("лимит использования · сброс через {}", fmt_reset_in(reset_at));
    });

    {
        let mut st = d.limits.state.lock().unwrap();
        st.active = true;
        st.kind = "rate_limit".into();
        st.plan = plan.clone();
        st.reset_at = reset_at;
        st.since = now_ms();
    }
    push_limit(d);
    d.usage.refresh_official_soon(d);
    schedule_auto_resume(d);

    let auto = d.settings.bool("autoResume");
    d.notify(
        &format!("Claude{} — лимит использования", if plan.is_empty() { String::new() } else { format!(" {plan}") }),
        &format!(
            "Сброс через {} · {project} {}",
            fmt_reset_in(reset_at),
            if auto { "— продолжу сам" } else { "ждёт" }
        ),
        Some(sid),
        "limit",
    );
    println!("[jarvis] stop-failure ({project}): подтверждённый лимит (sessPct={sess_pct:?})");
    // голос лимита идёт сам через notify() выше (kind="limit") — отдельно не дублируем
    d.push();
}

/// Самоисцеление баннера: официальный usage упал ниже 80% или окно сброшено.
pub fn reconcile(d: &Arc<Daemon>) {
    let (active, reset_at) = {
        let st = d.limits.state.lock().unwrap();
        (st.active, st.reset_at)
    };
    if !active {
        return;
    }
    let pct = d
        .usage
        .official_info()
        .and_then(|o| o.session.map(|s| s.pct));
    let expired = reset_at > 0 && now_ms() > reset_at;
    if pct.is_some_and(|p| p < 80) || expired {
        d.limits.state.lock().unwrap().active = false;
        {
            let mut sessions = d.sessions.lock().unwrap();
            for s in sessions.values_mut() {
                if s.status == Status::Limit {
                    s.status = Status::Idle;
                    s.limit_wait = false;
                }
            }
        }
        push_limit(d);
        d.push();
        println!("[jarvis] лимит-баннер снят (usage упал / окно сброшено)");
    }
}

pub fn schedule_auto_resume(d: &Arc<Daemon>) {
    let mut timer = d.limits.resume_timer.lock().unwrap();
    if let Some(h) = timer.take() {
        h.abort();
    }
    if !d.settings.bool("autoResume") {
        return;
    }
    let reset_at = d.limits.state.lock().unwrap().reset_at;
    // +90с джиттера после сброса; не раньше 10с и не позже 6ч
    let delay = ((reset_at - now_ms() + 90_000).max(10_000) as u64).min(6 * 3600_000);
    println!("[jarvis] авто-продолжение через {} мин", delay / 60_000);
    let d2 = d.clone();
    *timer = Some(tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay)).await;
        run_auto_resume(&d2).await;
    }));
}

async fn run_auto_resume(d: &Arc<Daemon>) {
    d.limits.state.lock().unwrap().active = false;
    push_limit(d);
    let waiters: Vec<(String, String, String)> = d
        .sessions
        .lock()
        .unwrap()
        .values()
        .filter(|s| s.limit_wait && s.tmux_pane.is_some())
        .map(|s| {
            (
                s.id.clone(),
                s.tmux_pane.clone().unwrap(),
                s.project.clone().unwrap_or_else(|| "?".into()),
            )
        })
        .collect();

    for (i, (sid, pane, project)) in waiters.iter().cloned().enumerate() {
        let d2 = d.clone();
        tauri::async_runtime::spawn(async move {
            // стаггер 2 мин — не сжигаем свежее окно залпом
            tokio::time::sleep(Duration::from_millis(i as u64 * 120_000)).await;
            let still_waiting = d2.session(&sid).is_some_and(|s| s.limit_wait);
            if !still_waiting || !tmux::pane_alive(&pane).await {
                return;
            }
            if tmux::reply(&pane, "продолжай").await.is_ok() {
                d2.with_session(&sid, |s| s.limit_wait = false);
                d2.mark_prompt_sent(&sid, "продолжай (авто после сброса лимита)");
                println!("[jarvis] авто-продолжил {project}");
            }
        });
    }
    if !waiters.is_empty() {
        let n = waiters.len();
        d.notify(
            "Claude — лимит сброшен",
            &format!("Продолжаю {n} {}", if n == 1 { "сессию" } else { "сессии" }),
            None,
            "done",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classification_is_conservative() {
        assert_eq!(classify_failure(&json!({"error": "Rate limit reached"})), "rate_limit");
        assert_eq!(classify_failure(&json!({"error": "429 too many"})), "rate_limit");
        assert_eq!(classify_failure(&json!({"error": "insufficient credit"})), "billing");
        assert_eq!(classify_failure(&json!({"error": "529 overloaded"})), "overloaded");
        assert_eq!(classify_failure(&json!({"error": "connection reset"})), "transient");
    }
}
