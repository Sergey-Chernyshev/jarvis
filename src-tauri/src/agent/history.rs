//! История разговоров главного агента: что было — знает ДИСК, а не настройки.
//!
//! Настройки хранят имена и порядок, но не факт разговора. Пока список строился
//! только из них, потерянный указатель означал потерянный разговор: файл на 249
//! реплик всё это время лежал в `~/.claude/projects/…`, а дотянуться до него из
//! окна было нельзя. Теперь источник правды — транскрипты, настройки поверх них
//! лишь надстройка: имя, порядок, какой чат открыт.
//!
//! Каталог не зашит: хост работает из `std::env::temp_dir()` (см. `agent/mod.rs`),
//! и путь считается тем же механизмом, что у обычных сессий (`transcript_dir_for`).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use super::ChatBook;
use crate::util::{ellipsize, one_line};

/// Сколько головы читать ради первой реплики и сколько хвоста — ради времени.
const HEAD_BYTES: u64 = 64 * 1024;
const TAIL_BYTES: u64 = 8 * 1024;
/// Автозаголовок короче превью: в списке он стоит в строке с датой и счётчиком.
const NAME_CHARS: usize = 48;
const PREVIEW_CHARS: usize = 120;

/// Разговор, найденный на диске.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub session_id: String,
    /// Записей в транскрипте. Именно записей, а не разобранных сообщений: размер
    /// разговора нужен человеку как порядок величины, а счёт сообщений стоил бы
    /// разбора всего файла на каждое открытие окна.
    pub turns: usize,
    /// Мс эпохи, последняя запись. `None` — таймстампов не нашлось и mtime не дался.
    pub at: Option<i64>,
    /// Первая реплика человека — по ней разговор и узнаётся в списке.
    pub preview: String,
}

/// Кэш по (размер, mtime). Транскрипт бывает под мегабайт, а список дёргается на
/// каждом открытии окна — читать всё целиком каждый раз нельзя. Файл только
/// дописывается в хвост, поэтому при росте считаем ДЕЛЬТУ; первая реплика не
/// меняется никогда и переезжает в новую запись как есть.
#[derive(Clone)]
struct Cached {
    size: u64,
    mtime: Option<i64>,
    turns: usize,
    at: Option<i64>,
    preview: String,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// id разговора уходит в имя файла — пускаем только то, из чего пути не собрать.
pub fn is_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Транскрипт разговора в каталоге агента (существование не проверяем).
pub fn transcript_path(dir: &Path, sid: &str) -> Option<PathBuf> {
    is_session_id(sid).then(|| dir.join(format!("{sid}.jsonl")))
}

/// Все разговоры каталога, свежие сверху.
pub fn scan(dir: &Path) -> Vec<Thread> {
    let mut out: Vec<Thread> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| thread_of(&p))
        .collect();
    // Свежие сверху; без времени (нечитаемый файл) — в конец.
    out.sort_by_key(|t| std::cmp::Reverse(t.at));
    out
}

/// Разговор по одному файлу. Пустой/чужой файл — `None`, а не пустой разговор:
/// строка «0 реплик» в списке ничего не значит.
pub fn thread_of(file: &Path) -> Option<Thread> {
    let sid = file.file_stem()?.to_str()?.to_string();
    if !is_session_id(&sid) {
        return None;
    }
    let md = fs::metadata(file).ok()?;
    if !md.is_file() || md.len() == 0 {
        return None;
    }
    let size = md.len();
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64);

    let prev = cache().lock().ok().and_then(|c| c.get(file).cloned());
    if let Some(c) = prev.as_ref().filter(|c| c.size == size && c.mtime == mtime) {
        return Some(Thread {
            session_id: sid,
            turns: c.turns,
            at: c.at,
            preview: c.preview.clone(),
        });
    }

    // Дописали хвост — дочитываем только его. Файл укоротился (ротация, ручная
    // правка) — считаем заново: дельта в этом случае врёт.
    let turns = match prev.as_ref().filter(|c| c.size <= size) {
        Some(c) => c.turns + count_lines(file, c.size)?,
        None => count_lines(file, 0)?,
    };
    let preview = prev
        .as_ref()
        .map(|c| c.preview.clone())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| first_human_line(file));
    let at = last_ts(file).or(mtime);

    if let Ok(mut c) = cache().lock() {
        c.insert(
            file.to_path_buf(),
            Cached { size, mtime, turns, at, preview: preview.clone() },
        );
    }
    Some(Thread { session_id: sid, turns, at, preview })
}

/// Число записей = число переводов строки от смещения. Стримом, без аллокации
/// на весь файл: мегабайтный транскрипт незачем поднимать в память ради счётчика.
fn count_lines(file: &Path, from: u64) -> Option<usize> {
    let mut f = fs::File::open(file).ok()?;
    if from > 0 {
        f.seek(SeekFrom::Start(from)).ok()?;
    }
    let mut buf = vec![0u8; 64 * 1024];
    let mut n = 0usize;
    loop {
        match f.read(&mut buf).ok()? {
            0 => return Some(n),
            got => n += buf[..got].iter().filter(|b| **b == b'\n').count(),
        }
    }
}

/// Первая реплика человека — из ГОЛОВЫ файла: она там по построению, а хвост у
/// длинного разговора её уже не содержит.
fn first_human_line(file: &Path) -> String {
    let Some(text) = read_head(file, HEAD_BYTES) else { return String::new() };
    crate::transcript::entries_from_text(&text)
        .iter()
        .flat_map(crate::transcript::to_chat_items)
        .find(|i| i.role == "user" && i.kind == "text")
        .map(|i| cut(&i.text, PREVIEW_CHARS))
        .unwrap_or_default()
}

fn read_head(file: &Path, max: u64) -> Option<String> {
    let mut buf = Vec::with_capacity(max as usize);
    fs::File::open(file).ok()?.take(max).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Время последней записи из хвоста. Служебные записи (`mode`, `last-prompt`)
/// таймстампа не несут — идём с конца до первого, у кого он есть.
fn last_ts(file: &Path) -> Option<i64> {
    let text = crate::transcript::read_recent_text(file, TAIL_BYTES)?;
    crate::transcript::entries_from_text(&text)
        .iter()
        .rev()
        .find_map(|e| {
            e.get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::transcript::parse_ts)
        })
}

/// Обрезка с многоточием: `ellipsize` режет молча, а в списке нужен признак,
/// что фраза продолжается.
fn cut(s: &str, n: usize) -> String {
    let s = one_line(s);
    if s.chars().count() <= n {
        s
    } else {
        format!("{}…", ellipsize(&s, n).trim_end())
    }
}

/// Имя чата в списке: имя человека → автозаголовок из первой реплики → «Новый
/// чат». Имя человека первое и не теряется, когда автозаголовок появился:
/// «Чат 5» → «Изучите текущие сессии…» — это находка, а «Главный» → автозаголовок
/// было бы потерей.
pub fn display_name(human: Option<&str>, preview: &str) -> String {
    if let Some(n) = human.map(str::trim).filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    match cut(preview, NAME_CHARS) {
        s if !s.is_empty() => s,
        _ => "Новый чат".to_string(),
    }
}

/// Список для окна: сперва чаты из настроек в их порядке, затем разговоры,
/// найденные на диске и ни к кому не привязанные, — свежие сверху.
///
/// Второй кусок и есть вся суть: удалённый (или потерявший указатель) чат
/// остаётся видимым, потому что файл никуда не делся.
pub fn chats_json(book: &ChatBook, threads: &[Thread]) -> Value {
    let thread_of = |sid: Option<&str>| sid.and_then(|s| threads.iter().find(|t| t.session_id == s));
    let bound = |sid: &str| book.chats.iter().any(|c| c.session_id.as_deref() == Some(sid));

    let mut out: Vec<Value> = book
        .chats
        .iter()
        .enumerate()
        .map(|(i, c)| {
            entry(
                Some(&c.id),
                c.human_name(),
                c.session_id.as_deref(),
                i == book.current_index(),
                thread_of(c.session_id.as_deref()),
            )
        })
        .collect();
    out.extend(
        threads
            .iter()
            .filter(|t| !bound(&t.session_id))
            .map(|t| entry(None, None, Some(&t.session_id), false, Some(t))),
    );
    Value::Array(out)
}

fn entry(
    id: Option<&str>,
    human: Option<&str>,
    sid: Option<&str>,
    current: bool,
    t: Option<&Thread>,
) -> Value {
    let preview = t.map(|t| t.preview.as_str()).unwrap_or("");
    json!({
        "id": id,
        "name": display_name(human, preview),
        "named": human.is_some(),
        "sessionId": sid,
        "current": current,
        "turns": t.map(|t| t.turns).unwrap_or(0),
        "at": t.and_then(|t| t.at),
        "preview": preview,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::read_chats;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jarvis-hist-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Транскрипт в том же виде, в каком его пишет Claude: очередь, реплика
    /// человека, ответ ассистента.
    fn write_thread(dir: &Path, sid: &str, first: &str, ts: &str, extra: usize) -> PathBuf {
        let mut s = format!(
            "{{\"type\":\"queue-operation\",\"operation\":\"enqueue\",\"content\":\"{first}\"}}\n\
             {{\"type\":\"user\",\"uuid\":\"u1\",\"timestamp\":\"{ts}\",\"message\":{{\"role\":\"user\",\"content\":\"{first}\"}}}}\n"
        );
        for i in 0..extra {
            s.push_str(&format!(
                "{{\"type\":\"assistant\",\"uuid\":\"a{i}\",\"parentUuid\":\"u1\",\"timestamp\":\"{ts}\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"ответ {i}\"}}]}}}}\n"
            ));
        }
        let p = dir.join(format!("{sid}.jsonl"));
        fs::write(&p, s).unwrap();
        p
    }

    /// Главное: разговоры видны, даже если в настройках о них ни слова.
    #[test]
    fn scan_reads_threads_from_disk_newest_first() {
        let d = dir("scan");
        write_thread(&d, "s-old", "Сколько ты сейчас чатов видишь?", "2026-08-19T10:00:00Z", 3);
        write_thread(&d, "s-new", "Изучите текущие сессии", "2026-08-20T13:45:00Z", 1);
        // мусор в каталоге не должен попадать в список
        fs::write(d.join("notes.txt"), "не транскрипт").unwrap();
        fs::write(d.join("empty.jsonl"), "").unwrap();
        fs::create_dir_all(d.join("memory")).unwrap();

        let t = scan(&d);
        assert_eq!(t.len(), 2, "два разговора, мусор отброшен: {t:?}");
        assert_eq!(t[0].session_id, "s-new", "свежие сверху");
        assert_eq!(t[0].preview, "Изучите текущие сессии");
        assert_eq!(t[0].turns, 3, "считаем записи транскрипта");
        assert_eq!(t[1].turns, 5);
        assert_eq!(t[0].at, crate::transcript::parse_ts("2026-08-20T13:45:00Z"));
        assert!(t[1].at < t[0].at);
    }

    /// Список дёргается на каждом открытии окна: дописанный хвост дочитываем
    /// дельтой, первую реплику не перечитываем вовсе.
    #[test]
    fn growing_transcript_is_counted_by_delta() {
        let d = dir("delta");
        let p = write_thread(&d, "s-grow", "Первая реплика", "2026-08-20T10:00:00Z", 1);
        assert_eq!(thread_of(&p).unwrap().turns, 3);
        let mut text = fs::read_to_string(&p).unwrap();
        text.push_str("{\"type\":\"assistant\",\"uuid\":\"a9\",\"timestamp\":\"2026-08-20T11:00:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"ещё\"}]}}\n");
        fs::write(&p, &text).unwrap();

        let t = thread_of(&p).unwrap();
        assert_eq!(t.turns, 4, "дельта досчитана, а не потеряна");
        assert_eq!(t.preview, "Первая реплика", "заголовок пережил дописывание");
        assert_eq!(t.at, crate::transcript::parse_ts("2026-08-20T11:00:00Z"));

        // файл укоротили — считаем заново, а не «прошлое плюс дельта»
        fs::write(&p, "{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{\"content\":\"x\"}}\n").unwrap();
        assert_eq!(thread_of(&p).unwrap().turns, 1);
    }

    #[test]
    fn session_id_never_escapes_the_directory() {
        assert!(is_session_id("a25d01f8-c372-43fb-b84c-111ab14abf69"));
        assert!(!is_session_id("../../etc/passwd"));
        assert!(!is_session_id("a b"));
        assert!(!is_session_id(""));
        assert_eq!(transcript_path(Path::new("/tmp/x"), "../evil"), None);
    }

    // ── список: настройки поверх диска ────────────────────────────────────

    #[test]
    fn chat_without_a_thread_does_not_break_the_list() {
        // Свежий чат: нити ещё нет, а строка в списке быть обязана.
        let book = read_chats(&json!({}));
        let list = chats_json(&book, &[]);
        assert_eq!(list[0]["id"], json!("c1"));
        assert_eq!(list[0]["name"], json!("Новый чат"));
        assert_eq!(list[0]["named"], json!(false));
        assert_eq!(list[0]["sessionId"], Value::Null);
        assert_eq!(list[0]["turns"], json!(0));
        assert_eq!(list[0]["at"], Value::Null);
        assert_eq!(list[0]["preview"], json!(""));
        assert_eq!(list[0]["current"], json!(true));
        assert_eq!(list.as_array().unwrap().len(), 1);
    }

    /// Ровно та беда, с которой всё началось: чат удалили (или указатель ушёл из
    /// настроек), а разговор на диске жив — он обязан остаться достижимым.
    #[test]
    fn deleted_chat_keeps_its_conversation_in_the_history() {
        let d = dir("deleted");
        write_thread(&d, "s-249", "Изучите текущие сессии", "2026-08-20T13:45:00Z", 4);
        let threads = scan(&d);

        let mut book = read_chats(&json!({ "agentChat": {
            "chats": [{ "id": "c1", "name": "Главный", "sessionId": "s-249" },
                      { "id": "c2", "name": "" }],
            "current": "c2",
        }}));
        book.delete("c1").unwrap();

        let list = chats_json(&book, &threads);
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 2, "чат ушёл, разговор остался: {list}");
        assert_eq!(arr[1]["id"], Value::Null, "с диска — без чата");
        assert_eq!(arr[1]["sessionId"], json!("s-249"));
        assert_eq!(arr[1]["turns"], json!(6));
        assert_eq!(arr[1]["name"], json!("Изучите текущие сессии"), "заголовок из первой реплики");
        assert_eq!(arr[1]["named"], json!(false));
    }

    #[test]
    fn human_name_survives_the_auto_title() {
        let d = dir("named");
        write_thread(&d, "s-1", "Сколько ты сейчас чатов видишь?", "2026-08-20T13:45:00Z", 1);
        let threads = scan(&d);
        let book = read_chats(&json!({ "agentChat": {
            "chats": [{ "id": "c1", "name": "Главный", "sessionId": "s-1" }],
            "current": "c1",
        }}));
        let list = chats_json(&book, &threads);
        assert_eq!(list[0]["name"], json!("Главный"), "имя человека важнее автозаголовка");
        assert_eq!(list[0]["named"], json!(true));
        assert_eq!(list[0]["preview"], json!("Сколько ты сейчас чатов видишь?"));

        // а вот «Чат 5» — не имя, а прежняя заглушка: по такому списку не найти
        let book = read_chats(&json!({ "agentChat": {
            "chats": [{ "id": "c5", "name": "Чат 5", "sessionId": "s-1" }],
        }}));
        let list = chats_json(&book, &threads);
        assert_eq!(list[0]["name"], json!("Сколько ты сейчас чатов видишь?"));
        assert_eq!(list[0]["named"], json!(false));
    }

    #[test]
    fn adopted_thread_leaves_the_disk_only_tail() {
        let d = dir("adopt");
        write_thread(&d, "s-1", "Первый разговор", "2026-08-20T10:00:00Z", 1);
        write_thread(&d, "s-2", "Второй разговор", "2026-08-20T12:00:00Z", 1);
        let threads = scan(&d);

        let mut book = read_chats(&json!({}));
        assert_eq!(chats_json(&book, &threads).as_array().unwrap().len(), 3);

        book.adopt("s-2").unwrap();
        let list = chats_json(&book, &threads);
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 3, "разговор переехал в чат, а не задвоился: {list}");
        assert_eq!(arr[1]["id"], json!("c2"));
        assert_eq!(arr[1]["current"], json!(true), "привязанный разговор открывается");
        assert_eq!(arr[1]["name"], json!("Второй разговор"));
        assert_eq!(arr[2]["id"], Value::Null, "непривязанным остался один");
        assert_eq!(arr[2]["sessionId"], json!("s-1"));
    }

    #[test]
    fn long_first_line_is_cut_for_the_title() {
        let long = "Разбери мне пожалуйста всю историю чатов ".repeat(6);
        assert_eq!(display_name(None, "").as_str(), "Новый чат");
        assert_eq!(display_name(Some("  Грант  "), "первая реплика"), "Грант");
        let name = display_name(None, &long);
        assert!(name.ends_with('…') && name.chars().count() <= NAME_CHARS + 1, "{name}");
    }
}
