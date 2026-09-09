//! Недописанные реплики чатов Джарвиса: `~/.jarvis/agent-drafts.json`.
//!
//! Почему свой файл, а не блок в settings.json. Настройки пишутся целиком:
//! каждая правка — это read-modify-write всего дерева с fsync (settings.rs,
//! `Store::update`). Черновик же ложится через полсекунды после последней
//! клавиши, то есть десятки раз за один разговор. Пустить этот поток через
//! общий файл значит поставить под тот же риск узлы, хоткеи и гранты — и терять
//! их разом с черновиками. Здесь терять нечего, кроме самого недописанного.
//!
//! Пишем атомарно (tmp + rename), как настройки: оборванная на середине запись
//! оставила бы файл, из которого не читается НИ ОДИН черновик, а не один битый.
//!
//! Ключ — id чата, а не сессии: черновик принадлежит месту, куда его напишут,
//! а у свежего чата нити ещё нет вовсе.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::util::{jarvis_dir, now_ms};

/// Потолок на один черновик. Вставленный лог на мегабайт — не реплика, а
/// случайность, и таскать её на каждой записи незачем.
const MAX_TEXT: usize = 64 * 1024;
/// Сколько чатов помним. Скрытие черновик НЕ трогает (иначе «скрыто» стало бы
/// потерей), поэтому ключи ушедших чатов копятся — режем самые давние.
const MAX_CHATS: usize = 200;

/// Недописанное одного чата.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Draft {
    pub text: String,
    /// Куда смотрел курсор. Без него человек возвращается в чат и дописывает в
    /// конец — не туда, куда смотрел, когда уходил.
    pub caret: usize,
    /// Когда трогали — по нему и режем лишнее.
    pub at: i64,
}

/// Все черновики: id чата → недописанное. BTreeMap, чтобы файл не перемешивался
/// от записи к записи (глазами его тоже читают).
pub type Book = BTreeMap<String, Draft>;

/// Записать черновик чата. Пустой (и пробельный) текст — не черновик, а стёртый
/// черновик: строка списка обязана перестать быть помеченной, а файл — не
/// хранить пустоту вечно.
pub fn put(book: &mut Book, chat_id: &str, text: &str, caret: usize, at: i64) {
    let id = chat_id.trim();
    if id.is_empty() {
        return;
    }
    if text.trim().is_empty() {
        book.remove(id);
        return;
    }
    let text: String = text.chars().take(MAX_TEXT).collect();
    let caret = caret.min(text.chars().count());
    book.insert(id.to_string(), Draft { text, caret, at });
    prune(book);
}

/// Убрать самые давние, когда ключей стало больше, чем мы обещали помнить.
fn prune(book: &mut Book) {
    let extra = book.len().saturating_sub(MAX_CHATS);
    if extra == 0 {
        return;
    }
    let mut by_age: Vec<(i64, String)> = book.iter().map(|(k, d)| (d.at, k.clone())).collect();
    by_age.sort_unstable();
    for (_, id) in by_age.into_iter().take(extra) {
        book.remove(&id);
    }
}

/// Ответ окну: черновики разом. Их немного и они маленькие — ходить за каждым
/// отдельно дороже самих данных.
pub fn to_json(book: &Book) -> Value {
    json!({ "ok": true, "drafts": book })
}

fn file() -> PathBuf {
    jarvis_dir().join("agent-drafts.json")
}

/// Прочитать книжку. Нет файла или он не разбирается — пусто: черновики не тот
/// повод, ради которого стоит отказывать окну в запуске.
pub fn read_at(path: &Path) -> Book {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Положить книжку на диск целиком и атомарно.
pub fn write_at(path: &Path, book: &Book) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("не смог создать {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".agent-drafts.tmp-{}", std::process::id()));
    let bytes = serde_json::to_string_pretty(book).map_err(|e| e.to_string())? + "\n";
    let out = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = out {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("не смог записать {}: {e}", path.display()));
    }
    Ok(())
}

/// Книжка в памяти: файл читаем один раз, дальше пишем поверх. Два окна ходят
/// сюда же — значит и видят одно и то же, а не два расходящихся снимка.
static CACHE: OnceLock<Mutex<Book>> = OnceLock::new();
fn cache() -> &'static Mutex<Book> {
    CACHE.get_or_init(|| Mutex::new(read_at(&file())))
}

pub fn all() -> Book {
    cache().lock().unwrap().clone()
}

/// Сохранить черновик — или стереть его пустым текстом. Одна команда на оба
/// действия: «стёр всё в поле» и «отправил» для файла одно и то же событие.
pub fn set(chat_id: &str, text: &str, caret: usize) -> Result<(), String> {
    let mut book = cache().lock().unwrap();
    put(&mut book, chat_id, text, caret, now_ms());
    write_at(&file(), &book)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("jarvis-drafts-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Главное: черновики двух чатов не сливаются в один.
    #[test]
    fn drafts_of_two_chats_do_not_mix() {
        let mut b = Book::new();
        put(&mut b, "c1", "письмо первому", 5, 100);
        put(&mut b, "c2", "письмо второму", 3, 200);
        assert_eq!(b["c1"].text, "письмо первому");
        assert_eq!(b["c2"].text, "письмо второму");
        assert_eq!(b["c1"].caret, 5);
        assert_eq!(b["c2"].caret, 3);
    }

    #[test]
    fn empty_text_erases_the_draft() {
        let mut b = Book::new();
        put(&mut b, "c1", "было", 2, 100);
        put(&mut b, "c1", "   \n ", 0, 200);
        assert!(!b.contains_key("c1"), "стёртое в поле осталось черновиком");
    }

    /// Курсор за пределами текста — не курсор: восстановить по нему нечего.
    #[test]
    fn caret_never_points_past_the_text() {
        let mut b = Book::new();
        put(&mut b, "c1", "три", 999, 100);
        assert_eq!(b["c1"].caret, 3);
    }

    #[test]
    fn oversized_text_is_cut_but_kept() {
        let mut b = Book::new();
        let huge = "я".repeat(MAX_TEXT + 500);
        put(&mut b, "c1", &huge, MAX_TEXT + 400, 100);
        assert_eq!(b["c1"].text.chars().count(), MAX_TEXT);
        assert_eq!(b["c1"].caret, MAX_TEXT);
    }

    #[test]
    fn the_oldest_drafts_go_first_when_the_book_overflows() {
        let mut b = Book::new();
        for i in 0..(MAX_CHATS + 5) {
            put(&mut b, &format!("c{i}"), "текст", 0, i as i64);
        }
        assert_eq!(b.len(), MAX_CHATS);
        assert!(!b.contains_key("c0"), "давний черновик пережил чистку");
        let last = format!("c{}", MAX_CHATS + 4);
        assert!(b.contains_key(&last), "свежий черновик вычистили");
    }

    /// Круг через диск: то же, что писали, тем же курсором. Это и есть
    /// «переживает закрытие приложения».
    #[test]
    fn a_draft_survives_the_round_trip_through_disk() {
        let dir = temp_dir("round");
        let path = dir.join("agent-drafts.json");
        let mut b = Book::new();
        put(&mut b, "c1", "не отправлено", 4, 777);
        write_at(&path, &b).unwrap();

        let back = read_at(&path);
        assert_eq!(back["c1"].text, "не отправлено");
        assert_eq!(back["c1"].caret, 4);
        assert_eq!(back.len(), 1);
    }

    /// Битый файл не должен мешать окну открыться: черновики — не настройки.
    #[test]
    fn a_broken_file_reads_as_no_drafts() {
        let dir = temp_dir("broken");
        let path = dir.join("agent-drafts.json");
        fs::write(&path, "не json").unwrap();
        assert!(read_at(&path).is_empty());
        assert!(read_at(&dir.join("нет-такого.json")).is_empty());
    }

    /// Оборванная запись не должна оставлять полуфайл: пишем в tmp и
    /// переименовываем, поэтому старая книжка цела до последнего мига.
    #[test]
    fn the_write_leaves_no_temp_file_behind() {
        let dir = temp_dir("tmp");
        let path = dir.join("agent-drafts.json");
        let mut b = Book::new();
        put(&mut b, "c1", "текст", 0, 1);
        write_at(&path, &b).unwrap();
        write_at(&path, &b).unwrap(); // второй заход поверх первого
        let left: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(left.is_empty(), "временный файл остался рядом: {left:?}");
    }

    #[test]
    fn json_for_the_window_keeps_text_and_caret() {
        let mut b = Book::new();
        put(&mut b, "c1", "хвост", 2, 5);
        let v = to_json(&b);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["drafts"]["c1"]["text"], json!("хвост"));
        assert_eq!(v["drafts"]["c1"]["caret"], json!(2));
    }
}
