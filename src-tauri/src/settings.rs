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
    /// Разобранные настройки + отпечаток файла, с которого они прочитаны.
    /// Отпечаток нужен потому, что settings.json правит не только приложение:
    /// `jarvis-setup remote add` дописывает узел, и человек редактирует файл
    /// руками. Без сверки приложение записало бы поверх свой устаревший
    /// снимок — то есть молча стёрло бы чужую правку.
    cache: Mutex<Option<(Value, Stamp)>>,
    path: PathBuf,
}

/// Отпечаток файла: время правки и размер. Не содержимое — читать файл ради
/// сравнения означало бы отказаться от кэша вовсе, а stat дёшев. Размер идёт
/// в пару к mtime, потому что на файловых системах с секундной гранулярностью
/// две правки внутри одной секунды имеют одинаковое время.
type Stamp = Option<(std::time::SystemTime, u64)>;

fn stamp_of(path: &Path) -> Stamp {
    let m = fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Главный модификатор приложения в терминах аксельератора Tauri.
/// macOS — `Command`, остальные — `Control`.
fn main_mod() -> &'static str {
    if cfg!(target_os = "macos") {
        "Command"
    } else {
        "Control"
    }
}

/// `concat_mod("Alt+J")` → `"Command+Alt+J"` | `"Control+Alt+J"`.
fn concat_mod(rest: &str) -> String {
    format!("{}+{rest}", main_mod())
}

fn defaults() -> Value {
    json!({
        // Главный модификатор платформенный: на macOS это Command, на Linux —
        // Control. Super на Linux принадлежит окружению рабочего стола (в GNOME
        // Super+1..4 переключает приложения дока), поэтому мы его не занимаем.
        "hotkey": concat_mod("J"),
        "quietHotkey": concat_mod("Alt+J"),
        "continueHotkey": concat_mod("Alt+C"),
        "repeatHotkey": concat_mod("Alt+R"),
        "muteHotkey": concat_mod("Alt+M"),
        "selectHotkeyTemplate": concat_mod("Alt+{n}"),
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
        // Глобальный «опасный режим» (claude --dangerously-skip-permissions /
        // codex --dangerously-bypass-approvals-and-sandbox) включён по умолчанию:
        // Jarvis запускает агентов в СВОИХ проектах, и подтверждать каждое
        // действие руками — ровно та работа, ради отсутствия которой его и
        // ставят. Выключается тумблером в «Запуске».
        "launchDangerous": true,
        // внешность (дизайн «Клевер», экран 14f «вид»)
        "theme": "auto",   // 'light' | 'dark' | 'auto' (системная)
        "paint": "clover",  // 'clover' | 'coal' | 'raspberry' | 'custom'
        "accent": "#0B6B44", // тон своей краски: остальное выводится из него
        "density": "normal", // 'compact' | 'normal' | 'roomy' — высота строк
        "radius": "normal",  // 'sharp' | 'normal' | 'soft' — скругление углов
        "scale": 1.0,        // масштаб интерфейса, 0.85..1.4
        "footerBottom": "limit", // что показывать внизу панели: 'limit' | 'spend'
        // раскладка: 'overlay' — накладка ⌘J поверх всего; 'window' — обычное
        // окно с иконкой в доке и списком слева (макет 14h)
        "mode": "window",
        "windowW": 1120,
        "windowH": 780,
        // удалённые узлы (VPS/рабочая станция): [{name, sshHost, jarvisDir}].
        // Пусто — удалённый слой выключен целиком: ни ssh-туннелей, ни поллеров.
        "remotes": [],
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

fn read_for_update(path: &Path) -> Result<Value, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(defaults()),
        Err(error) => return Err(format!("Не удалось прочитать настройки: {error}")),
    };
    let disk: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Файл настроек повреждён: {error}"))?;
    let disk = disk.as_object().ok_or("Файл настроек должен содержать JSON-объект")?;
    let mut merged = defaults();
    merged.as_object_mut().unwrap().extend(disk.clone());
    Ok(merged)
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
    pub(crate) fn with_path(path: PathBuf) -> Self {
        Self {
            cache: Mutex::new(None),
            path,
        }
    }

    fn current_locked(&self, cache: &mut Option<(Value, Stamp)>) -> Value {
        let stamp = stamp_of(&self.path);
        if let Some((value, at)) = cache.as_ref() {
            if *at == stamp {
                return value.clone();
            }
            // Файл сменился под нами — перечитываем. Иначе следующая же запись
            // (любой тумблер в панели) вернула бы файл к нашему снимку.
            crate::log::line("[settings] файл изменился снаружи — перечитываю");
        }
        let value = read_merged(&self.path);
        *cache = Some((value.clone(), stamp));
        value
    }

    /// Execute one read-modify-write transaction while holding the cache
    /// mutex. Cache advances only after the atomic rename has succeeded.
    pub(crate) fn try_update(&self, mutate: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>) -> Result<Value, String> {
        let mut cache = self.cache.lock().map_err(|_| "Хранилище настроек временно недоступно")?;
        // Writes are infrequent and already include fsync. Read strictly here:
        // a fallback cached by load() must never overwrite malformed user data.
        let current = read_for_update(&self.path)?;
        let mut next = current.clone();
        mutate(next.as_object_mut().unwrap())?;
        atomic_write(&self.path, &next).map_err(|error| format!("Не удалось сохранить настройки: {error}"))?;
        *cache = Some((next.clone(), stamp_of(&self.path)));
        Ok(next)
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

    pub fn try_save(&self, patch: Map<String, Value>) -> Result<Value, String> {
        self.try_update(|m| {
            for (k, v) in patch {
                m.insert(k, v);
            }
            Ok(())
        })
    }

    /// Compensate a failed native operation without replacing unrelated fields
    /// that another writer added meanwhile. Only already-authorized keys enter.
    pub(crate) fn try_restore_fields(&self, fields: Vec<(String, Option<Value>)>) -> Result<Value, String> {
        self.try_update(|root| {
            for (key, value) in fields {
                match value { Some(value) => { root.insert(key, value); }, None => { root.remove(&key); } }
            }
            Ok(())
        })
    }

    /// Background callers may tolerate failed persistence, but must log it.
    /// UI-facing mutations must use try_save/try_set_* and propagate the error.
    pub fn save(&self, patch: Map<String, Value>) -> Value {
        self.try_save(patch).unwrap_or_else(|error| {
            crate::log::line(&format!("[settings] background save failed: {error}"));
            self.load()
        })
    }

    fn log_background(result: Result<Value, String>) {
        if let Err(error) = result {
            crate::log::line(&format!("[settings] background save failed: {error}"));
        }
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
        Self::log_background(self.try_remove_top(key));
    }

    pub fn try_remove_top(&self, key: &str) -> Result<Value, String> {
        self.try_update(|m| {
            m.remove(key);
            Ok(())
        })
    }

    /// Установить верхнеуровневый ключ (merge поверх остального).
    pub fn set_top(&self, key: &str, value: Value) {
        Self::log_background(self.try_set_top(key, value));
    }

    pub fn try_set_top(&self, key: &str, value: Value) -> Result<Value, String> {
        let mut root = Map::new();
        root.insert(key.to_string(), value);
        self.try_save(root)
    }

    /// Deep-set полей в объект "voice" (не затирая остальные voice-ключи).
    pub fn set_voice(&self, patch: Map<String, Value>) {
        Self::log_background(self.try_set_voice(patch));
    }

    pub fn try_set_voice(&self, patch: Map<String, Value>) -> Result<Value, String> {
        self.try_set_block("voice", patch)
    }

    /// Deep-set полей в объект "stt" (не затирая остальные stt-ключи).
    pub fn set_stt(&self, patch: Map<String, Value>) {
        Self::log_background(self.try_set_stt(patch));
    }

    pub fn try_set_stt(&self, patch: Map<String, Value>) -> Result<Value, String> {
        self.try_set_block("stt", patch)
    }

    /// Deep-set полей в произвольный объект-блок верхнего уровня (инкр. 10:
    /// "wake"/"verification"), не затирая остальные ключи блока.
    pub fn set_block(&self, block: &str, patch: Map<String, Value>) {
        Self::log_background(self.try_set_block(block, patch));
    }

    pub fn try_set_block(&self, block: &str, patch: Map<String, Value>) -> Result<Value, String> {
        self.try_update(|root| {
            let replaces_proxy = block == "service" && patch.contains_key("proxy");
            let value = root.entry(block).or_insert_with(|| json!({}));
            let obj = value.as_object_mut().ok_or_else(|| format!("Раздел настроек «{block}» должен быть объектом"))?;
            obj.extend(patch);
            if replaces_proxy { root.remove("proxy"); }
            Ok(())
        })
    }

    pub fn set_plugin(&self, id: &str, patch: Map<String, Value>) {
        Self::log_background(self.try_set_plugin(id, patch));
    }

    pub fn try_set_plugin(&self, id: &str, patch: Map<String, Value>) -> Result<Value, String> {
        self.try_update(|root| {
            let plugins = root.entry("plugins").or_insert_with(|| json!({}));
            let plugins = plugins.as_object_mut().ok_or("Раздел plugins должен быть объектом")?;
            let plugin = plugins.entry(id.to_string()).or_insert_with(|| json!({}));
            let obj = plugin.as_object_mut().ok_or("Настройки плагина должны быть объектом")?;
            obj.extend(patch);
            Ok(())
        })
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

    // load() отдаёт кэш, если отпечаток файла совпал — подменяем и то, и другое.
    fn store_with(v: Value) -> Store {
        let s = Store::new();
        let stamp = stamp_of(&s.path);
        *s.cache.lock().unwrap() = Some((v, stamp));
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
    fn external_edit_is_not_overwritten_by_a_stale_cache() {
        // Реальный сценарий: `jarvis-setup remote add` дописывает узел в
        // settings.json, пока приложение работает. Приложение обязано увидеть
        // правку до своей следующей записи, иначе оно молча её сотрёт.
        let dir = temp_dir("external-edit");
        let store = store_at(&dir);
        store.set_top("theme", Value::from("dark")); // кэш прогрет и записан

        // ...кто-то правит файл мимо нас
        let path = dir.join("settings.json");
        let mut disk: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        disk["remotes"] = json!([{ "name": "vps", "sshHost": "dev@vps" }]);
        // Отпечаток — пара (mtime, размер), и здесь ловится любой из двух:
        // ключ добавился, значит размер точно другой. Пауза лишь разводит
        // mtime там, где файловая система его различает.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, serde_json::to_string_pretty(&disk).unwrap() + "\n").unwrap();

        store.set_top("quietMode", Value::from(true)); // следующая запись приложения

        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            after["remotes"][0]["name"],
            Value::from("vps"),
            "чужая правка обязана пережить нашу запись"
        );
        assert_eq!(after["quietMode"], Value::from(true), "и наша тоже");
        assert_eq!(after["theme"], Value::from("dark"), "и прежняя наша");
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
        assert_eq!(store.proxy(), None, "смена service.proxy атомарно убирает старый ключ");
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

    #[test]
    fn strict_writes_report_failure_and_preserve_malformed_external_data() {
        let dir = temp_dir("strict-failure");
        let path = dir.join("settings.json");
        let store = Store::with_path(path.clone());
        store.try_set_top("theme", json!("dark")).unwrap();
        fs::write(&path, b"{unfinished external edit").unwrap();
        let failed = store.try_set_top("theme", json!("light"));
        assert!(failed.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{unfinished external edit");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(store.try_set_stt(json!({"engine":"qwen3-0.6b"}).as_object().unwrap().clone()).is_err());
        assert!(path.is_dir());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn proxy_replacement_and_compensation_preserve_unrelated_settings() {
        let dir = temp_dir("compensate");
        let store = store_at(&dir);
        store.try_set_top("proxy", json!("http://old.invalid:8080")).unwrap();
        store.try_set_block("service", json!({"proxy":""}).as_object().unwrap().clone()).unwrap();
        assert!(store.load().get("proxy").is_none());
        store.try_set_top("density", json!("compact")).unwrap();
        store.try_restore_fields(vec![("theme".into(), Some(json!("dark")))]).unwrap();
        assert_eq!(store.load()["density"], "compact");
        assert_eq!(store.load()["theme"], "dark");
        fs::remove_dir_all(dir).unwrap();
    }
}
