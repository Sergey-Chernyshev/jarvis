//! Токены потребителей сокета (R2). Идентичность входящего-по-сокету — по
//! токену из ~/.jarvis/tokens.json (права 0600), а НЕ по строке в теле запроса.
//! Панель (in-process) токена не требует и здесь не резолвится: Consumer::panel()
//! не выдаётся ни по какому токену (INV-PANEL).

use std::io::Read;
use std::path::PathBuf;

use serde_json::{json, Value};

use super::contract::RiskClass;
use super::grant::Consumer;
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

    /// Резолв токена в потребителя. Неизвестный/пустой → None. panel НИКОГДА.
    pub fn resolve(&self, token: &str) -> Option<Consumer> {
        if token.is_empty() {
            return None;
        }
        let v = self.read();
        if v.get("agent").and_then(|t| t.as_str()) == Some(token) {
            return Some(Consumer::agent());
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
    let mut buf = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
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
