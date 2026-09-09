//! Токены потребителей сокета (R2). Идентичность входящего-по-сокету — по
//! токену из ~/.jarvis/tokens.json (права 0600), а НЕ по строке в теле запроса.
//! Панель (in-process) токена не требует и здесь не резолвится: Consumer::panel()
//! не выдаётся ни по какому токену (INV-PANEL).

use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;

use serde_json::{json, Value};

use super::contract::RiskClass;
use super::grant::{auto_approve_from_settings, Consumer};
use crate::util::jarvis_dir;

/// Доступ к таблице токенов. Файл читается на каждый резолв (вызовы редки).
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new() -> Self {
        Self { path: jarvis_dir().join("tokens.json") }
    }

    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn read(&self) -> Value {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({}))
    }

    fn write(&self, v: &Value) {
        use std::os::unix::fs::PermissionsExt;
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&self.path, serde_json::to_string_pretty(v).unwrap_or_default() + "\n")
            .is_ok()
        {
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
    }

    /// Сгенерировать/прочитать токен агента (идемпотентно).
    pub fn ensure_agent_token(&self) -> String {
        let mut v = self.read();
        if let Some(t) = v.get("agent").and_then(|t| t.as_str()) {
            return t.to_string();
        }
        let tok = gen_token();
        v.as_object_mut().unwrap().insert("agent".into(), json!(tok));
        self.write(&v);
        tok
    }

    /// Поимённое авто-одобрение потребителя из settings.json (лежит рядом с
    /// tokens.json). Читаем на резолве, а не на старте: правку настроек видно со
    /// следующего вызова, без перезапуска демона.
    fn auto_approve(&self, consumer: &str) -> HashSet<String> {
        let settings = std::fs::read_to_string(self.path.with_file_name("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({}));
        auto_approve_from_settings(&settings, consumer)
    }

    /// Выпустить (или перевыпустить) токен плагина с грантом из манифеста.
    /// Каждое включение — новый токен: старый, утёкший из выключенного плагина,
    /// после этого не работает.
    pub fn issue_plugin(&self, id: &str, classes: &[RiskClass]) -> String {
        let mut v = self.read();
        let tok = gen_token();
        let root = v.as_object_mut().expect("tokens.json — объект");
        let plugins = root.entry("plugins").or_insert_with(|| json!({}));
        if let Some(obj) = plugins.as_object_mut() {
            let names: Vec<Value> =
                classes.iter().map(|c| Value::from(c.as_str())).collect();
            obj.insert(id.to_string(), json!({ "token": tok, "classes": names }));
        }
        self.write(&v);
        tok
    }

    /// Отозвать токен плагина: грант перестаёт существовать вместе с ним.
    pub fn revoke_plugin(&self, id: &str) {
        let mut v = self.read();
        let changed = v
            .get_mut("plugins")
            .and_then(|p| p.as_object_mut())
            .map(|o| o.remove(id).is_some())
            .unwrap_or(false);
        if changed {
            self.write(&v);
        }
    }

    /// Резолв токена в потребителя. Неизвестный/пустой → None. panel НИКОГДА.
    pub fn resolve(&self, token: &str) -> Option<Consumer> {
        if token.is_empty() {
            return None;
        }
        let v = self.read();
        if v.get("agent").and_then(|t| t.as_str()) == Some(token) {
            return Some(Consumer::agent().with_auto_approve(self.auto_approve("agent")));
        }
        // плагины: { "plugins": { "<id>": { "token": "...", "classes": ["read",...] } } }
        let plugins = v.get("plugins").and_then(|p| p.as_object())?;
        for (id, entry) in plugins {
            if entry.get("token").and_then(|t| t.as_str()) == Some(token) {
                let classes = parse_classes(entry.get("classes"));
                return Some(Consumer::plugin(id, &classes));
            }
        }
        None
    }
}

fn parse_classes(v: Option<&Value>) -> Vec<RiskClass> {
    let mut out = Vec::new();
    if let Some(arr) = v.and_then(|v| v.as_array()) {
        for c in arr {
            match c.as_str() {
                Some("read") => out.push(RiskClass::Read),
                Some("control") => out.push(RiskClass::Control),
                Some("settings") => out.push(RiskClass::Settings),
                _ => {} // admin и мусор игнорируем — least-privilege
            }
        }
    }
    out
}

/// 32 байта из /dev/urandom → hex (64 симв.). Без новых зависимостей.
fn gen_token() -> String {
    // Fail-closed (как gen_nonce в confirm_panel): недоступный/нечитаемый
    // /dev/urandom оставил бы буфер нулями, и в tokens.json легли бы 64 нуля —
    // предсказуемый пароль, по которому любой предъявитель получает права
    // агента. Лучше не запуститься, чем выдать такой.
    let mut buf = [0u8; 32];
    let mut f = std::fs::File::open("/dev/urandom").expect("/dev/urandom недоступен");
    f.read_exact(&mut buf).expect("/dev/urandom не читается");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("jarvis-tok-{}-{n}.json", std::process::id()))
    }

    #[test]
    fn agent_token_is_stable_and_resolves() {
        let s = TokenStore::at(tmp());
        let t1 = s.ensure_agent_token();
        let t2 = s.ensure_agent_token();
        assert_eq!(t1, t2, "токен идемпотентен");
        assert_eq!(t1.len(), 64, "32 байта hex");
        // Нули — признак того, что /dev/urandom не прочитался: такой «токен»
        // угадывается с первой попытки и даёт права агента кому угодно.
        assert_ne!(t1, "0".repeat(64), "предсказуемый токен недопустим");
        assert_ne!(gen_token(), gen_token(), "каждый токен свой");
        let c = s.resolve(&t1).expect("агентский токен резолвится");
        assert_eq!(c.id, "agent");
    }

    #[test]
    fn unknown_and_empty_token_rejected() {
        let s = TokenStore::at(tmp());
        s.ensure_agent_token();
        assert!(s.resolve("deadbeef").is_none());
        assert!(s.resolve("").is_none());
    }

    #[test]
    fn no_token_yields_panel_consumer() {
        // INV-PANEL: ни один токен не даёт грант панели.
        let s = TokenStore::at(tmp());
        let agent = s.ensure_agent_token();
        assert_ne!(s.resolve(&agent).unwrap().id, "panel");
    }

    /// Свой каталог: авто-одобрение читается из settings.json РЯДОМ с токенами.
    fn tmp_home() -> PathBuf {
        let dir = tmp().with_extension("dir");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn agent_gets_auto_approve_from_settings() {
        let dir = tmp_home();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"grants":{"agent":{"autoApprove":["sessions.reply"]}}}"#,
        )
        .unwrap();
        let s = TokenStore::at(dir.join("tokens.json"));
        let t = s.ensure_agent_token();
        let c = s.resolve(&t).unwrap();
        assert!(!c.grant.needs_confirm("sessions.reply", RiskClass::Control), "разрешено человеком");
        assert!(c.grant.needs_confirm("sessions.control", RiskClass::Control), "остальное со спросом");
    }

    #[test]
    fn agent_without_settings_asks_as_before() {
        // Нет файла настроек — поведение прежнее: подтверждение на каждый side-effect.
        let s = TokenStore::at(tmp_home().join("tokens.json"));
        let t = s.ensure_agent_token();
        let c = s.resolve(&t).unwrap();
        assert!(c.grant.needs_confirm("sessions.reply", RiskClass::Control));
    }

    #[test]
    fn issued_plugin_token_resolves_and_revokes() {
        let s = TokenStore::at(tmp());
        s.ensure_agent_token();
        let tok = s.issue_plugin("weather", &[RiskClass::Read, RiskClass::Control]);
        let c = s.resolve(&tok).expect("выпущенный токен резолвится");
        assert_eq!(c.id, "plugin:weather");
        assert!(c.grant.allows(RiskClass::Control));
        assert!(!c.grant.allows(RiskClass::Admin));

        // перевыпуск обесценивает прежний токен
        let tok2 = s.issue_plugin("weather", &[RiskClass::Read]);
        assert_ne!(tok, tok2);
        assert!(s.resolve(&tok).is_none(), "старый токен больше не действует");

        s.revoke_plugin("weather");
        assert!(s.resolve(&tok2).is_none(), "после отзыва грант не существует");
        assert!(s.resolve(&s.ensure_agent_token()).is_some(), "агента не задели");
    }

    #[test]
    fn plugin_token_resolves_least_privilege() {
        let p = tmp();
        std::fs::write(
            &p,
            r#"{"agent":"aaaa","plugins":{"weather":{"token":"bbbb","classes":["read"]}}}"#,
        )
        .unwrap();
        let s = TokenStore::at(p);
        let c = s.resolve("bbbb").expect("плагин резолвится");
        assert_eq!(c.id, "plugin:weather");
        assert!(c.grant.allows(RiskClass::Read));
        assert!(!c.grant.allows(RiskClass::Control), "least-privilege: только read");
    }
}
