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

/// Набор прав потребителя.
#[derive(Clone, Debug)]
pub struct Grant {
    pub classes: HashSet<RiskClass>,
    pub confirm: ConfirmPolicy,
}

impl Grant {
    pub fn allows(&self, class: RiskClass) -> bool {
        self.classes.contains(&class)
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
            grant: Grant { classes, confirm: ConfirmPolicy::Always },
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
            grant: Grant { classes, confirm: ConfirmPolicy::Never },
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
            },
        }
    }
}

/// Ключи `~/.jarvis/settings.json`, которые НИ ОДНА капабилити менять не вправе
/// (§7, запрет самоэскалации): гранты, плагины, политика гейта. Их правит
/// только пользователь напрямую через UI/конфиг.
pub const SECURITY_KEYS: &[&str] = &["grants", "plugins", "gatePolicy", "capability"];
