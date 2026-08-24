//! Встроенные плагины — наши собственные способности на общем контракте
//! (спека `2026-08-19-everything-is-plugin-design.md` §2.1).
//!
//! Регистрируются здесь и больше нигде: ядро знает о них ровно столько же,
//! сколько о стороннем сайдкаре — манифест и рантайм. Разница одна и честная:
//! встроенный плагин **не изолирован** (это наш код в нашем процессе) и токена
//! не получает; «выключен» для него значит «не держит ресурс и не отвечает на
//! команды», а не «выгружен из памяти».

pub mod clamshell;
pub mod keep_awake;

use std::sync::Arc;

use serde_json::Value;

use super::Host;

pub fn register_all(host: &Host) {
    // Порядок регистрации = порядок в списке настроек и в меню трея.
    let _ = host.register(Arc::new(keep_awake::KeepAwake::new()));
    let _ = host.register(Arc::new(clamshell::Clamshell::new()));
    // Остальные способности переезжают сюда инкрементами 5-6 спеки.
}

/// Сервисы Jarvis отвечают `{ok, error}`-объектом, контракт плагина — `Result`.
/// Перевод в одном месте, чтобы адаптеры оставались в три строки.
pub(crate) fn to_result(v: Value) -> Result<Value, String> {
    if v.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("не вышло, причина не названа")
            .to_string());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ok_shape_passes_error_shape_becomes_err() {
        assert!(to_result(json!({ "ok": true })).is_ok());
        assert_eq!(
            to_result(json!({ "ok": false, "error": "плагин выключен" })).unwrap_err(),
            "плагин выключен"
        );
        assert!(to_result(json!({ "value": 1 })).is_ok(), "нет ok — не отказ");
    }

    /// INV-KERNEL (спека §2.2): в ядре нет ветвлений по идентификатору
    /// конкретного плагина. Тест живёт здесь, а не в ядре, ровно потому, что
    /// в ядре нельзя даже упомянуть эти строки.
    ///
    /// Осознанное исключение: `convo/skills.rs` пока зовёт «keep-awake»
    /// по имени — голосовые скилы переезжают в реестр инкрементом 6.
    #[test]
    fn kernel_never_branches_on_a_plugin_id() {
        let kernel = [
            ("plugin/mod.rs", include_str!("../mod.rs")),
            ("plugin/host.rs", include_str!("../host.rs")),
            ("plugin/contract.rs", include_str!("../contract.rs")),
            ("plugin/manifest.rs", include_str!("../manifest.rs")),
            ("plugin/sidecar.rs", include_str!("../sidecar.rs")),
            ("tray.rs", include_str!("../../tray.rs")),
            ("ipc.rs", include_str!("../../ipc.rs")),
        ];
        // Имена собираются по кускам: написанные целиком, они нашлись бы в
        // исходнике самого теста.
        let ids = [["keep", "awake"].join("-"), ["clam", "shell"].concat()];
        for (file, src) in kernel {
            for id in &ids {
                assert!(
                    !src.contains(id),
                    "{file} знает про плагин '{id}' — это возврат к хардкоду; \
                     расширять надо контракт, а не ядро"
                );
            }
        }
    }

    /// Манифесты своих плагинов проходят ту же валидацию, что чужие: если бы
    /// они её не проходили, «всё есть плагин» было бы фикцией уже на старте.
    #[test]
    fn builtin_manifests_are_valid_and_declare_their_commands() {
        use crate::plugin::contract::Plugin;
        use crate::daemon::Daemon;
        let ka = keep_awake::KeepAwake::new();
        let cs = clamshell::Clamshell::new();
        let manifests = [
            Plugin::<Arc<Daemon>>::manifest(&ka),
            Plugin::<Arc<Daemon>>::manifest(&cs),
        ];
        for m in manifests {
            assert!(!m.commands.is_empty(), "{}: плагин без команд бесполезен", m.id);
            assert!(m.settings.iter().all(|s| s.key != "enabled"), "{}", m.id);
            assert!(m.risk_classes().is_empty(), "{}: встроенный гранта не просит", m.id);
        }
    }
}
