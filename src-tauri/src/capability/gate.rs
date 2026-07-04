//! Гейт безопасности (§7) — единственная точка, через которую проходит каждый
//! вызов любой капабилити, кем бы ни инициирован. Живёт в слое истины, не в
//! транспорте, поэтому необходим всем проекциям (MCP-сервер, in-process).
//!
//! Порядок проверок: реестр (notfound) → грант по классу (+ поимённый denylist)
//! → запрет самоэскалации (класс Settings: security-ключи всем + allowlist для
//! agent/plugin) → подтверждение side-effect (дедлайн 60с) → исполнение
//! (дедлайн 30с) → аудит каждого исхода.

use std::time::Duration;
use std::time::Instant;

use serde_json::Value;

use super::audit::{AuditEntry, AuditSink};
use super::confirm::Confirmer;
use super::contract::{CallOutput, GateError, RiskClass};
use super::grant::{Consumer, SettingsWrite, SECURITY_KEYS, SETTINGS_ALLOWLIST};
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

    // фабрика записи аудита с уже известными meta. Аргументы снимаем до инъекции
    // _consumer: в аудите потребитель и так пишется отдельным полем.
    let audit_args = args.clone();
    let entry_for = |outcome: String, ms: u128| AuditEntry {
        consumer: consumer.id.clone(),
        id: meta.id.to_string(),
        class: meta.class.as_str(),
        args: audit_args.clone(),
        provenance: meta.provenance.as_str(),
        outcome,
        ms,
    };

    // 1. Грант по классу (+ поимённый denylist, напр. audit.query агенту).
    if !consumer.grant.allows_id(meta.id, meta.class) {
        audit.record(&entry_for("denied:class".into(), t0.elapsed().as_millis()));
        return Err(GateError::Denied(format!(
            "грант '{}' не разрешает {} ({})",
            consumer.id, meta.id, meta.class.as_str()
        )));
    }

    // 2. Самоэскалация (R7): для класса Settings — security-ключи запрещены ВСЕМ;
    //    agent/plugin (SettingsWrite::Allowlist) — только ключи из allowlist.
    if meta.class == RiskClass::Settings {
        if let Some(key) = touched_key(&args, |k| SECURITY_KEYS.contains(&k)) {
            audit.record(&entry_for("denied:security-key".into(), t0.elapsed().as_millis()));
            return Err(GateError::Denied(format!(
                "ключ '{key}' защищён — меняется только пользователем через UI"
            )));
        }
        if consumer.grant.write == SettingsWrite::Allowlist {
            if let Some(key) = touched_key(&args, |k| !SETTINGS_ALLOWLIST.contains(&k)) {
                audit.record(&entry_for("denied:settings-key".into(), t0.elapsed().as_millis()));
                return Err(GateError::Denied(format!(
                    "ключ '{key}' не в allowlist — агент/плагин не вправе его менять"
                )));
            }
        }
    }

    // 2б. Личность вызывающего для consumer-aware капабилити (entities.publish):
    // ключ служебный, перезаписывается всегда — подделать нельзя. Инъекция после
    // проверки самоэскалации, чтобы _consumer не считался «изменяемым ключом».
    let mut args = args;
    if let Value::Object(ref mut m) = args {
        m.insert("_consumer".into(), Value::String(consumer.id.clone()));
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

/// Первый ключ patch (или корня), удовлетворяющий предикату. Принимаем обе формы:
/// `{patch:{...}}` и `{...}` напрямую.
fn touched_key(args: &Value, pred: impl Fn(&str) -> bool) -> Option<String> {
    let obj = args
        .get("patch")
        .and_then(|p| p.as_object())
        .or_else(|| args.as_object())?;
    obj.keys().find(|k| pred(k.as_str())).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::audit::MemAudit;
    use crate::capability::confirm::AutoApprove;
    use crate::capability::contract::{CapabilityMeta, Provenance};
    use crate::capability::grant::ConfirmPolicy;
    use crate::capability::registry::{make_handler, Registry};
    use serde_json::json;

    /// Реестр с одной Read-капабилити, возвращающей свои args как есть.
    fn echo_registry() -> Registry<()> {
        let mut reg = Registry::new();
        reg.register(
            CapabilityMeta {
                id: "test.echo",
                class: RiskClass::Read,
                provenance: Provenance::Trusted,
                description: "эхо аргументов (тест)",
                input_schema: json!({ "type": "object" }),
            },
            make_handler(|_: (), args| async move { Ok(args) }),
        );
        reg
    }

    #[tokio::test]
    async fn injects_consumer_identity_into_args() {
        let reg = echo_registry();
        let c = Consumer::custom("plugin:test", &[RiskClass::Read], ConfirmPolicy::Never);
        let out = invoke(
            &reg, (), &c, "test.echo", json!({ "x": 1 }),
            &AutoApprove, &MemAudit::new(), GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.value["_consumer"], "plugin:test");
        assert_eq!(out.value["x"], 1, "остальные args не тронуты");
    }

    #[tokio::test]
    async fn overwrites_spoofed_consumer() {
        let reg = echo_registry();
        let c = Consumer::custom("plugin:test", &[RiskClass::Read], ConfirmPolicy::Never);
        let out = invoke(
            &reg, (), &c, "test.echo", json!({ "_consumer": "panel" }),
            &AutoApprove, &MemAudit::new(), GateConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.value["_consumer"], "plugin:test", "подделка перезаписана");
    }
}
