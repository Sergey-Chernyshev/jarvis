//! Гранты потребителей и запрет самоэскалации (§7, §8 слой b).
//!
//! Грант = какие классы капабилити разрешены потребителю и нужна ли
//! конфирмация side-effect. Внутренний агент — такой же грантодержатель,
//! как плагин (догфудинг). `Admin` не выдаётся никому, кроме пользователя.

use std::collections::HashSet;

use serde_json::Value;

use super::contract::RiskClass;

/// Политика подтверждения side-effect для гранта.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfirmPolicy {
    /// Всегда спрашивать пользователя (грант агента в v1).
    Always,
    /// Не спрашивать (грант панели — это сам пользователь).
    Never,
}

/// Право записи конфига: панель (пользователь) пишет всё; агент/плагин — только
/// ключи из allowlist (deny-by-default, R7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsWrite {
    All,
    Allowlist,
}

/// Набор прав потребителя.
#[derive(Clone, Debug)]
pub struct Grant {
    pub classes: HashSet<RiskClass>,
    pub confirm: ConfirmPolicy,
    pub write: SettingsWrite,
    /// Капабилити, которые этому потребителю запрещены поимённо (помимо класса).
    pub denied_ids: HashSet<&'static str>,
    /// Капабилити, которые пользователь заранее разрешил без подтверждения.
    /// Поимённо и только из настроек: «класс без подтверждения» — это гейт
    /// выключенный целиком, такой настройки нет и не будет.
    pub auto_approve: HashSet<String>,
}

impl Grant {
    pub fn allows(&self, class: RiskClass) -> bool {
        self.classes.contains(&class)
    }
    /// Класс разрешён И капабилити не в поимённом denylist.
    pub fn allows_id(&self, id: &str, class: RiskClass) -> bool {
        self.allows(class) && !self.denied_ids.contains(id)
    }
    /// Нужна ли конфирмация для этой капабилити при этом гранте. Авто-одобрение
    /// снимает лишь вопрос, и только для перечисленных id: прав оно не добавляет —
    /// класс, denylist и security-ключи проверяются гейтом до и помимо него.
    pub fn needs_confirm(&self, id: &str, class: RiskClass) -> bool {
        class.is_side_effect()
            && self.confirm == ConfirmPolicy::Always
            && !SELF_LIMITED.contains(&id)
            && !self.auto_approve.contains(id)
    }
}

/// Идентифицированный потребитель капабилити (агент/панель/плагин).
#[derive(Clone, Debug)]
pub struct Consumer {
    pub id: String,
    pub grant: Grant,
}

impl Consumer {
    /// Грант внутреннего агента (v1): read — авто, control/settings —
    /// подтверждение всегда, admin — недоступен (§8).
    pub fn agent() -> Self {
        let mut classes = HashSet::new();
        classes.insert(RiskClass::Read);
        classes.insert(RiskClass::Control);
        classes.insert(RiskClass::Settings);
        // RiskClass::Admin намеренно НЕ включён — запрет самоэскалации.
        Consumer {
            id: "agent".into(),
            grant: Grant {
                classes,
                confirm: ConfirmPolicy::Always,
                write: SettingsWrite::Allowlist,
                // аудит — поверхность эксфильтрации/разведки (спека §11): агенту не даём.
                // stt.transcribe — доступ к микрофону (§10): агент не вправе слушать
                // речь пользователя без явного гранта; внутренняя диктовка идёт напрямую.
                denied_ids: ["audit.query", "stt.transcribe"].into_iter().collect(),
                // пусто по умолчанию: без явной настройки поведение прежнее —
                // спрашиваем каждый side-effect. Заполняет только `with_auto_approve`.
                auto_approve: HashSet::new(),
            },
        }
    }

    /// Навесить поимённое авто-одобрение из настроек пользователя (см.
    /// `auto_approve_from_settings`). Отдельный шаг, а не поле конструктора:
    /// `Consumer::agent()` обязан оставаться чистым — от него зависят проекция
    /// tools/list и тесты, а диск читается только на пути живого вызова.
    pub fn with_auto_approve(mut self, ids: HashSet<String>) -> Self {
        self.grant.auto_approve = ids;
        self
    }

    /// Грант панели/трея — это действия самого пользователя: всё, кроме admin,
    /// без конфирмации (пользователь уже нажал кнопку в UI).
    pub fn panel() -> Self {
        let mut classes = HashSet::new();
        classes.insert(RiskClass::Read);
        classes.insert(RiskClass::Control);
        classes.insert(RiskClass::Settings);
        Consumer {
            id: "panel".into(),
            grant: Grant {
                classes,
                confirm: ConfirmPolicy::Never,
                write: SettingsWrite::All,
                denied_ids: HashSet::new(),
                auto_approve: HashSet::new(),
            },
        }
    }

    /// Грант плагина: least-privilege из манифеста, подтверждение side-effect
    /// всегда, admin недоступен, запись конфига — только allowlist.
    pub fn plugin(id: &str, classes: &[RiskClass]) -> Self {
        let classes: HashSet<RiskClass> =
            classes.iter().copied().filter(|c| *c != RiskClass::Admin).collect();
        Consumer {
            id: format!("plugin:{id}"),
            grant: Grant {
                classes,
                confirm: ConfirmPolicy::Always,
                write: SettingsWrite::Allowlist,
                denied_ids: HashSet::new(),
                auto_approve: HashSet::new(),
            },
        }
    }

    /// Тестовый потребитель с произвольным набором классов и политикой.
    #[cfg(test)]
    pub fn custom(id: &str, classes: &[RiskClass], confirm: ConfirmPolicy) -> Self {
        Consumer {
            id: id.into(),
            grant: Grant {
                classes: classes.iter().copied().collect(),
                confirm,
                write: SettingsWrite::All,
                denied_ids: HashSet::new(),
                auto_approve: HashSet::new(),
            },
        }
    }
}

/// Капабилити, чей эффект ограничен тем, что потребитель создал САМ, — карточки
/// они не просят. Спрашивать «можно закрыть сессию, которую ты минуту назад сам
/// и поднял?» — не безопасность, а шум: разрешения там ровно столько, сколько
/// уже дали на запуск. Гейт при этом не выключается: класс, denylist и аудит
/// работают как обычно, а владение проверяет сам хендлер (у `sessions.close` —
/// по реестру запусков: чужая и человеческая сессия в него не попадают).
///
/// Список поимённый и короткий намеренно: «класс без подтверждения» — это гейт
/// выключенный целиком, такой настройки нет и не будет.
pub const SELF_LIMITED: &[&str] = &["sessions.close"];

/// Ключи `~/.jarvis/settings.json`, которые НИ ОДНА капабилити менять не вправе
/// (§7, запрет самоэскалации): гранты, плагины, политика гейта. Их правит
/// только пользователь напрямую через UI/конфиг.
pub const SECURITY_KEYS: &[&str] = &["grants", "plugins", "gatePolicy", "capability"];

/// Ключи settings.json, которые агент/плагин ВПРАВЕ менять (deny-by-default, R7).
/// Всё, чего тут нет (включая SECURITY_KEYS), агенту/плагину запрещено. Панель
/// (SettingsWrite::All) не ограничена этим списком.
pub const SETTINGS_ALLOWLIST: &[&str] = &[
    "hotkey", "notifyDone", "notifyWaiting", "position", "autoResume",
    "voice", "diagnostics", "duckOthers", "quiet", "proxy",
];

/// Поимённый allowlist авто-одобрения потребителя из `~/.jarvis/settings.json`:
/// `{"grants": {"agent": {"autoApprove": ["sessions.reply"]}}}`.
///
/// Ключ `grants` — в SECURITY_KEYS, поэтому выдать его себе капабилити не может
/// ни при каком гранте: список пишет человек (панель настроек или файл руками).
/// Нет ключа, чужая форма, мусор в элементах → пусто, то есть «спрашивать всё»:
/// авто-одобрение включается только явным перечислением id, без шаблонов и «*».
pub fn auto_approve_from_settings(settings: &Value, consumer: &str) -> HashSet<String> {
    settings
        .get("grants")
        .and_then(|g| g.get(consumer))
        .and_then(|c| c.get("autoApprove"))
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(String::from).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_excludes_audit_query_and_is_allowlist_writer() {
        let g = Consumer::agent().grant;
        assert!(g.allows(RiskClass::Read));
        assert!(!g.allows_id("audit.query", RiskClass::Read), "агенту аудит не виден");
        assert_eq!(g.write, SettingsWrite::Allowlist);
    }

    #[test]
    fn agent_excludes_stt_transcribe() {
        let g = Consumer::agent().grant;
        // Агент имеет класс Control, но stt.transcribe — в denied_ids → недоступна.
        assert!(g.allows(RiskClass::Control), "класс Control агенту разрешён");
        assert!(!g.allows_id("stt.transcribe", RiskClass::Control), "stt.transcribe агенту запрещена поимённо");
    }

    #[test]
    fn panel_sees_stt_transcribe() {
        let g = Consumer::panel().grant;
        // Панель — это сам пользователь, denied_ids пуст → stt.transcribe доступна.
        assert!(g.allows_id("stt.transcribe", RiskClass::Control), "панель видит stt.transcribe");
    }

    #[test]
    fn plugin_with_control_grant_can_access_stt_transcribe() {
        // Плагин с Control в гранте — stt.transcribe НЕ в denied_ids плагина,
        // т.е. пройдёт gate (при наличии класса Control).
        let c = Consumer::plugin("voice-plugin", &[RiskClass::Control]);
        assert!(c.grant.allows(RiskClass::Control));
        assert!(c.grant.allows_id("stt.transcribe", RiskClass::Control),
            "плагин с Control-грантом может вызвать stt.transcribe");
    }

    #[test]
    fn plugin_without_control_cannot_access_stt_transcribe() {
        // Плагин без Control → класс не разрешён → stt.transcribe недоступна.
        let c = Consumer::plugin("read-only-plugin", &[RiskClass::Read]);
        assert!(!c.grant.allows_id("stt.transcribe", RiskClass::Control),
            "плагин без Control-класса не может вызвать stt.transcribe");
    }

    #[test]
    fn panel_is_full_writer_and_sees_everything() {
        let g = Consumer::panel().grant;
        assert!(g.allows_id("audit.query", RiskClass::Read));
        assert_eq!(g.write, SettingsWrite::All);
    }

    #[test]
    fn plugin_is_least_privilege() {
        let c = Consumer::plugin("x", &[RiskClass::Read]);
        assert!(c.grant.allows(RiskClass::Read));
        assert!(!c.grant.allows(RiskClass::Settings));
        assert_eq!(c.grant.write, SettingsWrite::Allowlist);
    }

    #[test]
    fn agent_asks_confirmation_by_default() {
        // Дефолт — пусто: без настройки каждый side-effect по-прежнему со спросом.
        let g = Consumer::agent().grant;
        assert!(g.needs_confirm("sessions.reply", RiskClass::Control));
        assert!(g.needs_confirm("settings.set", RiskClass::Settings));
        assert!(!g.needs_confirm("sessions.list", RiskClass::Read), "read и так без спроса");
    }

    #[test]
    fn auto_approve_is_per_id_not_per_class() {
        let g = Consumer::agent()
            .with_auto_approve(["sessions.reply".to_string()].into_iter().collect())
            .grant;
        assert!(!g.needs_confirm("sessions.reply", RiskClass::Control), "разрешён поимённо");
        // соседи по классу Control остаются со спросом — это не «выключить гейт»
        assert!(g.needs_confirm("sessions.control", RiskClass::Control));
        assert!(g.needs_confirm("settings.set", RiskClass::Settings));
    }

    #[test]
    fn auto_approve_does_not_widen_grant() {
        // Авто-одобрение снимает вопрос, но не даёт прав: denylist сильнее.
        let g = Consumer::agent()
            .with_auto_approve(["audit.query".to_string(), "stt.transcribe".to_string()].into_iter().collect())
            .grant;
        assert!(!g.allows_id("audit.query", RiskClass::Read));
        assert!(!g.allows_id("stt.transcribe", RiskClass::Control));
    }

    #[test]
    fn auto_approve_read_from_settings_shape() {
        let s = serde_json::json!({
            "grants": { "agent": { "autoApprove": ["sessions.reply", 42] } }
        });
        let ids = auto_approve_from_settings(&s, "agent");
        assert_eq!(ids.len(), 1, "мусор в списке отброшен");
        assert!(ids.contains("sessions.reply"));
        assert!(auto_approve_from_settings(&s, "plugin:x").is_empty(), "список свой у каждого");
    }

    #[test]
    fn auto_approve_absent_or_broken_is_empty() {
        for s in [
            serde_json::json!({}),
            serde_json::json!({ "grants": {} }),
            serde_json::json!({ "grants": { "agent": {} } }),
            serde_json::json!({ "grants": { "agent": { "autoApprove": "sessions.reply" } } }),
            serde_json::json!({ "grants": { "agent": "admin" } }),
        ] {
            assert!(auto_approve_from_settings(&s, "agent").is_empty(), "битая форма → дефолт: {s}");
        }
    }

    #[test]
    fn allowlist_has_user_tunables_not_security() {
        assert!(SETTINGS_ALLOWLIST.contains(&"hotkey"));
        assert!(!SETTINGS_ALLOWLIST.iter().any(|k| SECURITY_KEYS.contains(k)));
    }
}
