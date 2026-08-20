//! Слой капабилити — источник истины Jarvis (спека инкр. 8, §4).
//!
//! Один реестр именованных возможностей; единый гейт безопасности перед каждым
//! вызовом; тонкие проекции под потребителей (MCP-сервер для агента, прямой
//! вызов в процессе для панели/тестов). Доменная логика — в фасадах `native/`,
//! которые делегируют в существующие сервисы демона (НЕ переписываем).

// Реэкспорты — публичная поверхность слоя для будущих фаз (агент-хост,
// MCP-сервер-бинарь); часть из них ещё не используется crate-wide.
#![allow(unused_imports)]

pub mod audit;
pub mod confirm;
pub mod contract;
pub mod gate;
pub mod grant;
pub mod registry;
pub mod tokens;
pub mod confirm_panel;

pub mod native;

pub use confirm::{AutoApprove, AutoDeny, Confirmer};
pub use contract::{CallOutput, CapabilityMeta, GateError, Provenance, RiskClass};
pub use gate::{invoke, GateConfig};
pub use grant::{ConfirmPolicy, Consumer, Grant, SettingsWrite};
pub use registry::{make_handler, Registry};

use std::sync::Arc;

use crate::daemon::Daemon;

/// Боевой реестр капабилити демона: контекст хендлеров — `Arc<Daemon>`.
pub type DaemonRegistry = Registry<Arc<Daemon>>;

/// Собрать боевой реестр: регистрирует все нативные фасады.
pub fn build_registry() -> DaemonRegistry {
    let mut reg = Registry::new();
    native::register_all(&mut reg);
    reg
}

#[cfg(test)]
mod tests {
    use super::audit::MemAudit;
    use super::confirm::{AutoApprove, AutoDeny};
    use super::contract::{CapabilityMeta, GateError, Provenance, RiskClass};
    use super::gate::GateConfig;
    use super::grant::{ConfirmPolicy, Consumer};
    use super::registry::{make_handler, Registry};
    use serde_json::json;

    /// Тестовый реестр с контекстом `()` — ядро гейта от Daemon не зависит.
    fn test_registry() -> Registry<()> {
        let mut reg = Registry::new();
        reg.register(
            CapabilityMeta {
                id: "echo.read",
                class: RiskClass::Read,
                provenance: Provenance::Trusted,
                description: "эхо (read)",
                input_schema: json!({"type":"object"}),
            },
            make_handler(|_ctx: (), args| async move { Ok(json!({ "echo": args })) }),
        );
        reg.register(
            CapabilityMeta {
                id: "echo.control",
                class: RiskClass::Control,
                provenance: Provenance::Trusted,
                description: "эхо (control, side-effect)",
                input_schema: json!({"type":"object"}),
            },
            make_handler(|_ctx: (), args| async move { Ok(json!({ "did": args })) }),
        );
        reg.register(
            CapabilityMeta {
                id: "settings.set",
                class: RiskClass::Settings,
                provenance: Provenance::Trusted,
                description: "запись конфига",
                input_schema: json!({"type":"object"}),
            },
            make_handler(|_ctx: (), _args| async move { Ok(json!({ "ok": true })) }),
        );
        reg.register(
            CapabilityMeta {
                id: "boom.read",
                class: RiskClass::Read,
                provenance: Provenance::Trusted,
                description: "всегда падает",
                input_schema: json!({"type":"object"}),
            },
            make_handler(|_ctx: (), _args| async move { Err("сервис недоступен".to_string()) }),
        );
        reg.register(
            CapabilityMeta {
                id: "slow.read",
                class: RiskClass::Read,
                provenance: Provenance::Trusted,
                description: "висит дольше дедлайна хендлера",
                input_schema: json!({"type":"object"}),
            },
            make_handler(|_ctx: (), _args| async move {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                Ok(json!({"slept": true}))
            }),
        );
        reg.register(
            CapabilityMeta {
                id: "settings.other",
                class: RiskClass::Settings,
                provenance: Provenance::Trusted,
                description: "другая settings-капа (для теста class-based)",
                input_schema: json!({"type":"object"}),
            },
            make_handler(|_ctx: (), _args| async move { Ok(json!({"ok":true})) }),
        );
        reg
    }

    // приёмочный 1/9: read вызывается автоматически, успех, провенанс, аудит.
    #[tokio::test]
    async fn read_auto_allowed_records_audit() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let out = super::invoke(&reg, (), &Consumer::agent(), "echo.read", json!({"x":1}), &AutoApprove, &audit, GateConfig::default())
            .await
            .expect("read должен пройти");
        assert_eq!(out.value["echo"]["x"], 1);
        // гейт инжектит _consumer (см. gate.rs, шаг 2б)
        assert_eq!(out.value["echo"]["_consumer"], "agent");
        assert_eq!(out.provenance, Provenance::Trusted);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit.last().unwrap().outcome, "ok");
    }

    // приёмочный 2: control с подтверждением исполняется.
    #[tokio::test]
    async fn control_with_approval_executes() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let out = super::invoke(&reg, (), &Consumer::agent(), "echo.control", json!({"to":"recrew"}), &AutoApprove, &audit, GateConfig::default())
            .await
            .expect("control с approve должен пройти");
        assert_eq!(out.value["did"]["to"], "recrew");
        // гейт инжектит _consumer (см. gate.rs, шаг 2б)
        assert_eq!(out.value["did"]["_consumer"], "agent");
        assert_eq!(audit.last().unwrap().outcome, "ok");
    }

    // приёмочный 2/3: control без подтверждения отклоняется (запутанный помощник).
    #[tokio::test]
    async fn control_without_approval_rejected() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let err = super::invoke(&reg, (), &Consumer::agent(), "echo.control", json!({}), &AutoDeny, &audit, GateConfig::default())
            .await
            .unwrap_err();
        assert_eq!(err, GateError::Rejected);
        assert_eq!(audit.last().unwrap().outcome, "rejected");
    }

    // приёмочный 4/6: класс вне гранта — отказ ещё до исполнения.
    #[tokio::test]
    async fn class_outside_grant_denied() {
        let reg = test_registry();
        let audit = MemAudit::new();
        // потребитель только с read — control запрещён
        let reader = Consumer::custom("reader", &[RiskClass::Read], ConfirmPolicy::Always);
        let err = super::invoke(&reg, (), &reader, "echo.control", json!({}), &AutoApprove, &audit, GateConfig::default())
            .await
            .unwrap_err();
        assert!(matches!(err, GateError::Denied(_)));
        assert_eq!(audit.last().unwrap().outcome, "denied:class");
    }

    // приёмочный 6: settings.set по security-ключу — отказ даже с approve (самоэскалация).
    #[tokio::test]
    async fn settings_set_security_key_blocked() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let err = super::invoke(
            &reg,
            (),
            &Consumer::agent(),
            "settings.set",
            json!({ "patch": { "grants": { "agent": "admin" } } }),
            &AutoApprove,
            &audit,
            GateConfig::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, GateError::Denied(_)));
        assert_eq!(audit.last().unwrap().outcome, "denied:security-key");
    }

    // settings.set по обычному ключу — проходит (с подтверждением).
    #[tokio::test]
    async fn settings_set_normal_key_ok() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let out = super::invoke(
            &reg,
            (),
            &Consumer::agent(),
            "settings.set",
            json!({ "patch": { "hotkey": "Cmd+J" } }),
            &AutoApprove,
            &audit,
            GateConfig::default(),
        )
        .await
        .expect("обычный ключ должен пройти");
        assert_eq!(out.value, json!({"ok":true}));
        assert_eq!(audit.last().unwrap().outcome, "ok");
    }

    // Авто-одобрение: id из списка пользователя исполняется, хотя confirmer
    // отвечает «нет» — значит его вообще не спрашивали.
    #[tokio::test]
    async fn auto_approved_id_runs_without_confirm() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let agent =
            Consumer::agent().with_auto_approve(["echo.control".to_string()].into_iter().collect());
        let out = super::invoke(&reg, (), &agent, "echo.control", json!({"to":"recrew"}), &AutoDeny, &audit, GateConfig::default())
            .await
            .expect("авто-одобренная капабилити идёт мимо подтверждения");
        assert_eq!(out.value["did"]["to"], "recrew");
        assert_eq!(audit.last().unwrap().outcome, "ok");
    }

    // …а её сосед по классу — нет: список поимённый, а не «весь Control».
    #[tokio::test]
    async fn non_listed_id_still_confirmed() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let agent =
            Consumer::agent().with_auto_approve(["sessions.reply".to_string()].into_iter().collect());
        let err = super::invoke(&reg, (), &agent, "echo.control", json!({}), &AutoDeny, &audit, GateConfig::default())
            .await
            .unwrap_err();
        assert_eq!(err, GateError::Rejected);
        assert_eq!(audit.last().unwrap().outcome, "rejected");
    }

    // R7 поверх авто-одобрения: даже с авто-одобренным settings.set агент не
    // выпишет себе новых прав — security-ключ закрыт до подтверждения.
    #[tokio::test]
    async fn auto_approve_cannot_be_self_granted() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let agent =
            Consumer::agent().with_auto_approve(["settings.set".to_string()].into_iter().collect());
        let err = super::invoke(
            &reg,
            (),
            &agent,
            "settings.set",
            json!({ "patch": { "grants": { "agent": { "autoApprove": ["sessions.reply"] } } } }),
            &AutoApprove,
            &audit,
            GateConfig::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, GateError::Denied(_)));
        assert_eq!(audit.last().unwrap().outcome, "denied:security-key");
    }

    // неизвестная капабилити — NotFound, тоже в аудите.
    #[tokio::test]
    async fn unknown_capability_not_found() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let err = super::invoke(&reg, (), &Consumer::agent(), "nope.nope", json!({}), &AutoApprove, &audit, GateConfig::default())
            .await
            .unwrap_err();
        assert!(matches!(err, GateError::NotFound(_)));
        assert_eq!(audit.last().unwrap().outcome, "notfound");
    }

    // сбой хендлера — Failed, провенанс не теряется в аудите.
    #[tokio::test]
    async fn handler_failure_surfaced() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let err = super::invoke(&reg, (), &Consumer::agent(), "boom.read", json!({}), &AutoApprove, &audit, GateConfig::default())
            .await
            .unwrap_err();
        assert!(matches!(err, GateError::Failed(_)));
        assert!(audit.last().unwrap().outcome.starts_with("failed:"));
    }

    // приёмочный 9: нативный реестр собирается, read-капабилити на месте,
    // видны агенту как инструменты (одна регистрация → виден без правок агента).
    #[test]
    fn native_registry_wires_read_capabilities() {
        let reg = super::build_registry();
        for id in [
            "sessions.list",
            "sessions.get",
            "metrics.query",
            "notifications.history",
            "tasks.get",
            "settings.get",
            "audit.query",
            "chats.read",
        ] {
            assert!(reg.get(id).is_some(), "нет капабилити {id}");
        }
        // chats.read всегда untrusted (§6)
        assert_eq!(reg.get("chats.read").unwrap().meta.provenance, Provenance::Untrusted);
        assert_eq!(reg.get("metrics.query").unwrap().meta.class, RiskClass::Read);
        // агент видит read-инструменты в проекции tools/list
        let tools = reg.tools_json(&Consumer::agent().grant);
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"metrics.query"));
        assert!(names.contains(&"sessions.list"));
    }

    // приёмочный 2/3: control/settings-капабилити имеют правильный класс, значит
    // гейт ВСЕГДА потребует подтверждения у агента (доказано generic-тестами выше).
    #[test]
    fn control_settings_capabilities_have_side_effect_class() {
        let reg = super::build_registry();
        assert_eq!(reg.get("sessions.reply").unwrap().meta.class, RiskClass::Control);
        assert_eq!(reg.get("sessions.control").unwrap().meta.class, RiskClass::Control);
        assert_eq!(reg.get("settings.set").unwrap().meta.class, RiskClass::Settings);
        // read-only потребитель НЕ видит их в tools/list
        let reader = Consumer::custom("reader", &[RiskClass::Read], ConfirmPolicy::Never);
        let tools = reg.tools_json(&reader.grant);
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(!names.contains(&"sessions.reply"));
        assert!(!names.contains(&"settings.set"));
    }

    // sessions.rename — side-effect (подтверждение обязательно), но класса
    // Settings у неё быть НЕ может: гейт читает аргументы settings-капабилити
    // как патч конфига, и 'session_id' сразу упёрся бы в SETTINGS_ALLOWLIST.
    #[test]
    fn sessions_rename_is_a_confirmed_capability_the_agent_can_see() {
        let reg = super::build_registry();
        let cap = reg.get("sessions.rename").expect("sessions.rename должна быть в реестре");
        assert!(cap.meta.class.is_side_effect(), "переименование не должно идти без спроса");
        assert_ne!(cap.meta.class, RiskClass::Settings, "аргументы — не патч настроек");
        // агент обязан понимать по описанию, когда звать и как снять имя
        assert!(cap.meta.description.contains("переименовать"));
        assert!(cap.meta.description.contains("Пустая строка"));
        assert_eq!(cap.meta.input_schema["required"], json!(["session_id", "title"]));
        let tools = reg.tools_json(&Consumer::agent().grant);
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"sessions.rename"), "агенту инструмент не виден");
    }

    /// Видеть инструмент мало — надо, чтобы вызов доходил до хендлера. С классом
    /// Settings гейт отклонил бы его на самоэскалации («ключ 'session_id' не в
    /// allowlist»), и переименование голосом не работало бы вообще.
    #[tokio::test]
    async fn agent_really_gets_through_the_gate_to_rename() {
        let meta = super::build_registry().get("sessions.rename").unwrap().meta.clone();
        let mut reg: Registry<()> = Registry::new();
        reg.register(meta, make_handler(|_ctx: (), args| async move { Ok(json!({ "did": args })) }));
        let args = json!({ "session_id": "abc", "title": "БД" });
        let out = super::invoke(
            &reg, (), &Consumer::agent(), "sessions.rename", args,
            &AutoApprove, &MemAudit::new(), GateConfig::default(),
        )
        .await
        .expect("агент обязан дойти до переименования");
        assert_eq!(out.value["did"]["title"], "БД");

        // …но только с подтверждением: молчаливый отказ пользователя = отказ вызова.
        let denied = super::invoke(
            &reg, (), &Consumer::agent(), "sessions.rename",
            json!({ "session_id": "abc", "title": "БД" }),
            &AutoDeny, &MemAudit::new(), GateConfig::default(),
        )
        .await;
        assert!(matches!(denied, Err(GateError::Rejected)), "переименование прошло без спроса");
    }

    /// Запуск сессии тратит деньги и плодит процессы — по умолчанию карточка.
    /// Класс Settings взять нельзя по той же причине, что и у sessions.rename:
    /// гейт прочитал бы 'agent'/'cwd' как патч конфига и отклонил по allowlist.
    #[test]
    fn spawn_is_a_confirmed_capability_the_agent_can_see() {
        let reg = super::build_registry();
        let cap = reg.get("sessions.spawn").expect("sessions.spawn должна быть в реестре");
        assert_eq!(cap.meta.class, RiskClass::Control, "запуск — side-effect, но не патч настроек");
        assert!(Consumer::agent().grant.needs_confirm("sessions.spawn", cap.meta.class),
            "по умолчанию запуск обязан спрашивать");
        // агент обязан понять из описания, что имя обязательно и id придёт сразу
        assert!(cap.meta.description.contains("ОБЯЗАТЕЛЬНО"));
        assert!(cap.meta.description.contains("СРАЗУ"));
        assert_eq!(cap.meta.input_schema["required"], json!(["agent", "name", "cwd", "task"]));
        let tools = reg.tools_json(&Consumer::agent().grant);
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"sessions.spawn"), "агенту инструмент не виден");
        assert!(names.contains(&"sessions.close"));
        // read-only плагин запускать сессии не может — это Control
        let reader = Consumer::plugin("reader", &[RiskClass::Read]);
        assert!(!reader.grant.allows_id("sessions.spawn", RiskClass::Control));
    }

    /// Грант на авто-запуск выдаёт ЧЕЛОВЕК настройкой, а не константа в коде:
    /// без `grants.agent.autoApprove` карточка на месте, с ним — нет.
    #[tokio::test]
    async fn auto_launch_is_granted_by_settings_not_by_code() {
        let meta = super::build_registry().get("sessions.spawn").unwrap().meta.clone();
        let mut reg: Registry<()> = Registry::new();
        reg.register(meta, make_handler(|_ctx: (), args| async move { Ok(json!({ "did": args })) }));
        let args = json!({ "agent": "claude", "name": "Сайдбар·JRV·O5", "cwd": "/p", "task": "работай" });

        // молчаливый отказ пользователя = отказ вызова
        let denied = super::invoke(&reg, (), &Consumer::agent(), "sessions.spawn", args.clone(),
            &AutoDeny, &MemAudit::new(), GateConfig::default()).await;
        assert!(matches!(denied, Err(GateError::Rejected)), "запуск прошёл без спроса");

        // …а с грантом из настроек — идёт молча
        let granted = Consumer::agent().with_auto_approve(
            super::grant::auto_approve_from_settings(
                &json!({"grants":{"agent":{"autoApprove":["sessions.spawn"]}}}), "agent"),
        );
        let out = super::invoke(&reg, (), &granted, "sessions.spawn", args,
            &AutoDeny, &MemAudit::new(), GateConfig::default())
            .await
            .expect("грант человека снимает карточку");
        assert_eq!(out.value["did"]["name"], "Сайдбар·JRV·O5");
        // соседи по классу остаются со спросом — это не «выключить гейт»
        assert!(granted.grant.needs_confirm("sessions.reply", RiskClass::Control));
    }

    /// Закрытие СВОЕЙ дочерней карточки не просит (эффект ограничен тем, что
    /// потребитель сам и создал), но остаётся Control: read-only плагину не дано.
    #[tokio::test]
    async fn closing_your_own_child_needs_no_card() {
        let reg = super::build_registry();
        let cap = reg.get("sessions.close").expect("sessions.close должна быть в реестре");
        assert_eq!(cap.meta.class, RiskClass::Control);
        assert!(!Consumer::agent().grant.needs_confirm("sessions.close", cap.meta.class));
        assert!(!Consumer::panel().grant.needs_confirm("sessions.close", cap.meta.class));
        assert!(!Consumer::plugin("x", &[RiskClass::Read]).grant.allows_id("sessions.close", RiskClass::Control));

        // и вызов доходит до хендлера при confirmer'е, отвечающем «нет»
        let meta = cap.meta.clone();
        let mut r: Registry<()> = Registry::new();
        r.register(meta, make_handler(|_ctx: (), args| async move { Ok(json!({ "did": args })) }));
        let out = super::invoke(&r, (), &Consumer::agent(), "sessions.close",
            json!({ "id": "spawn-abc" }), &AutoDeny, &MemAudit::new(), GateConfig::default())
            .await
            .expect("закрытие своей дочерней не спрашивает");
        assert_eq!(out.value["did"]["id"], "spawn-abc");
    }

    // R4/least-priv: агент НЕ видит audit.query в tools/list (denied_ids).
    #[test]
    fn agent_tools_exclude_audit_query() {
        let reg = super::build_registry();
        let tools = reg.tools_json(&Consumer::agent().grant);
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"metrics.query"));
        assert!(!names.contains(&"audit.query"), "аудит агенту не проецируется");
    }

    // tools/list грант-фильтр: reader не видит control/settings.
    #[test]
    fn tools_list_filtered_by_grant() {
        let reg = test_registry();
        let reader = Consumer::custom("reader", &[RiskClass::Read], ConfirmPolicy::Never);
        let tools = reg.tools_json(&reader.grant);
        let names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"echo.read"));
        assert!(names.contains(&"boom.read"));
        assert!(!names.contains(&"echo.control"));
        assert!(!names.contains(&"settings.set"));
    }

    fn fast_cfg() -> super::gate::GateConfig {
        super::gate::GateConfig {
            confirm_timeout: std::time::Duration::from_millis(80),
            handler_timeout: std::time::Duration::from_millis(80),
        }
    }

    // R3: хендлер дольше дедлайна → Failed(timeout), аудит failed:timeout.
    #[tokio::test]
    async fn handler_timeout_fails_safely() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let err = super::invoke(&reg, (), &Consumer::agent(), "slow.read", json!({}), &AutoApprove, &audit, fast_cfg())
            .await
            .unwrap_err();
        assert!(matches!(err, GateError::Failed(_)));
        assert_eq!(audit.last().unwrap().outcome, "failed:timeout");
    }

    // R3: подтверждение дольше дедлайна → Rejected, аудит rejected:timeout.
    #[tokio::test]
    async fn confirm_timeout_rejects() {
        struct SlowConfirm;
        impl super::confirm::Confirmer for SlowConfirm {
            fn confirm<'a>(
                &'a self,
                _m: &'a CapabilityMeta,
                _a: &'a serde_json::Value,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
                Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    true
                })
            }
        }
        let reg = test_registry();
        let audit = MemAudit::new();
        let err = super::invoke(&reg, (), &Consumer::agent(), "echo.control", json!({}), &SlowConfirm, &audit, fast_cfg())
            .await
            .unwrap_err();
        assert_eq!(err, GateError::Rejected);
        assert_eq!(audit.last().unwrap().outcome, "rejected:timeout");
    }

    // R7: агент пишет ключ ВНЕ allowlist → отказ (даже не security-ключ).
    #[tokio::test]
    async fn agent_settings_non_allowlisted_denied() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let err = super::invoke(&reg, (), &Consumer::agent(), "settings.set",
            json!({"patch":{"someInternal":1}}), &AutoApprove, &audit, GateConfig::default())
            .await.unwrap_err();
        assert!(matches!(err, GateError::Denied(_)));
        assert_eq!(audit.last().unwrap().outcome, "denied:settings-key");
    }

    // R7: класс-based — ВТОРАЯ settings-капа с другим id тоже под allowlist.
    #[tokio::test]
    async fn class_based_escalation_covers_other_settings_cap() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let err = super::invoke(&reg, (), &Consumer::agent(), "settings.other",
            json!({"patch":{"grants":{"agent":"admin"}}}), &AutoApprove, &audit, GateConfig::default())
            .await.unwrap_err();
        assert!(matches!(err, GateError::Denied(_)));
    }

    // R7: панель (SettingsWrite::All) НЕ ограничена allowlist.
    #[tokio::test]
    async fn panel_settings_not_restricted_by_allowlist() {
        let reg = test_registry();
        let audit = MemAudit::new();
        let out = super::invoke(&reg, (), &Consumer::panel(), "settings.set",
            json!({"patch":{"someInternal":1}}), &AutoApprove, &audit, GateConfig::default())
            .await.expect("панель пишет любой не-security ключ");
        assert_eq!(out.value, json!({"ok":true}));
    }

    // Phase 7 / §10: stt.transcribe зарегистрирована в нативном реестре.
    #[test]
    fn build_registry_includes_stt_transcribe() {
        let reg = super::build_registry();
        let entry = reg.get("stt.transcribe");
        assert!(entry.is_some(), "stt.transcribe должна быть в реестре");
        let meta = &entry.unwrap().meta;
        assert_eq!(meta.class, RiskClass::Control, "класс STT — Control (доступ к микрофону)");
        assert_eq!(meta.provenance, Provenance::Trusted, "провенанс STT — Trusted");
    }

    // Phase 7 / §10: агент НЕ видит stt.transcribe в tools/list (denied_ids).
    #[test]
    fn agent_tools_exclude_stt_transcribe() {
        let reg = super::build_registry();
        let tools = reg.tools_json(&Consumer::agent().grant);
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(!names.contains(&"stt.transcribe"), "stt.transcribe агенту не проецируется");
        // При этом обычные Control-капабилити — sessions.reply — агент видит.
        assert!(names.contains(&"sessions.reply"), "sessions.reply агент видит как Control");
    }

    // Phase 7 / §10: панель ВИДИТ stt.transcribe (denied_ids у панели пуст).
    #[test]
    fn panel_tools_include_stt_transcribe() {
        let reg = super::build_registry();
        let tools = reg.tools_json(&Consumer::panel().grant);
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"stt.transcribe"), "панель видит stt.transcribe");
    }

    // Phase 7 / §10: плагин с Control-классом ВИДИТ stt.transcribe.
    #[test]
    fn plugin_with_control_sees_stt_transcribe() {
        let reg = super::build_registry();
        let plugin = Consumer::plugin("voice-plugin", &[RiskClass::Control]);
        let tools = reg.tools_json(&plugin.grant);
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"stt.transcribe"),
            "плагин с Control-грантом видит stt.transcribe");
    }

    // Phase 7 / §10: плагин БЕЗ Control-класса НЕ видит stt.transcribe.
    #[test]
    fn plugin_without_control_cannot_see_stt_transcribe() {
        let reg = super::build_registry();
        let plugin = Consumer::plugin("read-only-plugin", &[RiskClass::Read]);
        let tools = reg.tools_json(&plugin.grant);
        let names: Vec<&str> =
            tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(!names.contains(&"stt.transcribe"),
            "плагин без Control не видит stt.transcribe");
    }
}
