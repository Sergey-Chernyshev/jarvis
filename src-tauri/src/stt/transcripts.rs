//! История голосового ввода: распознанные фразы (диктовка F8 + разговоры
//! «Hey Jarvis») с временем, источником и стабильным id. Для страницы «История
//! голосового ввода» + копирование/преобразование.
//!
//! ПЕРСИСТ: по явному выбору пользователя пишем на диск
//! (`~/.jarvis[-dev]/voice-history.json`) — раньше было in-memory. `new()`
//! оставлен чисто-памятным (без диска) для тестов/фолбэка; демон — `load()`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Safety-cap: храним «всё», но не даём файлу расти бесконечно (старое вытесняется).
const CAP: usize = 5000;

/// Одна распознанная реплика.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Transcript {
    /// Стабильный id (для удаления по одной). Монотонный.
    #[serde(default)]
    pub id: u64,
    /// Распознанный текст.
    pub text: String,
    /// Unix-время (секунды) распознавания.
    pub ts: u64,
    /// Источник: "dictation" (F8) | "wake" (Hey Jarvis).
    pub source: String,
    /// Применённое умное преобразование (стиль) — для зелёного тега в истории.
    #[serde(rename = "appliedStyle", default, skip_serializing_if = "Option::is_none")]
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
        Transcripts { items: Mutex::new(VecDeque::new()), next_id: AtomicU64::new(1), path: None }
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
        Transcripts { items: Mutex::new(dq), next_id: AtomicU64::new(max_id + 1), path: Some(path) }
    }

    /// Записать текущее состояние на диск (best-effort; только если есть путь).
    fn save(&self, items: &VecDeque<Transcript>) {
        let Some(path) = &self.path else { return };
        let p = Persisted { items: items.iter().cloned().collect() };
        if let Ok(bytes) = serde_json::to_vec(&p) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// Добавить реплику (с текущим временем + новым id). Пустой текст игнорируется.
    pub fn push(&self, text: &str, source: &str) -> u64 {
        self.push_styled(text, source, None, false)
    }

    /// Добавить реплику с пометкой применённого умного преобразования и наличия
    /// сохранённого аудио. Возвращает id новой реплики (0 — если текст пуст и не
    /// добавлена), чтобы вызывающий (диктовка) сохранил аудио под этим id.
    pub fn push_styled(&self, text: &str, source: &str, applied: Option<&str>, has_audio: bool) -> u64 {
        let text = text.trim();
        if text.is_empty() {
            return 0;
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut g = match self.items.lock() {
            Ok(g) => g,
            Err(_) => return 0, // отравленный лок — fail-safe, не паникуем
        };
        g.push_front(Transcript {
            id,
            text: text.to_string(),
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
        self.save(&g);
        id
    }

    /// Заменить текст реплики по id (после ПЕРЕГЕНЕРАЦИИ распознавания из аудио).
    /// Сбрасывает пометку умного стиля — это сырой распознанный текст. true — найдена.
    pub fn update_text(&self, id: u64, text: &str) -> bool {
        let Ok(mut g) = self.items.lock() else { return false };
        let mut found = false;
        for t in g.iter_mut() {
            if t.id == id {
                t.text = text.trim().to_string();
                t.applied_style = None;
                found = true;
                break;
            }
        }
        if found {
            self.save(&g);
        }
        found
    }

    /// Все реплики (новые первыми) — для UI.
    pub fn list(&self) -> Vec<Transcript> {
        self.items.lock().map(|g| g.iter().cloned().collect()).unwrap_or_default()
    }

    /// Удалить одну реплику по id. true — была найдена.
    pub fn remove(&self, id: u64) -> bool {
        let Ok(mut g) = self.items.lock() else { return false };
        let before = g.len();
        let had_audio = g.iter().find(|t| t.id == id).map(|t| t.has_audio).unwrap_or(false);
        g.retain(|t| t.id != id);
        let removed = g.len() != before;
        if removed {
            if had_audio {
                crate::stt::audio_store::delete(id);
            }
            self.save(&g);
        }
        removed
    }

    /// Очистить историю (и персист).
    pub fn clear(&self) {
        if let Ok(mut g) = self.items.lock() {
            g.clear();
            self.save(&g);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("jarvis-voicehist-{}-{}.json", std::process::id(), tag))
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
        assert!(l[0].id != l[1].id && l[0].id > l[1].id, "id уникальны, новее → больший");
    }

    #[test]
    fn empty_text_ignored() {
        let t = Transcripts::new();
        t.push("   ", "dictation");
        t.push("", "dictation");
        assert!(t.list().is_empty());
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
        assert!(t.remove(target));
        assert!(!t.remove(target), "повторное удаление → false");
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
        t.clear();
        assert!(t.list().is_empty());
        assert!(Transcripts::load_from(path.clone()).list().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_or_corrupt_is_empty() {
        assert!(Transcripts::load_from(PathBuf::from("/nonexistent/jarvis/x.json")).list().is_empty());
        let path = tmp("corrupt");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(Transcripts::load_from(path.clone()).list().is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
