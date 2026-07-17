//! Настройки Jarvis: ~/.jarvis/settings.json. Битый файл → дефолты, молча.
//!
//! Загрузка мержит дефолты ⊕ диск, поэтому ДОБАВЛЕНИЕ полей безопасно (старый
//! файл без поля читается). Ломающие изменения схемы (переименование/смена
//! смысла/реструктуризация поля) — только через миграцию: подними
//! `SCHEMA_VERSION`, добавь шаг в `run_migrations`, вызови `migrate_on_startup`.
//! Политика целиком — docs/release/versioning-and-migration.md.

use serde_json::{json, Map, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::util::jarvis_dir;

/// Текущая версия схемы settings.json. Поднимать при ЛОМАЮЩИХ изменениях формата
/// (не при простом добавлении полей), добавляя шаг в `run_migrations`.
pub const SCHEMA_VERSION: u64 = 1;

pub struct Store {
    cache: Mutex<Option<Value>>,
    path: PathBuf,
}

fn defaults() -> Value {
    json!({
        "hotkey": "Command+J",
        "quietHotkey": "Command+Alt+J",
        "continueHotkey": "Command+Alt+C",
        "repeatHotkey": "Command+Alt+R",
        "muteHotkey": "Command+Alt+M",
        "selectHotkeyTemplate": "Command+Alt+{n}",
        "notifyDone": true,
        "notifyWaiting": true,
        "position": "center", // 'center' | 'corner'
        "autoResume": true,   // после сброса лимита сказать ждавшим сессиям «продолжай»
        "autoUpdate": true,   // тихо проверять и ставить обновления на старте
        "diagnostics": true,  // режим логов: тайминги/RAM/CPU/события → metrics.jsonl + jarvis.log (без текста промптов/ответов)
        // Запуск сессии прямо из Jarvis (вкладка «Запуск»). Флэт-ключи: settings_set
        // мержит лишь верхний уровень, вложенный объект затирался бы целиком.
        "launchTerminal": "terminal-app", // 'terminal-app' | 'iterm2' | 'custom'
        "launchCustomCmd": "",            // шаблон для 'custom', плейсхолдер {cmd}
        "launchProxyCmd": "",             // команда, выполняемая в терминале ПЕРЕД запуском агента (опц.)
        "launchDangerous": false,         // глобальный «опасный режим»: claude --dangerously-skip-permissions / codex YOLO
        "schemaVersion": SCHEMA_VERSION,
        "notify": {
            "content": { "branch": true, "model": false, "effort": false, "tokens": false, "time": false },
            "events":  { "done": true, "waiting": true, "limit": true },
            "ttlSec": 8
        },
    })
}

fn file() -> std::path::PathBuf {
    jarvis_dir().join("settings.json")
}

fn read_merged(path: &Path) -> Value {
    let mut merged = defaults();
    if let Ok(raw) = fs::read_to_string(path) {
        if let Ok(Value::Object(disk)) = serde_json::from_str::<Value>(&raw) {
            let m = merged.as_object_mut().unwrap();
            for (k, v) in disk {
                m.insert(k, v);
            }
        }
    }
    merged
}

/// Persist a complete settings snapshot without ever exposing a partially
/// written JSON file. The temp file lives next to the destination, so rename
/// is atomic on the target filesystem. Its mode is owner-only before any
/// settings bytes are written.
fn atomic_write(path: &Path, value: &Value) -> io::Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp_path = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = serde_json::to_string_pretty(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
        + "\n";

    let result = (|| -> io::Result<()> {
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)?;
        temp.write_all(bytes.as_bytes())?;
        temp.sync_all()?;
        drop(temp);
        fs::rename(&temp_path, path)?;

        // The renamed file already has 0600 from creation. Syncing the parent
        // makes the rename durable; a directory sync failure does not mean the
        // visible file differs from the cache, so it is deliberately best-effort.
        let _ = File::open(parent).and_then(|dir| dir.sync_all());
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
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
        Self {
            cache: Mutex::new(None),
            path: file(),
        }
    }

    #[cfg(test)]
    fn with_path(path: PathBuf) -> Self {
        Self {
            cache: Mutex::new(None),
            path,
        }
    }

    fn current_locked(&self, cache: &mut Option<Value>) -> Value {
        if let Some(value) = cache.as_ref() {
            return value.clone();
        }
        let value = read_merged(&self.path);
        *cache = Some(value.clone());
        value
    }

    /// Execute one read-modify-write transaction while holding the cache
    /// mutex. Cache advances only after the atomic rename has succeeded.
    fn update(&self, mutate: impl FnOnce(&mut Map<String, Value>)) -> Value {
        let mut cache = self.cache.lock().unwrap();
        let current = self.current_locked(&mut cache);
        let mut next = current.clone();
        mutate(next.as_object_mut().unwrap());

        match atomic_write(&self.path, &next) {
            Ok(()) => {
                *cache = Some(next.clone());
                next
            }
            Err(err) => {
                eprintln!("[jarvis] не смог записать настройки: {err}");
                current
            }
        }
    }

    /// Однократная миграция файла на старте: если версия на диске устарела —
    /// бэкап + прогон миграций + перезапись. Актуальный/отсутствующий/битый файл
    /// не трогаем. Вызывать ОДИН раз при инициализации, до чтения настроек.
    pub fn migrate_on_startup(&self) {
        let mut cache = self.cache.lock().unwrap();
        let path = &self.path;
        let Ok(raw) = fs::read_to_string(path) else { return }; // нет файла → дефолты
        let Ok(Value::Object(disk)) = serde_json::from_str::<Value>(&raw) else { return }; // битый → не трогаем
        let from = disk.get("schemaVersion").and_then(Value::as_u64).unwrap_or(0);
        if from >= SCHEMA_VERSION {
            return; // уже актуально
        }
        let backup = path.with_file_name("settings.bak.json");
        if fs::copy(path, &backup).is_ok() {
            let _ = fs::set_permissions(&backup, fs::Permissions::from_mode(0o600));
        }
        let migrated = Value::Object(run_migrations(disk, from));
        if atomic_write(path, &migrated).is_ok() {
            *cache = None; // сбросить кэш — перечитается мигрированным
            crate::log::line(&format!("[settings] миграция схемы {from} → {SCHEMA_VERSION}"));
        }
    }

    /// Настройки целиком (дефолты ⊕ диск). Значения — динамический JSON:
    /// схема расширяется плагинами, жёсткая структура тут только мешала бы.
    pub fn load(&self) -> Value {
        let mut cache = self.cache.lock().unwrap();
        self.current_locked(&mut cache)
    }

    pub fn save(&self, patch: Map<String, Value>) -> Value {
        self.update(|m| {
            for (k, v) in patch {
                m.insert(k, v);
            }
        })
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

    /// Эффективный egress-прокси для загрузок (модели, зависимости, сайдкары).
    /// Источник истины — `service.proxy` (туда сохраняет панель настроек через
    /// `service_set_proxy`); как фолбэк — верхнеуровневый `proxy` (легаси-онбординг).
    /// Пусто/отсутствует → None. Раньше читался ТОЛЬКО верхнеуровневый `proxy`, из-за
    /// чего прокси из панели не доходил до скачивания → загрузка шла напрямую и падала.
    pub fn proxy(&self) -> Option<String> {
        let all = self.load();
        let pick = |v: Option<&Value>| {
            v.and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        pick(all.pointer("/service/proxy")).or_else(|| pick(all.get("proxy")))
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

    /// Удалить верхнеуровневый ключ. Нужен онбордингу: явная запись прокси
    /// убирает легаси `proxy`, иначе пустой `service.proxy` провалится в него
    /// и очищенный пользователем прокси «воскреснет».
    pub fn remove_top(&self, key: &str) {
        self.update(|m| {
            m.remove(key);
        });
    }

    /// Установить верхнеуровневый ключ (merge поверх остального).
    pub fn set_top(&self, key: &str, value: Value) {
        let mut root = Map::new();
        root.insert(key.to_string(), value);
        self.save(root);
    }

    /// Deep-set полей в объект "voice" (не затирая остальные voice-ключи).
    pub fn set_voice(&self, patch: Map<String, Value>) {
        self.update(|root| {
            let voice = root.entry("voice").or_insert_with(|| json!({}));
            let Some(obj) = voice.as_object_mut() else { return };
            for (k, v) in patch {
                obj.insert(k, v);
            }
        });
    }

    /// Deep-set полей в объект "stt" (не затирая остальные stt-ключи).
    pub fn set_stt(&self, patch: Map<String, Value>) {
        self.update(|root| {
            let stt = root.entry("stt").or_insert_with(|| json!({}));
            let Some(obj) = stt.as_object_mut() else { return };
            for (k, v) in patch {
                obj.insert(k, v);
            }
        });
    }

    /// Deep-set полей в произвольный объект-блок верхнего уровня (инкр. 10:
    /// "wake"/"verification"), не затирая остальные ключи блока.
    pub fn set_block(&self, block: &str, patch: Map<String, Value>) {
        self.update(|root| {
            let block = root.entry(block).or_insert_with(|| json!({}));
            let Some(obj) = block.as_object_mut() else { return };
            for (k, v) in patch {
                obj.insert(k, v);
            }
        });
    }

    pub fn set_plugin(&self, id: &str, patch: Map<String, Value>) {
        self.update(|root| {
            let plugins = root.entry("plugins").or_insert_with(|| json!({}));
            let Some(plugins) = plugins.as_object_mut() else { return };
            let plugin = plugins.entry(id.to_string()).or_insert_with(|| json!({}));
            let Some(obj) = plugin.as_object_mut() else { return };
            for (k, v) in patch {
                obj.insert(k, v);
            }
        });
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

#[cfg(test)]
mod proxy_tests {
    use super::*;

    // load() отдаёт кэш, если он есть, минуя файл — подменяем его напрямую.
    fn store_with(v: Value) -> Store {
        let s = Store::new();
        *s.cache.lock().unwrap() = Some(v);
        s
    }

    #[test]
    fn reads_proxy_from_service_block() {
        // Реальный кейс бага: панель настроек пишет в service.proxy.
        let s = store_with(json!({ "service": { "proxy": "http://u:p@host:14165" } }));
        assert_eq!(s.proxy().as_deref(), Some("http://u:p@host:14165"));
    }

    #[test]
    fn falls_back_to_top_level_proxy() {
        // Легаси-онбординг писал верхнеуровневый proxy.
        let s = store_with(json!({ "proxy": "http://legacy:8080" }));
        assert_eq!(s.proxy().as_deref(), Some("http://legacy:8080"));
    }

    #[test]
    fn service_proxy_wins_over_top_level() {
        let s = store_with(json!({
            "proxy": "http://legacy:8080",
            "service": { "proxy": "http://service:9090" },
        }));
        assert_eq!(s.proxy().as_deref(), Some("http://service:9090"));
    }

    #[test]
    fn empty_or_missing_is_none() {
        assert_eq!(store_with(json!({})).proxy(), None);
        assert_eq!(store_with(json!({ "service": { "proxy": "" } })).proxy(), None);
        assert_eq!(store_with(json!({ "service": { "proxy": "   " } })).proxy(), None);
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "jarvis-settings-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_at(dir: &Path) -> Store {
        Store::with_path(dir.join("settings.json"))
    }

    #[test]
    fn remove_top_kills_legacy_proxy_resurrection() {
        // Сценарий бага: онбординг очищает прокси (service.proxy=""), но
        // остался легаси верхнеуровневый proxy — proxy() «воскрешал» его.
        let dir = temp_dir("remove-top");
        let store = store_at(&dir);
        store.set_top("proxy", Value::from("http://legacy:8080"));
        let mut service = Map::new();
        service.insert("proxy".into(), Value::from(""));
        store.set_block("service", service);
        assert_eq!(store.proxy().as_deref(), Some("http://legacy:8080"));

        store.remove_top("proxy");
        assert_eq!(store.proxy(), None, "легаси-ключ удалён — прокси очищен");
        assert!(
            store.load().get("proxy").is_none(),
            "ключ удалён и из файла, не только замаскирован"
        );
    }

    #[test]
    fn concurrent_nested_updates_are_not_lost() {
        const WRITERS: usize = 16;
        let dir = temp_dir("concurrent");
        let store = Arc::new(store_at(&dir));
        let barrier = Arc::new(Barrier::new(WRITERS));
        let threads: Vec<_> = (0..WRITERS)
            .map(|i| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut patch = Map::new();
                    patch.insert(format!("field{i}"), Value::from(i as u64));
                    barrier.wait();
                    store.set_block("concurrent", patch);
                })
            })
            .collect();

        for handle in threads {
            handle.join().unwrap();
        }

        let saved = store.load();
        for i in 0..WRITERS {
            assert_eq!(
                saved.pointer(&format!("/concurrent/field{i}")),
                Some(&Value::from(i as u64))
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn persisted_file_is_valid_json_and_owner_only() {
        let dir = temp_dir("atomic");
        let store = store_at(&dir);
        store.set_top("hotkey", Value::from("Command+K"));

        let path = dir.join("settings.json");
        let raw = fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.get("hotkey").and_then(Value::as_str), Some("Command+K"));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
                .count(),
            0
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_persist_does_not_advance_cache() {
        let dir = temp_dir("failure");
        let path = dir.join("settings.json");
        fs::create_dir(&path).unwrap(); // rename over a directory must fail
        let store = Store::with_path(path.clone());

        let mut patch = Map::new();
        patch.insert("hotkey".into(), Value::from("Command+K"));
        let returned = store.save(patch);

        assert_eq!(
            returned.get("hotkey").and_then(Value::as_str),
            defaults().get("hotkey").and_then(Value::as_str)
        );
        assert_eq!(
            store.load().get("hotkey").and_then(Value::as_str),
            defaults().get("hotkey").and_then(Value::as_str)
        );
        assert!(path.is_dir());
        assert_eq!(
            fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
                .count(),
            0
        );
        let _ = fs::remove_dir_all(dir);
    }
}
