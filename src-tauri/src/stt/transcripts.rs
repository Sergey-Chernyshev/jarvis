//! История голосового ввода: распознанные фразы (диктовка F8 + разговоры
//! «Hey Jarvis») с временем, источником и стабильным id. Для страницы «История
//! голосового ввода» + копирование/преобразование.
//!
//! ПЕРСИСТ: по явному выбору пользователя пишем на диск
//! (`~/.jarvis[-dev]/voice-history.json`) — раньше было in-memory. `new()`
//! оставлен чисто-памятным (без диска) для тестов/фолбэка; демон — `load()`.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Safety-cap: храним «всё», но не даём файлу расти бесконечно (старое вытесняется).
const CAP: usize = 5000;

static PRIVATE_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Атомарно заменить чувствительный файл через приватный временный inode в том
/// же каталоге. `pub(crate)` — этот же примитив нужен аудио и памяти разговора.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("jarvis-data");

    for _ in 0..16 {
        let id = PRIVATE_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".{file_name}.tmp-{}-{id}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = match options.open(&temp) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, path)?;
            let _ = std::fs::File::open(parent).and_then(|dir| dir.sync_all());
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        return result;
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "не удалось создать временный приватный файл",
    ))
}

/// Одна распознанная реплика.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Transcript {
    /// Стабильный id (для удаления по одной). Монотонный.
    #[serde(default)]
    pub id: u64,
    /// Распознанный текст.
    pub text: String,
    /// Original ASR output, before dictionary substitutions and formatting.
    /// Missing on older entries; never reconstructed from the formatted text.
    #[serde(rename = "rawText", default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    /// Unix-время (секунды) распознавания.
    pub ts: u64,
    /// Источник: "dictation" (F8) | "wake" (Hey Jarvis).
    pub source: String,
    /// Применённое умное преобразование (стиль) — для зелёного тега в истории.
    #[serde(
        rename = "appliedStyle",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub applied_style: Option<String>,
    /// Есть сохранённое сжатое аудио этой диктовки → можно перегенерировать
    /// распознавание (см. `stt::audio_store`). Для wake/конвo — false.
    #[serde(rename = "hasAudio", default)]
    pub has_audio: bool,
}

/// Формат файла персиста.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Persisted {
    items: Vec<Transcript>,
}

/// Потокобезопасная история реплик. Новые — в начало (front). Персист на диск,
/// если задан путь (`load`/`load_from`); `new()` — чисто-памятный.
pub struct Transcripts {
    items: Mutex<VecDeque<Transcript>>,
    next_id: AtomicU64,
    /// Путь персиста (None → in-memory, не пишем на диск).
    path: Option<PathBuf>,
}

impl Default for Transcripts {
    fn default() -> Self {
        Self::new()
    }
}

impl Transcripts {
    /// Чисто-памятная история (без диска) — для тестов/фолбэка.
    pub fn new() -> Self {
        Transcripts {
            items: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
            path: None,
        }
    }

    /// Путь персиста по умолчанию (каталог демона: dev/prod раздельно).
    pub fn default_path() -> PathBuf {
        crate::util::jarvis_dir().join("voice-history.json")
    }

    /// Загрузить из дефолтного пути (персист включён). Демон использует это.
    pub fn load() -> Self {
        Self::load_from(Self::default_path())
    }

    /// Загрузить из заданного пути (тестируемо). Битый/отсутствующий файл → пусто.
    /// next_id продолжается с max(id)+1, чтобы id не переиспользовались.
    pub fn load_from(path: PathBuf) -> Self {
        let mut dq = VecDeque::new();
        let mut max_id = 0u64;
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(p) = serde_json::from_slice::<Persisted>(&bytes) {
                for t in p.items.into_iter().take(CAP) {
                    max_id = max_id.max(t.id);
                    dq.push_back(t);
                }
            }
        }
        Transcripts {
            items: Mutex::new(dq),
            next_id: AtomicU64::new(max_id + 1),
            path: Some(path),
        }
    }

    /// Persist before publishing edits/deletions. Memory-only stores are valid
    /// but do not count as a durable save in a dictation recovery notification.
    fn persist(&self, items: &VecDeque<Transcript>) -> std::io::Result<bool> {
        let Some(path) = &self.path else {
            return Ok(false);
        };
        let p = Persisted {
            items: items.iter().cloned().collect(),
        };
        let bytes = serde_json::to_vec(&p)?;
        write_private_atomic(path, &bytes)?;
        Ok(true)
    }

    fn save(&self, items: &VecDeque<Transcript>) -> bool {
        match self.persist(items) {
            Ok(saved) => saved,
            Err(error) => {
                crate::log::line(&format!("[dictation] history save: {error}"));
                false
            }
        }
    }

    /// Добавить реплику (с текущим временем + новым id). Пустой текст игнорируется.
    pub fn push(&self, text: &str, source: &str) -> u64 {
        self.push_styled(text, source, None, false)
    }

    /// Добавить реплику с пометкой применённого умного преобразования и наличия
    /// сохранённого аудио. Возвращает id новой реплики (0 — если текст пуст и не
    /// добавлена), чтобы вызывающий (диктовка) сохранил аудио под этим id.
    pub fn push_styled(
        &self,
        text: &str,
        source: &str,
        applied: Option<&str>,
        has_audio: bool,
    ) -> u64 {
        self.push_styled_with_status(text, source, applied, has_audio)
            .0
    }

    /// Return the id and whether history was actually written to disk. An
    /// in-memory entry alone must not produce a misleading "saved" HUD label.
    pub fn push_styled_with_status(
        &self,
        text: &str,
        source: &str,
        applied: Option<&str>,
        has_audio: bool,
    ) -> (u64, bool) {
        self.push_with_raw(text, source, applied, has_audio, None)
    }

    pub fn push_dictated(
        &self,
        text: &str,
        raw: &str,
        applied: Option<&str>,
        has_audio: bool,
    ) -> (u64, bool) {
        self.push_with_raw(text, "dictation", applied, has_audio, Some(raw))
    }

    fn push_with_raw(
        &self,
        text: &str,
        source: &str,
        applied: Option<&str>,
        has_audio: bool,
        raw: Option<&str>,
    ) -> (u64, bool) {
        let text = text.trim();
        if text.is_empty() {
            return (0, false);
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut g = match self.items.lock() {
            Ok(g) => g,
            Err(_) => return (0, false), // отравленный лок — fail-safe, не паникуем
        };
        g.push_front(Transcript {
            id,
            text: text.to_string(),
            raw_text: raw.map(String::from),
            ts,
            source: source.to_string(),
            applied_style: applied.map(|s| s.to_string()),
            has_audio,
        });
        while g.len() > CAP {
            // выселяя реплику — чистим её аудио, чтобы оно не оставалось сиротой
            if let Some(old) = g.pop_back() {
                if old.has_audio {
                    crate::stt::audio_store::delete(old.id);
                }
            }
        }
        let saved = self.save(&g);
        (id, saved)
    }

    /// Заменить текст реплики по id (после ПЕРЕГЕНЕРАЦИИ распознавания из аудио).
    /// Сбрасывает пометку умного стиля. Ошибка диска не меняет исходную запись.
    pub fn update_text(&self, id: u64, text: &str) -> Result<String, String> {
        self.update_text_if_original(id, text, None)
    }

    /// A slow retranscription must not overwrite a manual edit made while it
    /// was processing. The comparison and durable replacement share one lock.
    pub fn update_text_if_original(
        &self,
        id: u64,
        text: &str,
        original: Option<&str>,
    ) -> Result<String, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Текст записи не может быть пустым".into());
        }
        let mut items = self
            .items
            .lock()
            .map_err(|_| "История временно недоступна")?;
        let mut updated = items.clone();
        let target = updated
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or("Запись больше не существует")?;
        if original.is_some_and(|original| original != target.text) {
            return Err("Запись изменена во время распознавания. Ваши изменения сохранены; повторите распознавание при необходимости.".into());
        }
        target.text = text.to_string();
        target.applied_style = None;
        self.persist(&updated)
            .map_err(|error| format!("Не удалось сохранить историю: {error}"))?;
        *items = updated;
        Ok(text.to_string())
    }

    /// Все реплики (новые первыми) — для UI.
    pub fn list(&self) -> Vec<Transcript> {
        self.items
            .lock()
            .map(|g| g.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Удалить одну реплику по id. true — была найдена.
    pub fn remove(&self, id: u64) -> Result<bool, String> {
        let mut items = self
            .items
            .lock()
            .map_err(|_| "История временно недоступна")?;
        let Some(target) = items.iter().find(|item| item.id == id) else {
            return Ok(false);
        };
        let had_audio = target.has_audio;
        let mut updated = items.clone();
        updated.retain(|item| item.id != id);
        self.persist(&updated)
            .map_err(|error| format!("Не удалось сохранить историю: {error}"))?;
        *items = updated;
        if had_audio {
            crate::stt::audio_store::delete(id);
        }
        Ok(true)
    }

    /// Очистить историю (и персист).
    pub fn clear(&self) -> Result<(), String> {
        let mut items = self
            .items
            .lock()
            .map_err(|_| "История временно недоступна")?;
        self.persist(&VecDeque::new())
            .map_err(|error| format!("Не удалось сохранить историю: {error}"))?;
        for item in items.iter().filter(|item| item.has_audio) {
            crate::stt::audio_store::delete(item.id);
        }
        items.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dictated_raw_text_survives_formatting_persistence_and_later_edit() {
        let path = tmp("raw-recognition");
        let _ = std::fs::remove_file(&path);
        let history = Transcripts::load_from(path.clone());
        let (id, saved) = history.push_dictated(
            "Проверь Jarvis.\n\nНе меняй 12,50 RUB.",
            "проверь джарвис новый абзац не меняй 12,50 RUB",
            Some("clean"),
            false,
        );
        assert!(saved);
        history.update_text(id, "Ручная правка").unwrap();
        let loaded = Transcripts::load_from(path.clone()).list();
        assert_eq!(loaded[0].text, "Ручная правка");
        assert_eq!(
            loaded[0].raw_text.as_deref(),
            Some("проверь джарвис новый абзац не меняй 12,50 RUB")
        );
        let legacy: Transcript =
            serde_json::from_str(r#"{"id":1,"text":"старый","ts":0,"source":"dictation"}"#)
                .unwrap();
        assert!(legacy.raw_text.is_none());
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    fn file_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "jarvis-voicehist-{}-{}.json",
            std::process::id(),
            tag
        ))
    }

    #[test]
    fn push_then_list_newest_first_with_ids() {
        let t = Transcripts::new();
        t.push("привет", "dictation");
        t.push("мир", "wake");
        let l = t.list();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].text, "мир");
        assert_eq!(l[0].source, "wake");
        assert_eq!(l[1].text, "привет");
        assert!(
            l[0].id != l[1].id && l[0].id > l[1].id,
            "id уникальны, новее → больший"
        );
    }

    #[test]
    fn empty_text_ignored() {
        let t = Transcripts::new();
        t.push("   ", "dictation");
        t.push("", "dictation");
        assert!(t.list().is_empty());
    }

    #[test]
    fn delivery_save_status_distinguishes_disk_and_memory() {
        let (id, saved) =
            Transcripts::new().push_styled_with_status("текст", "dictation", None, false);
        assert!(id > 0);
        assert!(!saved);
        let path = tmp("delivery-status");
        let t = Transcripts::load_from(path.clone());
        let (_, saved) = t.push_styled_with_status("текст", "dictation", None, false);
        assert!(saved);
        assert_eq!(Transcripts::load_from(path.clone()).list()[0].text, "текст");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn delivery_reports_failed_disk_write_but_keeps_recoverable_text() {
        let parent = tmp("not-a-directory");
        std::fs::write(&parent, b"file").unwrap();
        let t = Transcripts::load_from(parent.join("history.json"));
        let (id, saved) = t.push_styled_with_status("не терять", "dictation", None, false);
        assert!(id > 0);
        assert!(!saved);
        assert_eq!(t.list()[0].text, "не терять");
        let _ = std::fs::remove_file(parent);
    }

    #[test]
    fn persist_round_trip_and_next_id_continues() {
        let path = tmp("roundtrip");
        let _ = std::fs::remove_file(&path);
        {
            let t = Transcripts::load_from(path.clone());
            t.push("первая", "dictation");
            t.push("вторая", "wake");
        }
        let t2 = Transcripts::load_from(path.clone());
        let l = t2.list();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].text, "вторая");
        t2.push("третья", "dictation");
        let ids: std::collections::HashSet<u64> = t2.list().iter().map(|x| x.id).collect();
        assert_eq!(ids.len(), 3, "id не переиспользуются после load");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remove_by_id_persists() {
        let path = tmp("remove");
        let _ = std::fs::remove_file(&path);
        let t = Transcripts::load_from(path.clone());
        t.push("оставить", "dictation");
        t.push("удалить", "wake");
        let target = t.list().iter().find(|x| x.text == "удалить").unwrap().id;
        assert!(t.remove(target).unwrap());
        assert!(!t.remove(target).unwrap(), "повторное удаление → false");
        let t2 = Transcripts::load_from(path.clone());
        assert_eq!(t2.list().len(), 1);
        assert_eq!(t2.list()[0].text, "оставить");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clear_empties_and_persists() {
        let path = tmp("clear");
        let _ = std::fs::remove_file(&path);
        let t = Transcripts::load_from(path.clone());
        t.push("x", "dictation");
        t.clear().unwrap();
        assert!(t.list().is_empty());
        assert!(Transcripts::load_from(path.clone()).list().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn failed_edit_delete_and_clear_preserve_recoverable_record() {
        let path = tmp("transaction-failure");
        let _ = std::fs::remove_file(&path);
        let history = Transcripts::load_from(path.clone());
        let id = history.push_styled("исходный текст", "dictation", Some("clean"), false);
        // A directory occupying the destination makes the real atomic rename
        // fail even when this test runs as a privileged user.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(history.update_text(id, "новый текст").is_err());
        assert!(history.remove(id).is_err());
        assert!(history.clear().is_err());
        let items = history.list();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "исходный текст");
        assert_eq!(items[0].applied_style.as_deref(), Some("clean"));
        std::fs::remove_dir(&path).unwrap();
        assert_eq!(
            history.update_text(id, "  восстановлен  ").unwrap(),
            "восстановлен"
        );
        let saved = Transcripts::load_from(path.clone()).list();
        assert_eq!(saved[0].text, "восстановлен");
        assert!(saved[0].applied_style.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn slow_retranscription_cannot_recreate_or_overwrite_a_changed_record() {
        let history = Transcripts::new();
        let id = history.push("исходный", "dictation");
        history
            .update_text(id, "пользователь отредактировал")
            .unwrap();
        assert!(history
            .update_text_if_original(id, "поздний результат", Some("исходный"))
            .is_err());
        assert_eq!(history.list()[0].text, "пользователь отредактировал");
        history.remove(id).unwrap();
        assert!(history.update_text(id, "поздний результат").is_err());
        assert!(history.list().is_empty());
    }

    #[test]
    fn empty_manual_edit_preserves_record() {
        let history = Transcripts::new();
        let id = history.push("оставить", "dictation");
        assert!(history.update_text(id, "  \n ").is_err());
        assert_eq!(history.list()[0].text, "оставить");
    }

    #[test]
    fn load_missing_or_corrupt_is_empty() {
        assert!(
            Transcripts::load_from(PathBuf::from("/nonexistent/jarvis/x.json"))
                .list()
                .is_empty()
        );
        let path = tmp("corrupt");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(Transcripts::load_from(path.clone()).list().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn persisted_history_replaces_public_file_with_private_complete_json() {
        use std::os::unix::fs::PermissionsExt;

        let path = tmp("private-atomic");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, br#"{"items":[]}"#).expect("seed history");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("public mode");

        let history = Transcripts::load_from(path.clone());
        history.push("секретная фраза", "dictation");

        assert_eq!(
            file_mode(&path),
            0o600,
            "текст диктовки доступен только владельцу"
        );
        let persisted: Persisted =
            serde_json::from_slice(&std::fs::read(&path).expect("read history"))
                .expect("complete JSON");
        assert_eq!(persisted.items.len(), 1);
        assert_eq!(persisted.items[0].text, "секретная фраза");
        let _ = std::fs::remove_file(&path);
    }
}
