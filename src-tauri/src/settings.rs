//! Настройки Jarvis: ~/.jarvis/settings.json. Битый файл → дефолты, молча.
//!
//! Загрузка мержит дефолты ⊕ диск, поэтому ДОБАВЛЕНИЕ полей безопасно (старый
//! файл без поля читается). Ломающие изменения схемы (переименование/смена
//! смысла/реструктуризация поля) — только через миграцию: подними
//! `SCHEMA_VERSION`, добавь шаг в `run_migrations`, вызови `migrate_on_startup`.
//! Политика целиком — docs/release/versioning-and-migration.md.

use serde_json::{json, Map, Value};
use std::fs;
use std::sync::Mutex;

use crate::util::jarvis_dir;

/// Текущая версия схемы settings.json. Поднимать при ЛОМАЮЩИХ изменениях формата
/// (не при простом добавлении полей), добавляя шаг в `run_migrations`.
pub const SCHEMA_VERSION: u64 = 1;

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
        "autoUpdate": true,   // тихо проверять и ставить обновления на старте
        "schemaVersion": SCHEMA_VERSION,
    })
}

fn file() -> std::path::PathBuf {
    jarvis_dir().join("settings.json")
}

/// Чистая миграция настроек: применяет шаги от версии `from` до SCHEMA_VERSION.
/// Идемпотентна и ТОЛЬКО ВПЕРЁД; пользовательские поля сохраняются. Каждый новый
/// ломающий формат = новый блок `if v < N { …; v = N; }` с тестом.
fn run_migrations(mut obj: Map<String, Value>, from: u64) -> Map<String, Value> {
    let mut v = from;
    if v < 1 {
        // 0 → 1: установление базовой версии схемы. Полей не меняем — прежний
        // формат уже совместим (дефолты домерживаются при загрузке).
        v = 1;
    }
    // Шаблон следующего шага:
    // if v < 2 { /* преобразование JSON */ v = 2; }
    obj.insert("schemaVersion".into(), Value::from(v));
    obj
}

impl Store {
    pub fn new() -> Self {
        Self { cache: Mutex::new(None) }
    }

    /// Однократная миграция файла на старте: если версия на диске устарела —
    /// бэкап + прогон миграций + перезапись. Актуальный/отсутствующий/битый файл
    /// не трогаем. Вызывать ОДИН раз при инициализации, до чтения настроек.
    pub fn migrate_on_startup(&self) {
        let path = file();
        let Ok(raw) = fs::read_to_string(&path) else { return }; // нет файла → дефолты
        let Ok(Value::Object(disk)) = serde_json::from_str::<Value>(&raw) else { return }; // битый → не трогаем
        let from = disk.get("schemaVersion").and_then(Value::as_u64).unwrap_or(0);
        if from >= SCHEMA_VERSION {
            return; // уже актуально
        }
        let _ = fs::copy(&path, jarvis_dir().join("settings.bak.json")); // бэкап перед изменением
        let migrated = Value::Object(run_migrations(disk, from));
        if fs::write(&path, serde_json::to_string_pretty(&migrated).unwrap() + "\n").is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            *self.cache.lock().unwrap() = None; // сбросить кэш — перечитается мигрированным
            crate::log::line(&format!("[settings] миграция схемы {from} → {SCHEMA_VERSION}"));
        }
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
        } else {
            // settings.json может хранить секрет (proxy с паролем) — закрываем права
            // только владельцу (как tokens.json). Не маскируем proxy при отдаче в UI:
            // окно онбординга префиллит и шлёт его обратно (round-trip сломался бы).
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(file(), fs::Permissions::from_mode(0o600));
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

    /// Deep-set полей в произвольный объект-блок верхнего уровня (инкр. 10:
    /// "wake"/"verification"), не затирая остальные ключи блока.
    pub fn set_block(&self, block: &str, patch: Map<String, Value>) {
        let all = self.load();
        let mut obj = all.get(block).cloned().unwrap_or_else(|| json!({}));
        if let Some(o) = obj.as_object_mut() {
            for (k, v) in patch {
                o.insert(k, v);
            }
        }
        let mut root = Map::new();
        root.insert(block.into(), obj);
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

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn v0_file_stamps_version_and_preserves_user_fields() {
        let mut m = Map::new();
        m.insert("hotkey".into(), Value::from("Command+K"));
        m.insert("notifyDone".into(), Value::from(false));
        m.insert("voice".into(), json!({ "tts": "silero" }));
        let out = run_migrations(m, 0);
        // версия проставлена
        assert_eq!(out.get("schemaVersion").and_then(Value::as_u64), Some(SCHEMA_VERSION));
        // пользовательские поля целы (настройки не теряются)
        assert_eq!(out.get("hotkey").and_then(Value::as_str), Some("Command+K"));
        assert_eq!(out.get("notifyDone").and_then(Value::as_bool), Some(false));
        assert_eq!(out.get("voice"), Some(&json!({ "tts": "silero" })));
    }

    #[test]
    fn current_version_is_idempotent() {
        let mut m = Map::new();
        m.insert("schemaVersion".into(), Value::from(SCHEMA_VERSION));
        m.insert("stt".into(), json!({ "model": "qwen3-0.6b" }));
        let out = run_migrations(m.clone(), SCHEMA_VERSION);
        assert_eq!(out.get("schemaVersion").and_then(Value::as_u64), Some(SCHEMA_VERSION));
        assert_eq!(out.get("stt"), m.get("stt"));
    }
}
