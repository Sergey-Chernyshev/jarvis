//! Настройки Jarvis: ~/.jarvis/settings.json. Битый файл → дефолты, молча.
//!
//! Формат на диске совпадает с Electron-версией один в один — миграция не нужна:
//! { hotkey, notifyDone, notifyWaiting, position, autoResume, plugins: { id: {...} } }

use serde_json::{json, Map, Value};
use std::fs;
use std::sync::Mutex;

use crate::util::jarvis_dir;

pub struct Store {
    cache: Mutex<Option<Value>>,
}

fn defaults() -> Value {
    json!({
        "hotkey": "Command+J",
        "notifyDone": true,
        "notifyWaiting": true,
        "position": "center", // 'center' | 'corner'
        "autoResume": true,   // после сброса лимита сказать ждавшим сессиям «продолжай»
    })
}

fn file() -> std::path::PathBuf {
    jarvis_dir().join("settings.json")
}

impl Store {
    pub fn new() -> Self {
        Self { cache: Mutex::new(None) }
    }

    /// Настройки целиком (дефолты ⊕ диск). Значения — динамический JSON:
    /// схема расширяется плагинами, жёсткая структура тут только мешала бы.
    pub fn load(&self) -> Value {
        let mut cache = self.cache.lock().unwrap();
        if let Some(v) = cache.as_ref() {
            return v.clone();
        }
        let mut merged = defaults();
        if let Ok(raw) = fs::read_to_string(file()) {
            if let Ok(Value::Object(disk)) = serde_json::from_str::<Value>(&raw) {
                let m = merged.as_object_mut().unwrap();
                for (k, v) in disk {
                    m.insert(k, v);
                }
            }
        }
        *cache = Some(merged.clone());
        merged
    }

    pub fn save(&self, patch: Map<String, Value>) -> Value {
        let mut merged = self.load();
        {
            let m = merged.as_object_mut().unwrap();
            for (k, v) in patch {
                m.insert(k, v);
            }
        }
        *self.cache.lock().unwrap() = Some(merged.clone());
        let _ = fs::create_dir_all(jarvis_dir());
        if let Err(err) = fs::write(file(), serde_json::to_string_pretty(&merged).unwrap() + "\n") {
            eprintln!("[jarvis] не смог записать настройки: {err}");
        }
        merged
    }

    /* -------- типизированные шорткаты для частых полей -------- */

    pub fn bool(&self, key: &str) -> bool {
        self.load().get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn string(&self, key: &str) -> String {
        self.load()
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// Настройки плагина: дефолты ⊕ plugins.<id> из файла.
    pub fn plugin(&self, id: &str, defaults: Value) -> Value {
        let mut out = defaults;
        if let Some(saved) = self.load().pointer(&format!("/plugins/{id}")) {
            if let (Some(dst), Some(src)) = (out.as_object_mut(), saved.as_object()) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
        }
        out
    }

    /// Установить верхнеуровневый ключ (merge поверх остального).
    pub fn set_top(&self, key: &str, value: Value) {
        let mut root = Map::new();
        root.insert(key.to_string(), value);
        self.save(root);
    }

    /// Deep-set полей в объект "voice" (не затирая остальные voice-ключи).
    pub fn set_voice(&self, patch: Map<String, Value>) {
        let all = self.load();
        let mut voice = all.get("voice").cloned().unwrap_or_else(|| json!({}));
        if let Some(obj) = voice.as_object_mut() {
            for (k, v) in patch {
                obj.insert(k, v);
            }
        }
        let mut root = Map::new();
        root.insert("voice".into(), voice);
        self.save(root);
    }

    /// Deep-set полей в объект "stt" (не затирая остальные stt-ключи).
    pub fn set_stt(&self, patch: Map<String, Value>) {
        let all = self.load();
        let mut stt = all.get("stt").cloned().unwrap_or_else(|| json!({}));
        if let Some(obj) = stt.as_object_mut() {
            for (k, v) in patch {
                obj.insert(k, v);
            }
        }
        let mut root = Map::new();
        root.insert("stt".into(), stt);
        self.save(root);
    }

    pub fn set_plugin(&self, id: &str, patch: Map<String, Value>) {
        let all = self.load();
        let mut plugins = all.get("plugins").cloned().unwrap_or_else(|| json!({}));
        let entry = plugins
            .as_object_mut()
            .unwrap()
            .entry(id.to_string())
            .or_insert_with(|| json!({}));
        if let Some(obj) = entry.as_object_mut() {
            for (k, v) in patch {
                obj.insert(k, v);
            }
        }
        let mut root = Map::new();
        root.insert("plugins".into(), plugins);
        self.save(root);
    }
}
