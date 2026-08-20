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
    // Fail-closed: пустой/нечитаемый /dev/urandom оставил бы буфер нулями →
    // константный nonce (ломает одноразовость). Паникуем, а не используем нули.
    let mut buf = [0u8; 16];
    let mut f = std::fs::File::open("/dev/urandom").expect("/dev/urandom недоступен");
    f.read_exact(&mut buf).expect("/dev/urandom не читается");
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
    fn outcome_covers_every_exit() {
        use super::Outcome::*;
        assert_eq!(Outcome::decide(None, false), Expired, "отправителя не стало — решения не было");
        assert_eq!(Outcome::decide(None, true), Expired, "цель цела, но решения всё равно нет");
        assert_eq!(Outcome::decide(Some(false), true), Rejected);
        assert_eq!(Outcome::decide(Some(true), false), Stale, "разрешили, но цель уехала");
        assert_eq!(Outcome::decide(Some(true), true), Approved);
        // исполняет ровно один исход — остальные три обязаны быть отказом
        for o in [Expired, Rejected, Stale] {
            assert!(!o.allows(), "{} не должен исполняться", o.as_str());
        }
        assert!(Approved.allows());
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

/// Чем кончилось ожидание карточки. Нужен наружу: UI обязан снять кнопки в ЛЮБОМ
/// исходе, а не только когда человек нажал. Молчание на таймауте оставляло карточку
/// висеть с обещанием выбора, которого уже нет.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Решения не было: таймаут гейта, дроп будущего, снятый nonce.
    Expired,
    Rejected,
    /// Разрешено, но цель уехала за время ожидания (INV-CONFIRM-BIND) — НЕ исполнено.
    Stale,
    Approved,
}

impl Outcome {
    /// Чистое ядро решения: что пришло из реестра + совпал ли отпечаток цели.
    /// `recv: None` — отправителя не стало, то есть решения не было.
    pub fn decide(recv: Option<bool>, same_target: bool) -> Self {
        match recv {
            None => Outcome::Expired,
            Some(false) => Outcome::Rejected,
            Some(true) if same_target => Outcome::Approved,
            Some(true) => Outcome::Stale,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Expired => "expired",
            Outcome::Rejected => "rejected",
            Outcome::Stale => "stale",
            Outcome::Approved => "approved",
        }
    }

    /// Исполнять ли вызов. Всё, кроме `Approved`, — нет.
    pub fn allows(self) -> bool {
        self == Outcome::Approved
    }
}

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

            // Гарантированная очистка записи на любом выходе (вкл. дроп по таймауту
            // гейта) — и там же единственная точка, где UI узнаёт, что вопрос закрыт.
            // Именно в Drop, а не после await: дроп будущего — это как раз исход,
            // о котором иначе никто не сказал бы.
            struct Guard<'g> {
                pending: &'g PendingConfirms,
                app: &'g AppHandle,
                nonce: String,
                outcome: std::cell::Cell<Outcome>,
            }
            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    self.pending.cancel(&self.nonce);
                    let o = self.outcome.get();
                    let _ = self.app.emit(
                        "agent:confirm-done",
                        json!({ "nonce": self.nonce, "approved": o.allows(), "outcome": o.as_str() }),
                    );
                }
            }
            let guard = Guard {
                pending: &self.pending,
                app: &self.app,
                nonce: nonce.clone(),
                outcome: std::cell::Cell::new(Outcome::Expired),
            };

            let rx = self.pending.register(nonce.clone());
            // глобально — карточку ловит окно чата агента (agent-chat), а не только панель
            let _ = self.app.emit(
                "agent:confirm",
                json!({
                    "nonce": nonce,
                    "id": meta.id,
                    "class": meta.class.as_str(),
                    "provenance": meta.provenance.as_str(),
                    "card": card,
                }),
            );

            let recv = rx.await.ok();
            // перепроверка цели: если сменилась, пока ждали — НЕ исполняем
            let same = recv == Some(true)
                && target_fingerprint(&self.daemon, meta.id, args) == before;
            let outcome = Outcome::decide(recv, same);
            guard.outcome.set(outcome);
            outcome.allows()
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
        // Запуск подтверждают не по сырым аргументам: человек решает по тому,
        // что именно поднимут, где и под какую работу.
        "sessions.spawn" => json!({
            "kind": "spawn",
            "agent": args.get("agent"),
            "label": args.get("name"),
            "cwd": args.get("cwd").and_then(|v| v.as_str()).map(crate::util::short_home),
            "text": args.get("task").and_then(|v| v.as_str())
                .map(|t| crate::util::ellipsize(t, 160)),
            "model": args.get("model"),
            "parent": args.get("parent"),
            "isolate": args.get("isolate"),
        }),
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
