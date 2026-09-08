//! Оглавление проектов машины: где тут вообще работали.
//!
//! Узел читает ТОЛЬКО оглавление — имена файлов дают идентификаторы сессий,
//! mtime даёт время, а рабочий каталог берётся из первых килобайт свежайшего
//! транскрипта. Это не отменяет границу «интерпретация на ноуте»: статусы,
//! ходы и сводки по-прежнему считает он. Найти файлы на чужой машине кроме
//! узла некому — в этом и разница.

use serde_json::{json, Value};
use std::collections::HashMap;
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

/// Projects from both supported providers, without reading full transcripts.
pub fn list(home: &Path) -> Vec<Value> {
    let codex = std::env::var_os("CODEX_HOME").filter(|path| !path.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    list_roots(home, &codex)
}

fn list_roots(home: &Path, codex: &Path) -> Vec<Value> {
    let mut out = list_claude(home);
    out.extend(list_codex(codex));
    out.sort_by_key(|project| std::cmp::Reverse(project.get("lastAt").and_then(Value::as_i64).unwrap_or(0)));
    out
}

fn list_claude(home: &Path) -> Vec<Value> {
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

/// Codex stores rollouts by date rather than by project. The session_meta
/// header is the authority for identity and cwd; filename spelling is not.
fn codex_header(file: &Path) -> Option<(String, String)> {
    use std::io::{BufRead, BufReader, Read};
    let mut reader = BufReader::new(std::fs::File::open(file).ok()?.take(256 * 1024));
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 { break; }
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") { continue; }
        let payload = value.get("payload")?;
        let id = payload.get("id")?.as_str()?.trim();
        let cwd = payload.get("cwd")?.as_str()?;
        if !id.is_empty() && cwd.starts_with('/') { return Some((id.to_string(), cwd.to_string())); }
    }
    None
}

fn codex_files(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 { return; }
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() { codex_files(&entry.path(), depth + 1, out); }
        else if kind.is_file() && entry.path().extension().is_some_and(|extension| extension == "jsonl") { out.push(entry.path()); }
    }
}

fn list_codex(codex: &Path) -> Vec<Value> {
    let mut files = Vec::new();
    codex_files(&codex.join("sessions"), 0, &mut files);
    let mut groups: HashMap<String, HashMap<String, Value>> = HashMap::new();
    for file in files {
        let Some((id, cwd)) = codex_header(&file) else { continue };
        let at = mtime_ms(&file);
        let sessions = groups.entry(cwd).or_default();
        if sessions.get(&id).is_some_and(|existing| existing["at"].as_i64().unwrap_or(0) >= at) { continue; }
        sessions.insert(id.clone(), json!({"id":id,"agent":"codex","at":at,
            "size":std::fs::metadata(&file).map(|meta| meta.len()).unwrap_or(0),"path":file.to_string_lossy()}));
    }
    groups.into_iter().map(|(cwd, sessions)| {
        let mut sessions: Vec<_> = sessions.into_values().collect();
        sessions.sort_by_key(|session| std::cmp::Reverse(session["at"].as_i64().unwrap_or(0)));
        let count = sessions.len();
        let last_at = sessions.first().and_then(|session| session["at"].as_i64()).unwrap_or(0);
        sessions.truncate(MAX_SESSIONS);
        json!({"cwd":cwd,"agent":"codex","count":count,"lastAt":last_at,"sessions":sessions})
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_projects_use_rollout_metadata_and_coexist_with_claude() {
        let home = sandbox("both-providers");
        let claude = home.join(".claude/projects/-repo");
        let codex = home.join("custom-codex");
        let day = codex.join("sessions/2026/09/05");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(claude.join("claude-id.jsonl"), "{\"cwd\":\"/repo\"}\n").unwrap();
        std::fs::write(day.join("filename-is-not-session-id.jsonl"), "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-id\",\"cwd\":\"/repo\"}}\n").unwrap();
        let projects = list_roots(&home, &codex);
        assert_eq!(projects.len(), 2);
        let project = projects.iter().find(|project| project["agent"] == "codex").unwrap();
        assert_eq!(project["cwd"], "/repo");
        assert_eq!(project["sessions"][0]["id"], "codex-id");
        assert_eq!(project["sessions"][0]["agent"], "codex");
        assert!(projects.iter().any(|project| project["agent"] == "claude"));
    }

    #[test]
    fn codex_index_ignores_non_metadata_paths_and_does_not_follow_directory_symlinks() {
        let home = sandbox("codex-safe-scan");
        let sessions = home.join("sessions");
        let outside = home.join("outside");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(sessions.join("message.jsonl"), "{\"type\":\"response_item\",\"cwd\":\"/fake\"}\n").unwrap();
        std::fs::write(outside.join("hidden.jsonl"), "{\"type\":\"session_meta\",\"payload\":{\"id\":\"hidden\",\"cwd\":\"/outside\"}}\n").unwrap();
        std::os::unix::fs::symlink(&outside, sessions.join("link")).unwrap();
        assert!(list_codex(&home).is_empty());
    }

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

        let got = list_roots(&home, &home.join(".codex"));
        assert_eq!(got.len(), 2, "пустой каталог проектом не считается");
        assert_eq!(got[0]["cwd"], "/b", "свежий проект первым");
        assert_eq!(got[0]["sessions"][0]["id"], "two");
    }
}
