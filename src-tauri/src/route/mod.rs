//! Голосовая маршрутизация (под-проект 1). Оркестрация — в Rust: реплика →
//! детерминированный скоринг живых сессий → (узкий LLM-tie-break) → (пикер) →
//! stage-then-send с до-исполнительной отменой → видимый исход в HUD.
//!
//! Дизайн: docs/superpowers/specs/2026-06-26-voice-routing-design.md (ревизия 2).
//! Побочный эффект (`reply_core` → tmux paste+Enter) случается ТОЛЬКО после
//! истёкшего НЕотменённого stage-окна или тапа пикера — обе границы согласия
//! до-исполнительные.

pub mod hud;
pub mod pick;
pub mod score;
pub mod stage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::time::Duration;

use crate::daemon::Daemon;
use score::Decision;

/// Окно отмены уверенного роута (с) — успеешь отменить ДО tmux-пасты.
const STAGE_SECS: u32 = 5;
/// Таймаут ожидания выбора в пикере (с). Живёт здесь, не в гейте капабилити.
const PICK_TIMEOUT_SECS: u64 = 30;
/// Порог уверенности LLM-tie-break: ниже — в пикер (подключается в Stage 9).
const TIE_CONF: f32 = 0.75;

// ─── Single-flight: один голосовой цикл за раз ───────────────────────────────

/// Гард «голосовой цикл активен»: повторный wake во время цикла не плодит
/// второй захват/агента. Флаг снимается на Drop гарда — на ЛЮБОМ пути выхода.
#[derive(Default, Clone)]
pub struct SingleFlight(Arc<AtomicBool>);

pub struct SfGuard(Arc<AtomicBool>);

impl SingleFlight {
    /// Войти в цикл. None — если цикл уже идёт.
    pub fn try_enter(&self) -> Option<SfGuard> {
        if self.0.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(SfGuard(self.0.clone()))
        }
    }
}

impl Drop for SfGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ─── Чистое ветвление решения ────────────────────────────────────────────────

/// Что делать по итогу скоринга (+ опционального LLM-tie-break). Чистая, тестируемая.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Уверенно — стейджим и шлём в эту сессию.
    Send(String),
    /// Неоднозначно — показать пикер с этими кандидатами.
    Pick(Vec<String>),
    /// Нечего/некуда роутить.
    Nothing,
}

/// Свести решение скорера и результат LLM-tie-break в действие.
/// `tie` = Some((session_id, confidence)) от узкого вызова Клода (или None).
pub fn decide_action(decision: Decision, tie: Option<(String, f32)>) -> Action {
    match decision {
        Decision::Route(id) => Action::Send(id),
        Decision::Unknown => Action::Nothing,
        Decision::Ambiguous(cands) => match tie {
            Some((id, conf)) if conf >= TIE_CONF && cands.iter().any(|c| c == &id) => Action::Send(id),
            _ => Action::Pick(cands),
        },
    }
}

// ─── Оркестратор голосового цикла ────────────────────────────────────────────

/// Полный голосовой цикл после успешного STT. Зовётся из `on_wake` (в async).
/// Побочный эффект (`reply_core`) — только через `stage_and_send` (окно отмены)
/// или после тапа пикера; см. модель доверия в спеке.
pub async fn route_transcript(d: Arc<Daemon>, transcript: String) {
    let text = transcript.trim().to_string();
    if text.is_empty() {
        hud::emit(&d, hud::Phase::Empty);
        return;
    }
    hud::emit(&d, hud::Phase::Heard { text: text.clone() });

    let sessions = d.snapshot();
    let scored = score::rank(&text, &sessions);
    let decision = score::decide(&scored);

    // Stage 9 заменит `None` на узкий LLM-tie-break для ветки Ambiguous.
    let tie: Option<(String, f32)> = None;

    let label_of = |id: &str| -> String {
        scored.iter().find(|s| s.session_id == id).map(|s| s.label.clone()).unwrap_or_default()
    };

    match decide_action(decision, tie) {
        Action::Nothing => hud::emit(&d, hud::Phase::NoSessions),
        Action::Send(sid) => {
            let label = label_of(&sid);
            stage_and_send(d.clone(), sid, label, text);
        }
        Action::Pick(cands) => {
            let options: Vec<(String, String)> =
                cands.iter().map(|id| (id.clone(), label_of(id))).collect();
            if options.is_empty() {
                hud::emit(&d, hud::Phase::NoSessions);
                return;
            }
            let nonce = pick::gen_nonce();
            let rx = d.picks.register(nonce.clone());
            hud::emit(&d, hud::Phase::Picker { nonce: nonce.clone(), options });
            let chosen = match tokio::time::timeout(Duration::from_secs(PICK_TIMEOUT_SECS), rx).await {
                Ok(Ok(Some(sid))) => Some(sid),
                _ => {
                    d.picks.cancel(&nonce);
                    None
                }
            };
            match chosen {
                Some(sid) => {
                    let label = label_of(&sid);
                    stage_and_send(d.clone(), sid, label, text);
                }
                None => hud::emit(&d, hud::Phase::Cancelled),
            }
        }
    }
}

/// Стейдж текста в сессию с окном отмены; по истечении без отмены → `reply_core`
/// и эмит итога (доставлено / в очередь / ошибка) в HUD.
pub fn stage_and_send(d: Arc<Daemon>, session_id: String, label: String, text: String) {
    let nonce = pick::gen_nonce();
    hud::emit(
        &d,
        hud::Phase::Staged {
            nonce: nonce.clone(),
            label: label.clone(),
            text: text.clone(),
            secs: STAGE_SECS,
        },
    );
    let d2 = d.clone();
    d.stage.stage(
        nonce,
        session_id,
        text,
        Duration::from_secs(STAGE_SECS as u64),
        move |sid, txt| {
            // колбэк вне async — поднимаем таску для собственно отправки
            tauri::async_runtime::spawn(async move {
                let res = crate::ipc::reply_core(&d2, sid, txt).await;
                let ok = res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                if ok {
                    let queued = res.get("queued").and_then(|v| v.as_bool()).unwrap_or(false);
                    hud::emit(&d2, hud::Phase::Sent { label, queued });
                } else {
                    let msg = res
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("не доставлено")
                        .to_string();
                    hud::emit(&d2, hud::Phase::Error { msg });
                }
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_flight_blocks_reentry_and_releases_on_drop() {
        let sf = SingleFlight::default();
        let g = sf.try_enter().expect("первый вход");
        assert!(sf.try_enter().is_none(), "повторный вход заблокирован");
        drop(g);
        assert!(sf.try_enter().is_some(), "после drop снова можно");
    }

    #[test]
    fn route_decision_sends() {
        assert_eq!(decide_action(Decision::Route("a".into()), None), Action::Send("a".into()));
    }

    #[test]
    fn unknown_is_nothing() {
        assert_eq!(decide_action(Decision::Unknown, None), Action::Nothing);
    }

    #[test]
    fn ambiguous_without_tie_goes_to_picker() {
        let d = Decision::Ambiguous(vec!["a".into(), "b".into()]);
        assert_eq!(decide_action(d, None), Action::Pick(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn ambiguous_with_confident_tie_in_candidates_sends() {
        let d = Decision::Ambiguous(vec!["a".into(), "b".into()]);
        assert_eq!(decide_action(d, Some(("b".into(), 0.9))), Action::Send("b".into()));
    }

    #[test]
    fn ambiguous_with_low_confidence_tie_goes_to_picker() {
        let d = Decision::Ambiguous(vec!["a".into(), "b".into()]);
        assert_eq!(decide_action(d, Some(("b".into(), 0.5))), Action::Pick(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn ambiguous_with_tie_outside_candidates_ignored() {
        let d = Decision::Ambiguous(vec!["a".into(), "b".into()]);
        // LLM вернул id вне списка кандидатов → не доверяем, в пикер
        assert_eq!(decide_action(d, Some(("zzz".into(), 0.99))), Action::Pick(vec!["a".into(), "b".into()]));
    }
}

