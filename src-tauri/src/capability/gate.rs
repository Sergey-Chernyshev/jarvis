//! Гейт безопасности (§7) — единственная точка, через которую проходит каждый
//! вызов любой капабилити, кем бы ни инициирован. Живёт в слое истины, не в
//! транспорте, поэтому необходим всем проекциям (MCP-сервер, in-process).
//!
//! Порядок проверок: грант по классу → запрет самоэскалации (settings.set по
//! защищённому ключу) → подтверждение side-effect → исполнение → аудит.

use std::time::Duration;
use std::time::Instant;

use serde_json::Value;

use super::audit::{AuditEntry, AuditSink};
use super::confirm::Confirmer;
use super::contract::{CallOutput, GateError};
use super::grant::{Consumer, SECURITY_KEYS};
use super::registry::Registry;

/// Дедлайны гейта (R3). Default — боевые; тесты подставляют короткие.
#[derive(Clone, Copy, Debug)]
pub struct GateConfig {
    pub confirm_timeout: Duration,
    pub handler_timeout: Duration,
}

impl Default for GateConfig {
    fn default() -> Self {
        GateConfig {
            confirm_timeout: Duration::from_secs(60),
            handler_timeout: Duration::from_secs(30),
        }
    }
}

/// Прогнать вызов капабилити через все проверки и (при успехе) исполнить.
#[allow(clippy::too_many_arguments)]
pub async fn invoke<C>(
    reg: &Registry<C>,
    ctx: C,
    consumer: &Consumer,
    id: &str,
    args: Value,
    confirmer: &dyn Confirmer,
    audit: &dyn AuditSink,
    cfg: GateConfig,
) -> Result<CallOutput, GateError> {
    let t0 = Instant::now();

    let Some(entry) = reg.get(id) else {
        audit.record(&AuditEntry {
            consumer: consumer.id.clone(),
            id: id.to_string(),
            class: "?",
            args,
            provenance: "?",
            outcome: "notfound".into(),
            ms: t0.elapsed().as_millis(),
        });
        return Err(GateError::NotFound(id.to_string()));
    };
    let meta = &entry.meta;

    // фабрика записи аудита с уже известными meta
    let entry_for = |outcome: String, ms: u128| AuditEntry {
        consumer: consumer.id.clone(),
        id: meta.id.to_string(),
        class: meta.class.as_str(),
        args: args.clone(),
        provenance: meta.provenance.as_str(),
        outcome,
        ms,
    };

    // 1. Грант по классу.
    if !consumer.grant.allows(meta.class) {
        audit.record(&entry_for("denied:class".into(), t0.elapsed().as_millis()));
        return Err(GateError::Denied(format!(
            "грант '{}' не разрешает класс {}",
            consumer.id,
            meta.class.as_str()
        )));
    }

    // 2. Запрет самоэскалации: settings.set не вправе трогать security-ключи.
    if meta.id == "settings.set" {
        if let Some(key) = touched_security_key(&args) {
            audit.record(&entry_for("denied:security-key".into(), t0.elapsed().as_millis()));
            return Err(GateError::Denied(format!(
                "ключ '{key}' защищён — меняется только пользователем через UI"
            )));
        }
    }

    // 3. Подтверждение side-effect — с дедлайном (R3): нет ответа → Rejected.
    if consumer.grant.needs_confirm(meta.class) {
        let approved = match tokio::time::timeout(cfg.confirm_timeout, confirmer.confirm(meta, &args)).await {
            Ok(a) => a,
            Err(_) => {
                audit.record(&entry_for("rejected:timeout".into(), t0.elapsed().as_millis()));
                return Err(GateError::Rejected);
            }
        };
        if !approved {
            audit.record(&entry_for("rejected".into(), t0.elapsed().as_millis()));
            return Err(GateError::Rejected);
        }
    }

    // 4. Исполнение — с дедлайном (R3, fail-safe liveness; эффект at-least-once).
    match tokio::time::timeout(cfg.handler_timeout, (entry.handler)(ctx, args.clone())).await {
        Err(_) => {
            audit.record(&entry_for("failed:timeout".into(), t0.elapsed().as_millis()));
            Err(GateError::Failed("timeout".into()))
        }
        Ok(Ok(value)) => {
            audit.record(&entry_for("ok".into(), t0.elapsed().as_millis()));
            Ok(CallOutput { value, provenance: meta.provenance })
        }
        Ok(Err(e)) => {
            audit.record(&entry_for(format!("failed:{e}"), t0.elapsed().as_millis()));
            Err(GateError::Failed(e))
        }
    }
}

/// Если аргументы `settings.set` пытаются изменить защищённый ключ — вернуть его.
/// Принимаем обе формы: `{patch:{...}}` и `{...}` напрямую.
fn touched_security_key(args: &Value) -> Option<String> {
    let obj = args
        .get("patch")
        .and_then(|p| p.as_object())
        .or_else(|| args.as_object())?;
    obj.keys().find(|k| SECURITY_KEYS.contains(&k.as_str())).cloned()
}
