//! Оглавление проектов машины: где тут вообще работали.
//!
//! Узел читает ТОЛЬКО оглавление — имена файлов дают идентификаторы сессий,
//! mtime даёт время, а рабочий каталог берётся из первых килобайт свежайшего
//! транскрипта. Это не отменяет границу «интерпретация на ноуте»: статусы,
//! ходы и сводки по-прежнему считает он. Найти файлы на чужой машине кроме
//! узла некому — в этом и разница.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Сколько байт первого транскрипта прочесть ради `cwd`. Хватает с запасом:
/// Claude кладёт его в первую же запись.
const CWD_PROBE: usize = 8 * 1024;

/// Сколько сессий отдавать на проект. Список нужен, чтобы выбрать чат, а не
/// чтобы пролистать всё за год — древние всё равно не открывают.
const MAX_SESSIONS: usize = 50;

fn mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Рабочий каталог проекта из первой записи транскрипта.
///
/// Имя каталога Claude кодирует, заменяя `/` и `.` на `-`, — обратно это
/// однозначно не разворачивается («-» бывает и в самом имени). Поэтому
/// спрашиваем сам файл, а закодированное имя оставляем запасным вариантом.
fn cwd_from(file: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0u8; CWD_PROBE];
    let mut f = std::fs::File::open(file).ok()?;
    let n = f.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
            if cwd.starts_with('/') {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

/// Один проект: каталог, сессии (свежие сверху) и время последней.
fn scan_project(dir: &Path) -> Option<Value> {
    let mut files: Vec<(PathBuf, i64, u64)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .map(|p| {
            let at = mtime_ms(&p);
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            (p, at, size)
        })
        .collect();
    if files.is_empty() {
        return None; // пустой каталог проекта — не проект
    }
    files.sort_by_key(|(_, at, _)| -at); // свежие сверху

    let cwd = files.iter().find_map(|(p, _, _)| cwd_from(p));
    let sessions: Vec<Value> = files
        .iter()
        .take(MAX_SESSIONS)
        .map(|(p, at, size)| {
            json!({
                "id": p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                "at": at,
                "size": size,
                "path": p.to_string_lossy(),
            })
        })
        .collect();
    Some(json!({
        "dir": dir.to_string_lossy(),
        "cwd": cwd,
        "agent": "claude",
        "lastAt": files[0].1,
        "count": files.len(),
        "sessions": sessions,
    }))
}

/// Все проекты Claude Code этой машины.
pub fn list(home: &Path) -> Vec<Value> {
    let root = home.join(".claude").join("projects");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new(); // агент тут ещё не работал — это не ошибка
    };
    let mut out: Vec<Value> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| scan_project(&p))
        .collect();
    out.sort_by_key(|p| -p.get("lastAt").and_then(Value::as_i64).unwrap_or(0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jarvis-projects-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn cwd_comes_from_the_file_not_from_the_folder_name() {
        // имя каталога кодирует cwd необратимо («-» бывает и в самом имени),
        // поэтому единственный честный источник — сама запись
        let d = sandbox("cwd");
        let f = d.join("s1.jsonl");
        std::fs::write(&f, "{\"type\":\"user\",\"cwd\":\"/home/bob/my-proj\"}\n").unwrap();
        assert_eq!(cwd_from(&f).as_deref(), Some("/home/bob/my-proj"));
    }

    #[test]
    fn broken_first_lines_do_not_hide_the_cwd() {
        let d = sandbox("broken");
        let f = d.join("s.jsonl");
        std::fs::write(&f, "не json\n{\"cwd\":\"relative\"}\n{\"cwd\":\"/srv/x\"}\n").unwrap();
        assert_eq!(cwd_from(&f).as_deref(), Some("/srv/x"), "относительный путь — не cwd");
    }

    #[test]
    fn projects_are_listed_freshest_first_and_empty_dirs_skipped() {
        let home = sandbox("list");
        let projects = home.join(".claude/projects");
        std::fs::create_dir_all(projects.join("-a")).unwrap();
        std::fs::create_dir_all(projects.join("-b")).unwrap();
        std::fs::create_dir_all(projects.join("-empty")).unwrap();
        std::fs::write(projects.join("-a/one.jsonl"), "{\"cwd\":\"/a\"}\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(projects.join("-b/two.jsonl"), "{\"cwd\":\"/b\"}\n").unwrap();

        let got = list(&home);
        assert_eq!(got.len(), 2, "пустой каталог проектом не считается");
        assert_eq!(got[0]["cwd"], "/b", "свежий проект первым");
        assert_eq!(got[0]["sessions"][0]["id"], "two");
    }
}
