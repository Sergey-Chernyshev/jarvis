//! Подтверждение side-effect (§7). Абстрагировано за трейтом, чтобы ядро
//! тестировалось без живого UI: тесты подставляют `AutoApprove`/`AutoDeny`,
//! а реальный путь — `PanelConfirmer` (фаза 5), который рисует карточку в
//! панели и ждёт решения пользователя.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use super::contract::{CapabilityMeta, GateError};

/// Чем кончилось ожидание карточки. Нужен наружу: UI обязан снять кнопки в ЛЮБОМ
/// исходе, а не только когда человек нажал. Молчание на таймауте оставляло карточку
/// висеть с обещанием выбора, которого уже нет.
///
/// Живёт рядом с трейтом, а не в панельной проекции: «отказал» и «не ответил» —
/// разные ответы агенту, и различить их обязан гейт, а не UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Решения не было: снятый nonce, дроп будущего, смерть демона.
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

    /// Отказ в терминах агента. Разница между «человек отказал» и «человек не
    /// ответил» для него не косметика: на отказе он ищет другой путь, на
    /// молчании — обязан спросить человека словами, а не решать за него.
    pub fn gate_error(self) -> GateError {
        match self {
            Outcome::Rejected => GateError::Rejected,
            Outcome::Stale => GateError::Stale,
            _ => GateError::Expired,
        }
    }
}

/// Спрашивает человека и возвращает исход. Ответа ждёт столько, сколько нужно:
/// вопрос не про безопасность в моменте, а про решение человека, а человек
/// может отойти.
pub trait Confirmer: Send + Sync {
    fn confirm<'a>(
        &'a self,
        meta: &'a CapabilityMeta,
        args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Outcome> + Send + 'a>>;
}

/// Авто-подтверждение — для тестов и для гранта панели (пользователь сам нажал).
pub struct AutoApprove;
impl Confirmer for AutoApprove {
    fn confirm<'a>(
        &'a self,
        _meta: &'a CapabilityMeta,
        _args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Outcome> + Send + 'a>> {
        Box::pin(async { Outcome::Approved })
    }
}

/// Авто-отказ — для тестов «запутанного помощника».
pub struct AutoDeny;
impl Confirmer for AutoDeny {
    fn confirm<'a>(
        &'a self,
        _meta: &'a CapabilityMeta,
        _args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Outcome> + Send + 'a>> {
        Box::pin(async { Outcome::Rejected })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_covers_every_exit() {
        use Outcome::*;
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

    /// Агент обязан отличить «человек отказал» от «человек не ответил»: иначе на
    /// молчании он делает вид, что ему запретили, и задача исчезает тихо.
    #[test]
    fn the_agent_can_tell_a_refusal_from_a_silence() {
        assert_eq!(Outcome::Rejected.gate_error(), GateError::Rejected);
        assert_eq!(Outcome::Expired.gate_error(), GateError::Expired);
        assert_eq!(Outcome::Stale.gate_error(), GateError::Stale);
        let (no, quiet) = (GateError::Rejected.to_string(), GateError::Expired.to_string());
        assert_ne!(no, quiet, "два разных исхода — два разных текста");
        assert!(quiet.contains("не ответил"));
        assert_ne!(GateError::Rejected.code(), GateError::Expired.code());
    }
}
