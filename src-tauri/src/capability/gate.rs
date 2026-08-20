//! Гейт безопасности (§7) — единственная точка, через которую проходит каждый
//! вызов любой капабилити, кем бы ни инициирован. Живёт в слое истины, не в
//! транспорте, поэтому необходим всем проекциям (MCP-сервер, in-process).
//!
//! Порядок проверок: реестр (notfound) → грант по классу (+ поимённый denylist)
//! → запрет самоэскалации (класс Settings: security-ключи всем + allowlist для
//! agent/plugin) → подтверждение side-effect, кроме поимённо авто-одобренных
//! пользователем капабилити (дедлайн 60с) → исполнение
//! (дедлайн 30с или собственный дедлайн капабилити) → аудит каждого исхода.

use std::time::Duration;
use std::time::Instant;

use serde_json::Value;

use super::audit::{AuditEntry, AuditSink};
use super::confirm::Confirmer;
use super::contract::{CallOutput, GateError, RiskClass};
use super::grant::{Consumer, SettingsWrite, SECURITY_KEYS, SETTINGS_ALLOWLIST};
use super::registry::Registry;

/// Жёсткий потолок для покапабилитного дедлайна (R3). Своё значение капабилити
/// назначить может, безлимитность — нет: висящий вызов держит гейт и потребителя.
pub const MAX_HANDLER_TIMEOUT: Duration = Duration::from_secs(300);

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
    //    Молча пропускаем только то, что пользователь сам внёс в авто-одобрение
    //    гранта (grants.<consumer>.autoApprove) — поимённо, см. Grant::needs_confirm.
    if consumer.grant.needs_confirm(meta.id, meta.class) {
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
    //    Дедлайн покапабилитный: у ждущих (sessions.wait) свой, у остальных общий.
    let deadline = entry.timeout.unwrap_or(cfg.handler_timeout);
    match tokio::time::timeout(deadline, (entry.handler)(ctx, args.clone())).await {
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

    /// Реестр с одной «долгой» капабилити: спит 200мс, дедлайн у неё свой.
    fn slow_registry(own: Duration) -> Registry<()> {
        let mut reg = Registry::new();
        reg.register_with_timeout(
            CapabilityMeta {
                id: "test.wait",
                class: RiskClass::Read,
                provenance: Provenance::Trusted,
                description: "ждёт дольше общего дедлайна (тест)",
                input_schema: json!({ "type": "object" }),
            },
            make_handler(|_: (), _args| async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(json!({ "waited": true }))
            }),
            own,
        );
        reg
    }

    // Покапабилитный дедлайн: общий (30мс) капабилити не убивает — у неё свой.
    #[tokio::test]
    async fn own_timeout_beats_shared_deadline() {
        let reg = slow_registry(Duration::from_secs(5));
        let cfg = GateConfig { handler_timeout: Duration::from_millis(30), ..GateConfig::default() };
        let out = invoke(
            &reg, (), &Consumer::agent(), "test.wait", json!({}),
            &AutoApprove, &MemAudit::new(), cfg,
        )
        .await
        .expect("своя капабилити ждёт по своему дедлайну");
        assert_eq!(out.value["waited"], true);
    }

    // …но свой дедлайн — не безлимит: истёк — тот же честный failed:timeout.
    #[tokio::test]
    async fn own_timeout_still_kills_hung_handler() {
        let reg = slow_registry(Duration::from_millis(20));
        let audit = MemAudit::new();
        let err = invoke(
            &reg, (), &Consumer::agent(), "test.wait", json!({}),
            &AutoApprove, &audit, GateConfig::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, GateError::Failed(_)));
        assert_eq!(audit.last().unwrap().outcome, "failed:timeout");
    }

    // Потолок режется на регистрации, а не «когда-нибудь на вызове».
    #[test]
    fn own_timeout_capped_and_others_keep_shared_default() {
        let reg = slow_registry(Duration::from_secs(3600));
        assert_eq!(reg.get("test.wait").unwrap().timeout, Some(MAX_HANDLER_TIMEOUT));
        assert_eq!(MAX_HANDLER_TIMEOUT, Duration::from_secs(300));
        // обычная регистрация дедлайна не назначает — работает общий, 30с
        assert_eq!(echo_registry().get("test.echo").unwrap().timeout, None);
        assert_eq!(GateConfig::default().handler_timeout, Duration::from_secs(30));
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
