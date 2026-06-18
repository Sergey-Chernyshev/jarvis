//! Гранты потребителей и запрет самоэскалации (§7, §8 слой b).
//!
//! Грант = какие классы капабилити разрешены потребителю и нужна ли
//! конфирмация side-effect. Внутренний агент — такой же грантодержатель,
//! как плагин (догфудинг). `Admin` не выдаётся никому, кроме пользователя.

use std::collections::HashSet;

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
}

impl Grant {
    pub fn allows(&self, class: RiskClass) -> bool {
        self.classes.contains(&class)
    }
    /// Класс разрешён И капабилити не в поимённом denylist.
    pub fn allows_id(&self, id: &str, class: RiskClass) -> bool {
        self.allows(class) && !self.denied_ids.contains(id)
    }
    /// Нужна ли конфирмация для этого класса при этом гранте.
    pub fn needs_confirm(&self, class: RiskClass) -> bool {
        class.is_side_effect() && self.confirm == ConfirmPolicy::Always
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
                denied_ids: ["audit.query"].into_iter().collect(),
            },
        }
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
            },
        }
    }
}

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
    fn allowlist_has_user_tunables_not_security() {
        assert!(SETTINGS_ALLOWLIST.contains(&"hotkey"));
        assert!(!SETTINGS_ALLOWLIST.iter().any(|k| SECURITY_KEYS.contains(k)));
    }
}
