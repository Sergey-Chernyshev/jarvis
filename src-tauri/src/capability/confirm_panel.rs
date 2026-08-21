//! PanelConfirmer (R4) — боевой confirmer агента: карточка в панель + ожидание
//! решения пользователя. Реестр pending — вне локов Daemon. Нонсы одноразовы и
//! не пересекаются с auth-токенами. На подтверждении — перепроверка цели
//! (INV-CONFIRM-BIND): подтверждаем КОНКРЕТНЫЙ эффект, а не намерение вообще.
//!
//! Карточка не истекает: человек мог отойти, а истёкшая карточка — тихая потеря
//! задачи. Взамен ожидание не немое — пока оно идёт, мы следим за целью (уехала
//! → Stale) и напоминаем человеку тостом (окно чата могли закрыть).

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::Duration;

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

    /// Снять ожидание (дроп будущего гейта, смерть демона) — без утечки записи.
    pub fn cancel(&self, nonce: &str) {
        self.map.lock().unwrap().remove(nonce);
    }
}

/// Шаг ожидания: как часто оглядываемся на цель и на часы.
const POLL: Duration = Duration::from_secs(15);
/// Первое напоминание — через минуту (столько человек идёт от чайника).
const FIRST_REMINDER: Duration = Duration::from_secs(60);
/// Дальше — раз в пять минут, пока не ответят. Карточка не истекает, но и
/// потеряться из виду не может: тихо ждать вечно — то же исчезновение задачи.
const REMINDER_EVERY: Duration = Duration::from_secs(300);

/// Пора ли напомнить о карточке, прождавшей `waited`. Чистая функция — расписание
/// напоминаний проверяемо без часов и без UI.
pub fn remind_due(waited: Duration) -> bool {
    let (w, first, every) =
        (waited.as_secs(), FIRST_REMINDER.as_secs(), REMINDER_EVERY.as_secs());
    w >= first && (w - first) % every == 0
}

/// Ждать решения человека без дедлайна, но не вслепую.
///
/// `fingerprint` зовём на каждом шаге: если цель уехала (сессию закрыли, ключ
/// сменили), спрашивать больше не о чем — это Stale, и сказать об этом надо
/// сразу, а не когда человек вернётся к мёртвой карточке. `tick` — «всё ещё ждём,
/// уже столько-то»: что с этим делать, решает вызывающий. На ответе цель
/// перепроверяется ещё раз (INV-CONFIRM-BIND): при долгом ожидании эта проверка
/// не менее важна, а более.
pub async fn await_decision(
    mut rx: oneshot::Receiver<bool>,
    poll: Duration,
    before: &str,
    mut fingerprint: impl FnMut() -> String + Send,
    mut tick: impl FnMut(Duration) + Send,
) -> Outcome {
    let mut waited = Duration::ZERO;
    loop {
        tokio::select! {
            answer = &mut rx => return Outcome::decide(answer.ok(), fingerprint() == before),
            _ = tokio::time::sleep(poll) => {
                waited += poll;
                if fingerprint() != before {
                    return Outcome::Stale;
                }
                tick(waited);
            }
        }
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

    const TICK: Duration = Duration::from_millis(10);

    /// Главное свойство: карточка не истекает сама. Ждём заведомо дольше любого
    /// прежнего дедлайна — ответ человека всё ещё принимается и всё ещё исполняется.
    #[tokio::test]
    async fn a_card_never_expires_on_its_own() {
        let p = PendingConfirms::new();
        let rx = p.register("n-slow".into());
        let waiting = tokio::spawn(async move {
            await_decision(rx, TICK, "цел", || "цел".into(), |_| {}).await
        });
        tokio::time::sleep(Duration::from_millis(120)).await; // ≫ шага ожидания
        assert!(!waiting.is_finished(), "вопрос закрылся сам — это и есть тихая потеря");
        assert!(p.resolve("n-slow", true), "нонс жив: ответ ещё принимают");
        assert_eq!(waiting.await.unwrap(), Outcome::Approved);
    }

    /// Пока ждём, цель может исчезнуть (сессию закрыли). Молчать нельзя: человеку
    /// незачем возвращаться к мёртвой карточке, а агенту нужен внятный исход.
    #[tokio::test]
    async fn a_vanished_target_ends_the_wait_out_loud() {
        let p = PendingConfirms::new();
        let rx = p.register("n-gone".into());
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = alive.clone();
        let waiting = tokio::spawn(async move {
            await_decision(
                rx,
                TICK,
                "сессия s1",
                move || {
                    if flag.load(std::sync::atomic::Ordering::SeqCst) { "сессия s1".into() }
                    else { "сессия ушла".to_string() }
                },
                |_| {},
            )
            .await
        });
        alive.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(waiting.await.unwrap(), Outcome::Stale, "исход обязан быть, и он не «одобрено»");
    }

    /// INV-CONFIRM-BIND: разрешение действует на ТУ цель, которую показали. При
    /// долгом ожидании это важнее, а не менее важно.
    #[tokio::test]
    async fn approval_binds_to_the_target_that_was_shown() {
        let p = PendingConfirms::new();
        let rx = p.register("n-bind".into());
        let waiting = tokio::spawn(async move {
            await_decision(rx, Duration::from_secs(3600), "было", || "стало".into(), |_| {}).await
        });
        assert!(p.resolve("n-bind", true));
        assert_eq!(waiting.await.unwrap(), Outcome::Stale, "цель подменилась — не исполняем");
    }

    /// Ожидание не немое: пока карточка ждёт, наверх идут тики — из них растёт
    /// напоминание человеку. Немое ожидание — то же исчезновение задачи, только
    /// растянутое.
    #[tokio::test]
    async fn waiting_is_never_mute() {
        let p = PendingConfirms::new();
        let rx = p.register("n-tick".into());
        let seen = Arc::new(Mutex::new(Vec::<Duration>::new()));
        let log = seen.clone();
        let waiting = tokio::spawn(async move {
            await_decision(rx, TICK, "цел", || "цел".into(), move |w| log.lock().unwrap().push(w))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        p.resolve("n-tick", false);
        assert_eq!(waiting.await.unwrap(), Outcome::Rejected);
        let ticks = seen.lock().unwrap().clone();
        assert!(ticks.len() >= 2, "ожидание молчало");
        assert!(ticks[1] > ticks[0], "тик несёт, сколько уже ждём");
    }

    /// Расписание напоминаний: минута, дальше раз в пять минут — и не чаще.
    #[test]
    fn reminders_are_rare_but_never_stop() {
        assert!(!remind_due(Duration::from_secs(15)));
        assert!(!remind_due(Duration::from_secs(45)));
        assert!(remind_due(FIRST_REMINDER));
        assert!(!remind_due(Duration::from_secs(120)));
        assert!(remind_due(FIRST_REMINDER + REMINDER_EVERY));
        assert!(remind_due(FIRST_REMINDER + REMINDER_EVERY * 12), "через час — всё ещё напоминаем");
        assert_eq!(waited_text(Duration::from_secs(60)), "1 мин");
        assert_eq!(waited_text(Duration::from_secs(3660)), "1 ч 1 мин");
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

/// Исход ожидания живёт рядом с трейтом (`confirm::Outcome`) — гейту он нужен
/// без панели. Реэкспорт: снаружи путь `confirm_panel::Outcome` остаётся прежним.
pub use super::confirm::Outcome;

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
    ) -> Pin<Box<dyn Future<Output = Outcome> + Send + 'a>> {
        Box::pin(async move {
            let nonce = gen_nonce();
            // снимок цели ДО ожидания (INV-CONFIRM-BIND)
            let before = target_fingerprint(&self.daemon, meta.id, args);
            let card = resolve_target(&self.daemon, meta.id, args);

            // Гарантированная очистка записи на любом выходе (вкл. дроп будущего
            // гейта) — и там же единственная точка, где UI узнаёт, что вопрос закрыт.
            // Именно в Drop, а не после await: дроп будущего — это как раз исход,
            // о котором иначе никто не сказал бы.
            struct Guard<'g> {
                pending: &'g PendingConfirms,
                app: &'g AppHandle,
                daemon: &'g Arc<Daemon>,
                nonce: String,
                outcome: std::cell::Cell<Outcome>,
            }
            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    self.pending.cancel(&self.nonce);
                    // напоминание пережило бы вопрос и врало бы «жду решения»
                    crate::windows::toast_remove(self.daemon, &reminder_id(&self.nonce));
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
                daemon: &self.daemon,
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

            // Ждём человека без дедлайна: карточка не про безопасность в моменте,
            // а про его решение. Перепроверка цели — и на ответе, и по дороге.
            let outcome = await_decision(
                rx,
                POLL,
                &before,
                || target_fingerprint(&self.daemon, meta.id, args),
                |waited| {
                    if remind_due(waited) {
                        remind(&self.daemon, &nonce, meta.id, waited);
                    }
                },
            )
            .await;
            guard.outcome.set(outcome);
            outcome
        })
    }
}

/// Стабильный id тоста-напоминания: одна карточка — одно напоминание, а не лента
/// одинаковых (повторный notify_id обновляет существующую).
fn reminder_id(nonce: &str) -> String {
    format!("confirm-{nonce}")
}

/// Напомнить, что вопрос ещё ждёт. Окно чата могли закрыть (это hide — карточка
/// жива и ждёт), панель могли не открывать вовсе; тост доходит в любом случае.
/// kind не «waiting»: голосом это читать незачем, это напоминание, а не событие.
fn remind(d: &Arc<Daemon>, nonce: &str, id: &str, waited: Duration) {
    d.notify_id(
        &reminder_id(nonce),
        "Жду вашего решения",
        &format!("{id} — карточка ждёт {}", waited_text(waited)),
        None,
        "confirm",
    );
}

/// «1 мин» / «5 мин» / «1 ч 5 мин» — человеку важно, сколько он уже не отвечает.
fn waited_text(waited: Duration) -> String {
    let m = waited.as_secs() / 60;
    match (m / 60, m % 60) {
        (0, m) => format!("{m} мин"),
        (h, 0) => format!("{h} ч"),
        (h, m) => format!("{h} ч {m} мин"),
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
