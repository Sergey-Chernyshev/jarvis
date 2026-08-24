//! Манифест плагина — данные, из которых ядро строит всё остальное: тумблер,
//! настройки, грант, пункты трея, меню голосовых скилов (спека
//! `2026-08-19-everything-is-plugin-design.md` §4).
//!
//! Форма одна для обоих рантаймов: у внешнего плагина манифест лежит рядом с
//! бинарём (`manifest.json`), у встроенного собирается в коде тем же
//! `Manifest::parse` — чтобы валидация была общая, а не «у своих по-другому».

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability::contract::RiskClass;

/// Ключ настроек, которым ядро владеет само (тумблер плагина).
pub const RESERVED_SETTING: &str = "enabled";
/// Команды, которые исполняет/опрашивает ядро; плагин их не объявляет.
pub const RESERVED_COMMANDS: [&str; 3] = ["_enable", "_status", "_tray"];

/// Рантайм плагина: наш код в процессе или отдельный процесс.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Builtin,
    External,
}

impl PluginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginKind::Builtin => "builtin",
            PluginKind::External => "external",
        }
    }
}

/// Тип поля настроек = компонент, который уже есть в `ui/settings2.js`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingType {
    Toggle,
    Segmented,
    Select,
    Number,
    Text,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingOption {
    pub value: Value,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingSpec {
    pub key: String,
    #[serde(rename = "type")]
    pub ty: SettingType,
    pub title: String,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub default: Value,
    #[serde(default)]
    pub options: Vec<SettingOption>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    /// Показывать поле, только если другое поле этого же плагина истинно.
    #[serde(default)]
    pub depends: Option<String>,
}

/// Команда рантайма: кнопка в детали плагина / пункт трея / реализация скила.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    pub name: String,
    /// Подпись кнопки. None = команда вызывается не из UI (скил, трей).
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub args: Value,
}

/// Голосовой скил плагина (переезд хардкод-триплета — инкремент 6).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSpec {
    pub name: String,
    pub description: String,
    /// Класс риска скила: read — авто, control/settings — через подтверждение.
    #[serde(default = "default_risk")]
    pub risk: String,
    #[serde(default)]
    pub args: Value,
}

fn default_risk() -> String {
    "read".into()
}

/// Сущности, которые плагин публикует в реестр ядра.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProvideSpec {
    pub kind: String,
    #[serde(default)]
    pub attrs: Vec<String>,
}

/// Чужие данные, которые плагин хочет читать (грант — тумблером в UI).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsumeSpec {
    pub plugin: String,
    pub kind: String,
}

/// Как запускать внешний плагин. Путь — относительно каталога плагина.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Имя иконки из набора UI (не файл плагина — рисовать чужое не даём).
    #[serde(default)]
    pub icon: Option<String>,
    pub kind: PluginKind,
    #[serde(default)]
    pub entry: Option<Entry>,
    /// Классы риска, которые плагин просит: read / control / settings.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Конкретные капабилити ядра (least-privilege внутри классов).
    #[serde(default)]
    pub uses: Vec<String>,
    #[serde(default)]
    pub provides: Vec<ProvideSpec>,
    #[serde(default)]
    pub consumes: Vec<ConsumeSpec>,
    #[serde(default)]
    pub settings: Vec<SettingSpec>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub skills: Vec<SkillSpec>,
    #[serde(default)]
    pub tray: bool,
    /// Вкладка настроек, в которой живут настройки плагина. None = «Плагины».
    #[serde(default)]
    pub pane: Option<String>,
    /// Включён при первом запуске (наши способности — да, сторонние — нет).
    #[serde(default)]
    pub default_enabled: bool,
    /// Каталог плагина (external). Ставится загрузчиком, не приходит из JSON.
    #[serde(skip)]
    pub dir: Option<PathBuf>,
}

impl Manifest {
    /// Разобрать и провалидировать. Ошибка = плагин не грузим (статус `error`).
    pub fn parse(v: Value) -> Result<Manifest, String> {
        let m: Manifest = serde_json::from_value(v).map_err(|e| format!("манифест не разобрался: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    /// Прочитать `<dir>/manifest.json`. Внешний каталог не вправе объявить себя
    /// встроенным плагином — иначе чужой JSON получил бы доверие нашего кода.
    pub fn load_dir(dir: &Path) -> Result<Manifest, String> {
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| format!("{}: кривой JSON: {e}", path.display()))?;
        let mut m = Manifest::parse(v)?;
        if m.kind != PluginKind::External {
            return Err(format!("{}: плагин из каталога обязан быть external", path.display()));
        }
        m.dir = Some(dir.to_path_buf());
        Ok(m)
    }

    fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            || self.id.starts_with('-')
        {
            return Err(format!("кривой id плагина: '{}' (ждём [a-z0-9-])", self.id));
        }
        if self.name.trim().is_empty() {
            return Err(format!("{}: пустое имя", self.id));
        }
        if !is_semver(&self.version) {
            return Err(format!("{}: version не semver: '{}'", self.id, self.version));
        }
        match (self.kind, &self.entry) {
            (PluginKind::External, None) => {
                return Err(format!("{}: external без entry", self.id))
            }
            (PluginKind::External, Some(e)) => {
                if e.path.trim().is_empty() {
                    return Err(format!("{}: пустой entry.path", self.id));
                }
                let p = Path::new(&e.path);
                if p.is_absolute() || e.path.split('/').any(|c| c == "..") {
                    return Err(format!(
                        "{}: entry.path обязан быть внутри каталога плагина: '{}'",
                        self.id, e.path
                    ));
                }
            }
            (PluginKind::Builtin, Some(_)) => {
                return Err(format!("{}: у builtin не бывает entry", self.id))
            }
            (PluginKind::Builtin, None) => {}
        }
        let mut seen = std::collections::HashSet::new();
        for s in &self.settings {
            if s.key == RESERVED_SETTING {
                return Err(format!("{}: ключ '{RESERVED_SETTING}' занят ядром", self.id));
            }
            if s.key.trim().is_empty() {
                return Err(format!("{}: пустой ключ настройки", self.id));
            }
            if !seen.insert(s.key.as_str()) {
                return Err(format!("{}: дубль ключа настройки '{}'", self.id, s.key));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for c in &self.commands {
            if RESERVED_COMMANDS.contains(&c.name.as_str()) {
                return Err(format!("{}: команда '{}' зарезервирована ядром", self.id, c.name));
            }
            if c.name.trim().is_empty() {
                return Err(format!("{}: пустое имя команды", self.id));
            }
            if !seen.insert(c.name.as_str()) {
                return Err(format!("{}: дубль команды '{}'", self.id, c.name));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for s in &self.skills {
            if !seen.insert(s.name.as_str()) {
                return Err(format!("{}: дубль скила '{}'", self.id, s.name));
            }
        }
        Ok(())
    }

    /// Классы риска гранта. `admin` и мусор отфильтрованы молча — так же, как
    /// это делает `Consumer::plugin`: манифест просит, права выдаёт ядро.
    pub fn risk_classes(&self) -> Vec<RiskClass> {
        self.capabilities
            .iter()
            .filter_map(|c| match c.as_str() {
                "read" => Some(RiskClass::Read),
                "control" => Some(RiskClass::Control),
                "settings" => Some(RiskClass::Settings),
                _ => None,
            })
            .collect()
    }

    /// Абсолютный путь к исполняемому файлу внешнего плагина.
    pub fn entry_path(&self) -> Option<PathBuf> {
        let (dir, entry) = (self.dir.as_ref()?, self.entry.as_ref()?);
        Some(dir.join(&entry.path))
    }

    /// Дефолты настроек плагина в форме, которую понимает `settings.plugin()`.
    pub fn setting_defaults(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert(RESERVED_SETTING.into(), Value::Bool(self.default_enabled));
        for s in &self.settings {
            m.insert(s.key.clone(), s.default.clone());
        }
        Value::Object(m)
    }

}

fn is_semver(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok_builtin() -> Value {
        json!({
            "id": "sample-plugin",
            "name": "Пример",
            "version": "1.0.0",
            "kind": "builtin",
            "capabilities": ["read"],
            "settings": [
                { "key": "auto", "type": "toggle", "title": "Авто", "default": false }
            ]
        })
    }

    #[test]
    fn builtin_manifest_parses() {
        let m = Manifest::parse(ok_builtin()).expect("валидный манифест");
        assert_eq!(m.id, "sample-plugin");
        assert_eq!(m.kind, PluginKind::Builtin);
        assert_eq!(m.settings.len(), 1);
        assert!(m.entry_path().is_none());
    }

    #[test]
    fn bad_id_rejected() {
        for bad in ["", "Sample-Plugin", "sample_plugin", "sample.plugin", "-x"] {
            let mut v = ok_builtin();
            v["id"] = json!(bad);
            assert!(Manifest::parse(v).is_err(), "id '{bad}' должен отвергаться");
        }
    }

    #[test]
    fn version_must_be_semver() {
        let mut v = ok_builtin();
        v["version"] = json!("1.0");
        assert!(Manifest::parse(v).is_err());
        let mut v = ok_builtin();
        v["version"] = json!("1.2.3-beta.1");
        assert!(Manifest::parse(v).is_ok(), "пререлиз — валидный semver");
    }

    #[test]
    fn external_needs_entry_and_builtin_must_not_have_one() {
        let mut v = ok_builtin();
        v["kind"] = json!("external");
        assert!(Manifest::parse(v.clone()).is_err(), "external без entry");
        v["entry"] = json!({ "path": "bin/plug" });
        assert!(Manifest::parse(v).is_ok());

        let mut v = ok_builtin();
        v["entry"] = json!({ "path": "bin/plug" });
        assert!(Manifest::parse(v).is_err(), "builtin с entry");
    }

    #[test]
    fn entry_path_cannot_escape_plugin_dir() {
        for bad in ["/usr/bin/env", "../../etc/passwd", "bin/../../x"] {
            let mut v = ok_builtin();
            v["kind"] = json!("external");
            v["entry"] = json!({ "path": bad });
            assert!(Manifest::parse(v).is_err(), "путь '{bad}' должен отвергаться");
        }
    }

    #[test]
    fn reserved_keys_rejected() {
        let mut v = ok_builtin();
        v["settings"] = json!([{ "key": "enabled", "type": "toggle", "title": "…" }]);
        assert!(Manifest::parse(v).is_err(), "'enabled' принадлежит ядру");

        let mut v = ok_builtin();
        v["commands"] = json!([{ "name": "_enable" }]);
        assert!(Manifest::parse(v).is_err(), "'_enable' исполняет ядро");
    }

    #[test]
    fn duplicate_setting_keys_rejected() {
        let mut v = ok_builtin();
        v["settings"] = json!([
            { "key": "auto", "type": "toggle", "title": "A" },
            { "key": "auto", "type": "toggle", "title": "B" }
        ]);
        assert!(Manifest::parse(v).is_err());
    }

    #[test]
    fn admin_class_is_filtered_not_granted() {
        let mut v = ok_builtin();
        v["capabilities"] = json!(["read", "admin", "выдумка"]);
        let m = Manifest::parse(v).expect("мусор в classes не ломает манифест");
        assert_eq!(m.risk_classes(), vec![RiskClass::Read], "admin не выдаётся никому");
    }

    #[test]
    fn defaults_include_enabled_flag() {
        let m = Manifest::parse(ok_builtin()).unwrap();
        let d = m.setting_defaults();
        assert_eq!(d["enabled"], json!(false), "по умолчанию выключён");
        assert_eq!(d["auto"], json!(false));
    }

    #[test]
    fn unknown_fields_are_ignored_for_forward_compat() {
        let mut v = ok_builtin();
        v["изБудущего"] = json!({ "что-то": 1 });
        assert!(Manifest::parse(v).is_ok());
    }

    #[test]
    fn dir_manifest_must_be_external() {
        let dir = std::env::temp_dir().join(format!("jarvis-man-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("manifest.json"), ok_builtin().to_string()).unwrap();
        let err = Manifest::load_dir(&dir).unwrap_err();
        assert!(err.contains("external"), "чужой каталог не объявляет себя своим: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
