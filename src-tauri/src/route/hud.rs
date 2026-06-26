//! Фазы голосового HUD и их эмиссия в окно `toast`. `hud_payload` — чистая
//! (тестируема); `emit` — тонкий. Все фазы рендерятся ОДНОЙ карточкой со
//! стабильным id `voice-hud`, обновляемой на месте (без мигания, UX-9).

use serde_json::{json, Value};

use crate::daemon::Daemon;

/// Стабильный id единственной HUD-карточки голосового цикла.
pub const HUD_ID: &str = "voice-hud";

/// Фаза голосового цикла. Несёт ровно то, что нужно UI для рендера.
pub enum Phase {
    /// Идёт запись реплики; `secs` — длина окна (для кольца отсчёта).
    Listening { secs: u32 },
    /// Идёт распознавание (анализ) реплики.
    Analyzing,
    /// Распознали реплику.
    Heard { text: String },
    /// Стейдж: отправлю в `label` через `secs` с, текст — `text`, отмена по `nonce`.
    Staged { nonce: String, label: String, text: String, secs: u32 },
    /// Доставлено (или поставлено в очередь, если `queued`).
    Sent { label: String, queued: bool },
    /// Пикер: выбери сессию. `options` = (session_id, label).
    Picker { nonce: String, options: Vec<(String, String)> },
    /// Отменено пользователем / по таймауту.
    Cancelled,
    /// Ошибка доставки/захвата/распознавания.
    Error { msg: String },
    /// Не расслышали (пустой STT).
    Empty,
    /// Нет живых сессий для маршрутизации.
    NoSessions,
}

/// Собрать payload фазы (чистая функция). Поле `phase` — дискриминатор для UI.
pub fn hud_payload(p: Phase) -> Value {
    let base = |phase: &str, title: &str, body: &str| {
        json!({ "id": HUD_ID, "kind": "voice", "phase": phase, "title": title, "body": body })
    };
    match p {
        Phase::Listening { secs } => {
            let mut v = base("listening", "Слушаю…", "");
            v["secs"] = json!(secs);
            v
        }
        Phase::Analyzing => base("analyzing", "Анализирую…", ""),
        Phase::Heard { text } => base("heard", "Услышал", &text),
        Phase::Staged { nonce, label, text, secs } => {
            let mut v = base("staged", "Отправлю", &text);
            v["nonce"] = json!(nonce);
            v["label"] = json!(label);
            v["secs"] = json!(secs);
            v
        }
        Phase::Sent { label, queued } => {
            let title = if queued { "В очередь" } else { "Отправлено" };
            let mut v = base("sent", title, &label);
            v["queued"] = json!(queued);
            v
        }
        Phase::Picker { nonce, options } => {
            let opts: Vec<Value> = options
                .into_iter()
                .map(|(id, label)| json!({ "sessionId": id, "label": label }))
                .collect();
            let mut v = base("picker", "В какую сессию?", "");
            v["nonce"] = json!(nonce);
            v["options"] = json!(opts);
            v
        }
        Phase::Cancelled => base("cancelled", "Отменено", ""),
        Phase::Error { msg } => base("error", "Ошибка", &msg),
        Phase::Empty => base("empty", "Не расслышал", "Скажи ещё раз"),
        Phase::NoSessions => base("nosessions", "Нет активных сессий", ""),
    }
}

/// Эмитировать фазу в окно `toast` (через буфер toast-событий — ранние фазы не
/// теряются, пока webview грузится).
pub fn emit(d: &Daemon, p: Phase) {
    crate::windows::hud_emit(d, hud_payload(p));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_payload_shape() {
        let v = hud_payload(Phase::Heard { text: "привет".into() });
        assert_eq!(v["id"], HUD_ID);
        assert_eq!(v["kind"], "voice");
        assert_eq!(v["phase"], "heard");
        assert_eq!(v["body"], "привет");
    }

    #[test]
    fn staged_payload_has_label_text_nonce_secs() {
        let v = hud_payload(Phase::Staged {
            nonce: "abc".into(),
            label: "frontend · build".into(),
            text: "почини".into(),
            secs: 5,
        });
        assert_eq!(v["phase"], "staged");
        assert_eq!(v["nonce"], "abc");
        assert_eq!(v["label"], "frontend · build");
        assert_eq!(v["body"], "почини");
        assert_eq!(v["secs"], 5);
    }

    #[test]
    fn picker_payload_carries_session_ids() {
        let v = hud_payload(Phase::Picker {
            nonce: "n".into(),
            options: vec![("sid-1".into(), "a".into()), ("sid-2".into(), "b".into())],
        });
        assert_eq!(v["phase"], "picker");
        assert_eq!(v["options"][0]["sessionId"], "sid-1");
        assert_eq!(v["options"][1]["label"], "b");
    }

    #[test]
    fn sent_queued_changes_title() {
        let a = hud_payload(Phase::Sent { label: "x".into(), queued: false });
        let b = hud_payload(Phase::Sent { label: "x".into(), queued: true });
        assert_eq!(a["title"], "Отправлено");
        assert_eq!(b["title"], "В очередь");
        assert_eq!(b["queued"], true);
    }
}
