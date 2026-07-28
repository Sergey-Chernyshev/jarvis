//! Токены потребителей сокета (R2). Идентичность входящего-по-сокету — по
//! токену из ~/.jarvis/tokens.json (права 0600), а НЕ по строке в теле запроса.
//! Панель (in-process) токена не требует и здесь не резолвится: Consumer::panel()
//! не выдаётся ни по какому токену (INV-PANEL).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
        Self {
            path: jarvis_dir().join("tokens.json"),
        }
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

    fn write(&self, v: &Value) -> Result<(), String> {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|err| format!("не создать каталог токенов: {err}"))?;
        let file_name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let temp_path = parent.join(format!(
            ".{file_name}.tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let bytes = serde_json::to_string_pretty(v)
            .map_err(|err| format!("не сериализовать токены: {err}"))?
            + "\n";

        let result = (|| -> Result<(), String> {
            let mut temp = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp_path)
                .map_err(|err| format!("не создать временный файл токенов: {err}"))?;
            temp.write_all(bytes.as_bytes())
                .map_err(|err| format!("не записать токены: {err}"))?;
            temp.sync_all()
                .map_err(|err| format!("не синхронизировать токены: {err}"))?;
            drop(temp);
            fs::rename(&temp_path, &self.path)
                .map_err(|err| format!("не заменить файл токенов: {err}"))?;
            let _ = File::open(parent).and_then(|dir| dir.sync_all());
            Ok(())
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    /// Сгенерировать/прочитать токен агента (идемпотентно).
    pub fn ensure_agent_token(&self) -> String {
        let mut v = self.read();
        if let Some(t) = v.get("agent").and_then(|t| t.as_str()) {
            return t.to_string();
        }
        let tok = gen_token();
        v.as_object_mut()
            .unwrap()
            .insert("agent".into(), json!(tok));
        if let Err(err) = self.write(&v) {
            crate::log::line(&format!("[tokens] agent token persist failed: {err}"));
        }
        tok
    }

    /// Выпустить или обновить токен внешнего плагина. Identity стабильна между
    /// рестартами, классы всегда заменяются текущим least-privilege manifest.
    pub fn ensure_plugin_token(&self, id: &str, classes: &[RiskClass]) -> Result<String, String> {
        if id.is_empty() {
            return Err("plugin id обязателен".into());
        }
        let mut v = self.read();
        if !v.is_object() {
            v = json!({});
        }
        let existing = v
            .get("plugins")
            .and_then(Value::as_object)
            .and_then(|plugins| plugins.get(id))
            .and_then(|entry| entry.get("token"))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        let token = existing.unwrap_or_else(gen_token);

        let mut class_names = Vec::new();
        for class in classes {
            let allowed = match class {
                RiskClass::Read => Some("read"),
                RiskClass::Control => Some("control"),
                RiskClass::Settings => Some("settings"),
                RiskClass::Admin => None,
            };
            if let Some(name) = allowed {
                if !class_names.contains(&name) {
                    class_names.push(name);
                }
            }
        }

        let root = v.as_object_mut().unwrap();
        let plugins = root.entry("plugins").or_insert_with(|| json!({}));
        if !plugins.is_object() {
            *plugins = json!({});
        }
        plugins.as_object_mut().unwrap().insert(
            id.to_string(),
            json!({ "token": token, "classes": class_names }),
        );
        self.write(&v)?;
        Ok(token)
    }

    /// Отозвать plugin identity. Повторный revoke безопасен и не трогает agent.
    pub fn revoke_plugin(&self, id: &str) -> Result<bool, String> {
        let mut v = self.read();
        let Some(plugins) = v.get_mut("plugins").and_then(Value::as_object_mut) else {
            return Ok(false);
        };
        let removed = plugins.remove(id).is_some();
        if removed {
            self.write(&v)?;
        }
        Ok(removed)
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
        assert!(
            !c.grant.allows(RiskClass::Control),
            "least-privilege: только read"
        );
    }

    #[test]
    fn plugin_token_is_stable_updates_classes_and_uses_private_file() {
        use std::os::unix::fs::PermissionsExt;

        let p = tmp();
        let s = TokenStore::at(p.clone());
        let t1 = s
            .ensure_plugin_token("agent-vm", &[RiskClass::Read])
            .unwrap();
        let t2 = s
            .ensure_plugin_token("agent-vm", &[RiskClass::Read, RiskClass::Control])
            .unwrap();

        assert_eq!(t1, t2, "повторный выпуск сохраняет identity");
        let c = s.resolve(&t2).unwrap();
        assert!(c.grant.allows(RiskClass::Control));
        assert_eq!(
            std::fs::metadata(p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn revoke_plugin_invalidates_token_without_touching_agent() {
        let s = TokenStore::at(tmp());
        let agent = s.ensure_agent_token();
        let plugin = s
            .ensure_plugin_token("agent-vm", &[RiskClass::Read])
            .unwrap();

        assert!(s.revoke_plugin("agent-vm").unwrap());
        assert!(s.resolve(&plugin).is_none());
        assert_eq!(s.resolve(&agent).unwrap().id, "agent");
        assert!(!s.revoke_plugin("agent-vm").unwrap());
    }

    #[test]
    fn plugin_token_never_persists_admin_class() {
        let s = TokenStore::at(tmp());
        let token = s
            .ensure_plugin_token("agent-vm", &[RiskClass::Read, RiskClass::Admin])
            .unwrap();

        let c = s.resolve(&token).unwrap();
        assert!(!c.grant.allows(RiskClass::Admin));
    }
}
