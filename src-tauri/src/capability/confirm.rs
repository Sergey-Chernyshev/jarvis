//! Подтверждение side-effect (§7). Абстрагировано за трейтом, чтобы ядро
//! тестировалось без живого UI: тесты подставляют `AutoApprove`/`AutoDeny`,
//! а реальный путь — `PanelConfirmer` (фаза 5), который рисует карточку в
//! панели и ждёт решения пользователя.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use super::contract::CapabilityMeta;

/// Возвращает true, если пользователь подтвердил вызов.
pub trait Confirmer: Send + Sync {
    fn confirm<'a>(
        &'a self,
        meta: &'a CapabilityMeta,
        args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}

/// Авто-подтверждение — для тестов и для гранта панели (пользователь сам нажал).
pub struct AutoApprove;
impl Confirmer for AutoApprove {
    fn confirm<'a>(
        &'a self,
        _meta: &'a CapabilityMeta,
        _args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
}

/// Авто-отказ — для тестов «запутанного помощника».
pub struct AutoDeny;
impl Confirmer for AutoDeny {
    fn confirm<'a>(
        &'a self,
        _meta: &'a CapabilityMeta,
        _args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
}
