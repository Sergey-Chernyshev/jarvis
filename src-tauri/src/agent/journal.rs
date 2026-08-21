//! Память автономии между запусками: журнал заходов
//! (`~/.jarvis/agent-visits.json`) и ночная копилка (`~/.jarvis/agent-night.json`).
//!
//! Почему свои файлы, а не блок в settings.json. Настройки пишутся ЦЕЛИКОМ:
//! каждая правка — read-modify-write всего дерева (settings.rs, `Store::update`).
//! Журнал автономного чата ложится на каждый заход, ночная копилка — на каждое
//! непоказанное уведомление; за ночь это сотни записей. Пустить их через общий
//! файл значит поставить под тот же риск узлы, хоткеи и гранты — и потерять их
//! разом со следом ночной работы. Черновики уже увели по этой же причине
//! (`agent/drafts.rs`), здесь тот же приём.
//!
//! Почему ДВА файла, а не один. Это две разные памяти с разной судьбой: журнал
//! заходов копится и режется потолками, а копилка осушается утренней сводкой.
//! Общий файл заставил бы обоих писать чужое состояние из-под своего замка —
//! то есть терять чужую запись на каждой гонке.
//!
//! Пишем атомарно (tmp + rename), как настройки: оборванная на середине запись
//! оставила бы файл, из которого не читается НИ ОДИН заход, а не один битый.
//! Битый файл читается как «пусто»: память об автономии — не тот повод, ради
//! которого стоит отказывать окну в запуске.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::util::jarvis_dir;

/// Журнал заходов: чат → что делала цепочка и во сколько это обошлось.
pub fn visits_file() -> PathBuf {
    jarvis_dir().join("agent-visits.json")
}

/// Ночная копилка: непоказанные уведомления и отложенное до утра.
pub fn night_file() -> PathBuf {
    jarvis_dir().join("agent-night.json")
}

/// Прочитать. Нет файла или он не разбирается — пусто.
pub fn read_at<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Положить целиком и атомарно.
pub fn write_at<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("не смог создать {}: {e}", dir.display()))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let bytes = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    let out = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = out {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("не смог записать {}: {e}", path.display()));
    }
    Ok(())
}

/// Записать, если файл вообще заведён. Ошибку говорим в лог и живём дальше:
/// журнал — не настройки, вставать из-за него незачем, а молчать о потере —
/// ровно та беда, ради которой он и заводился.
pub fn save<T: Serialize>(file: Option<&PathBuf>, what: &str, value: &T) {
    let Some(path) = file else { return };
    if let Err(e) = write_at(path, value) {
        crate::log::line(&format!("[chain] {what} не записан: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::fs;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Toy {
        n: u32,
        text: String,
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("jarvis-journal-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_round_trip_through_disk_keeps_everything() {
        let dir = temp_dir("round");
        let path = dir.join("agent-visits.json");
        let toy = Toy { n: 7, text: "ночная работа".into() };
        write_at(&path, &toy).unwrap();
        assert_eq!(read_at::<Toy>(&path), toy);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_broken_or_missing_file_reads_as_nothing() {
        let dir = temp_dir("broken");
        let path = dir.join("agent-night.json");
        fs::write(&path, "не json").unwrap();
        assert_eq!(read_at::<Toy>(&path), Toy::default());
        assert_eq!(read_at::<Toy>(&dir.join("нет-такого.json")), Toy::default());
        let _ = fs::remove_dir_all(dir);
    }

    /// Оборванная запись не должна оставлять полуфайл: пишем в tmp и
    /// переименовываем, поэтому прежний журнал цел до последнего мига.
    #[test]
    fn the_write_leaves_no_temp_file_behind() {
        let dir = temp_dir("tmp");
        let path = dir.join("agent-visits.json");
        write_at(&path, &Toy { n: 1, text: "раз".into() }).unwrap();
        write_at(&path, &Toy { n: 2, text: "два".into() }).unwrap();
        let left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(left.is_empty(), "временный файл остался рядом: {left:?}");
        assert_eq!(read_at::<Toy>(&path).n, 2);
        let _ = fs::remove_dir_all(dir);
    }

    /// Журналы разные, и файлы у них разные: осушённая утром копилка не должна
    /// уносить с собой след заходов.
    #[test]
    fn the_two_memories_do_not_share_a_file() {
        assert_ne!(visits_file(), night_file());
        assert!(visits_file().starts_with(jarvis_dir()));
        assert!(night_file().starts_with(jarvis_dir()));
    }

    /// Файла нет — писать некуда, и это не ошибка: журнал в памяти (тесты,
    /// ранний старт) обязан работать молча.
    #[test]
    fn saving_without_a_file_is_silent() {
        save(None, "журнал заходов", &Toy::default());
    }
}
