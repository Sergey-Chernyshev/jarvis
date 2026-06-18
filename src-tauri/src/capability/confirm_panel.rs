//! PanelConfirmer (R4) — боевой confirmer агента: карточка в панель + ожидание
//! решения пользователя. Реестр pending — вне локов Daemon. Нонсы одноразовы и
//! не пересекаются с auth-токенами. На подтверждении — перепроверка цели
//! (INV-CONFIRM-BIND): подтверждаем КОНКРЕТНЫЙ эффект, а не намерение вообще.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;

use tokio::sync::oneshot;

/// Реестр ожидающих подтверждений: nonce → отправитель ответа. Отдельная
/// структура (не в мьютексах Daemon) — гейт не держит локов, пока ждёт юзера.
pub struct PendingConfirms {
    map: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl Default for PendingConfirms {
    fn default() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }
}

impl PendingConfirms {
    pub fn new() -> Self {
        Self::default()
    }

    /// Зарегистрировать ожидание; вернуть приёмник ответа.
    pub fn register(&self, nonce: String) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.map.lock().unwrap().insert(nonce, tx);
        rx
    }

    /// Разрешить ожидание (одноразово: запись удаляется). true — если nonce был.
    pub fn resolve(&self, nonce: &str, approved: bool) -> bool {
        if let Some(tx) = self.map.lock().unwrap().remove(nonce) {
            let _ = tx.send(approved);
            true
        } else {
            false
        }
    }

    /// Снять ожидание (на таймауте/дропе будущего гейта) — без утечки записи.
    pub fn cancel(&self, nonce: &str) {
        self.map.lock().unwrap().remove(nonce);
    }
}

/// Нонс подтверждения: 16 байт /dev/urandom → hex. НЕ auth-токен.
pub fn gen_nonce() -> String {
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_delivers_and_is_single_use() {
        let p = PendingConfirms::new();
        let rx = p.register("n1".into());
        assert!(p.resolve("n1", true), "первый резолв проходит");
        assert_eq!(rx.await.unwrap(), true);
        assert!(!p.resolve("n1", true), "повтор того же nonce — нет записи");
    }

    #[test]
    fn unknown_nonce_resolves_false() {
        let p = PendingConfirms::new();
        assert!(!p.resolve("nope", true));
    }

    #[test]
    fn nonce_is_unique_and_hex() {
        let a = gen_nonce();
        let b = gen_nonce();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}

// ─── PanelConfirmer ───────────────────────────────────────────────────────────

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use super::confirm::Confirmer;
use super::contract::CapabilityMeta;
use crate::daemon::Daemon;

/// Боевой confirmer: рисует карточку в панели и ждёт `agent_confirm` из UI.
pub struct PanelConfirmer {
    pub app: AppHandle,
    pub pending: Arc<PendingConfirms>,
    pub daemon: Arc<Daemon>,
}

impl Confirmer for PanelConfirmer {
    fn confirm<'a>(
        &'a self,
        meta: &'a CapabilityMeta,
        args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            let nonce = gen_nonce();
            // снимок цели ДО ожидания (INV-CONFIRM-BIND)
            let before = target_fingerprint(&self.daemon, meta.id, args);
            let card = resolve_target(&self.daemon, meta.id, args);

            // гарантированная очистка записи на любом выходе (вкл. дроп по таймауту гейта)
            struct Guard<'g> { pending: &'g PendingConfirms, nonce: String }
            impl Drop for Guard<'_> {
                fn drop(&mut self) { self.pending.cancel(&self.nonce); }
            }
            let _guard = Guard { pending: &self.pending, nonce: nonce.clone() };

            let rx = self.pending.register(nonce.clone());
            let _ = self.app.emit_to(
                "main",
                "agent:confirm",
                json!({
                    "nonce": nonce,
                    "id": meta.id,
                    "class": meta.class.as_str(),
                    "provenance": meta.provenance.as_str(),
                    "card": card,
                }),
            );

            let approved = rx.await.unwrap_or(false);
            if !approved {
                return false;
            }
            // перепроверка цели: если сменилась, пока ждали — НЕ исполняем
            target_fingerprint(&self.daemon, meta.id, args) == before
        })
    }
}

/// Человекочитаемая карточка цели для UI. Резолвит UUID сессии в метку проекта,
/// settings.set — в дифф ключей. НИКОГДА не отдаёт сырой UUID без метки.
pub fn resolve_target(d: &Arc<Daemon>, id: &str, args: &Value) -> Value {
    match id {
        "sessions.reply" | "sessions.control" => {
            let sid = args.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
            json!({
                "kind": "session",
                "label": d.session_label(sid),
                "text": args.get("text").and_then(|v| v.as_str())
                    .map(|t| crate::util::ellipsize(t, 160)),
                "model": args.get("model"),
                "effort": args.get("effort"),
            })
        }
        "settings.set" => json!({ "kind": "settings", "diff": settings_diff(d, args) }),
        _ => json!({ "kind": "other", "args": args }),
    }
}

/// Дифф ключей patch против текущих значений (что станет из чего).
fn settings_diff(d: &Arc<Daemon>, args: &Value) -> Value {
    let cur = d.settings.load();
    let patch = args.get("patch").and_then(|p| p.as_object());
    let mut out = serde_json::Map::new();
    if let Some(p) = patch {
        for (k, v) in p {
            out.insert(k.clone(), json!({ "from": cur.get(k).cloned(), "to": v.clone() }));
        }
    }
    Value::Object(out)
}

/// Стабильный отпечаток цели — меняется, если цель «уехала» за время ожидания.
/// Для сессии — её идентичность (метка), для settings — текущие значения ключей.
pub fn target_fingerprint(d: &Arc<Daemon>, id: &str, args: &Value) -> String {
    match id {
        "sessions.reply" | "sessions.control" => {
            let sid = args.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
            format!("{sid}|{}", d.session_label(sid))
        }
        "settings.set" => {
            let cur = d.settings.load();
            let mut keys: Vec<String> = args
                .get("patch")
                .and_then(|p| p.as_object())
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            keys.sort();
            keys.iter()
                .map(|k| format!("{k}={}", cur.get(k).cloned().unwrap_or(Value::Null)))
                .collect::<Vec<_>>()
                .join("|")
        }
        _ => String::new(),
    }
}
