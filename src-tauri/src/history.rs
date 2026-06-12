//! История чатов по проектам: прошлые сессии из транскриптов
//! ~/.claude/projects/**∕*.jsonl с заголовком, временем, моделью.
//!
//! Полный парс тысяч файлов дорог, поэтому лёгкое чтение (голова+хвост 32КБ)
//! с кэшем по mtime: пересобирается только то, что изменилось на диске.
//! Служебные `-p` вызовы Jarvis идут с --no-session-persistence и файлов не
//! создают; старые — отсекаем по сигнатуре первого промпта.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::util::*;

/// Первый промпт начинается с этого → наш служебный вызов, в историю не берём.
const SERVICE_PREFIXES: [&str; 7] = [
    "Ответ агента:",
    "Хвост диалога",
    "Диалог рабочей сессии:",
    "Переведи строки",
    "Суммаризируй",
    "сожми этот ответ",
    "Задача: выдай",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Meta {
    mtime: i64,
    session_id: String,
    cwd: Option<String>,
    project: Option<String>,
    title: String,
    model: String,
    first_at: i64,
    last_at: i64,
    service: bool,
}

pub struct History {
    cache: Mutex<HashMap<String, Meta>>, // path → meta
    scanning: AtomicBool,
    persist_pending: AtomicBool,
}

fn cache_file() -> PathBuf {
    jarvis_dir().join("history.json")
}

fn projects_dir() -> PathBuf {
    claude_dir().join("projects")
}

fn first_user_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|b| b.get("text").and_then(Value::as_str))
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

fn parse_meta(file: &Path, mtime: i64) -> Option<Meta> {
    let size = fs::metadata(file).ok()?.len();
    let mut f = fs::File::open(file).ok()?;
    let read_chunk = |f: &mut fs::File, from: u64, len: u64| -> Option<String> {
        f.seek(SeekFrom::Start(from)).ok()?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    };
    let hl = size.min(32 * 1024);
    let head = read_chunk(&mut f, 0, hl)?;
    let tl = size.min(32 * 1024);
    let tail = read_chunk(&mut f, size - tl, tl)?;

    let mut meta = Meta {
        mtime,
        session_id: file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        last_at: mtime,
        ..Default::default()
    };

    let mut first_prompt = String::new();
    for line in head.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(d) = serde_json::from_str::<Value>(line) else { continue };
        if meta.cwd.is_none() {
            meta.cwd = d.get("cwd").and_then(Value::as_str).map(String::from);
        }
        if meta.first_at == 0 {
            meta.first_at = d
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::transcript::parse_ts)
                .unwrap_or(0);
        }
        if first_prompt.is_empty()
            && d.get("type").and_then(Value::as_str) == Some("user")
            && !d.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        {
            let t = one_line(&first_user_text(d.get("message").unwrap_or(&Value::Null)));
            if !t.is_empty() && !t.starts_with('<') {
                first_prompt = t;
            }
        }
        if meta.cwd.is_some() && !first_prompt.is_empty() {
            break;
        }
    }

    // хвост: ai-title (приоритетный заголовок), последняя модель, последнее время
    let mut ai_title = String::new();
    for line in tail.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(d) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(ts) = d.get("timestamp").and_then(Value::as_str).and_then(crate::transcript::parse_ts) {
            meta.last_at = meta.last_at.max(ts);
        }
        match d.get("type").and_then(Value::as_str) {
            Some("ai-title") => {
                if let Some(t) = d.get("aiTitle").and_then(Value::as_str) {
                    ai_title = one_line(t);
                }
            }
            Some("summary") => {
                if ai_title.is_empty() {
                    if let Some(t) = d.get("summary").and_then(Value::as_str) {
                        ai_title = one_line(t);
                    }
                }
            }
            Some("assistant") => {
                if let Some(m) = d.pointer("/message/model").and_then(Value::as_str) {
                    meta.model = friendly_model_or_empty(m);
                }
            }
            _ => {}
        }
    }

    // [0-9A-Za-z_], не \w: в Rust \w юникодный и скрывал бы кириллические команды
    let single_slash = regex::Regex::new(r"^/[0-9A-Za-z_]+$").unwrap();
    meta.service = SERVICE_PREFIXES.iter().any(|p| first_prompt.starts_with(p))
        || single_slash.is_match(&first_prompt); // одиночная слэш-команда
    meta.project = Some(meta.cwd.as_deref().map(basename).unwrap_or_else(|| "другое".into()));
    let title_src = if ai_title.is_empty() { &first_prompt } else { &ai_title };
    meta.title = ellipsize(title_src, 100);
    if meta.first_at == 0 {
        meta.first_at = mtime;
    }
    Some(meta)
}

fn friendly_model_or_empty(id: &str) -> String {
    let m = friendly_model(id);
    let known = ["Opus", "Sonnet", "Haiku", "Fable", "Mythos"];
    if known.contains(&m.as_str()) {
        m
    } else {
        String::new()
    }
}

impl History {
    pub fn load() -> Self {
        let cache = fs::read_to_string(cache_file())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            cache: Mutex::new(cache),
            scanning: AtomicBool::new(false),
            persist_pending: AtomicBool::new(false),
        }
    }

    fn persist(self: &Arc<Self>) {
        if self.persist_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let h = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            h.persist_pending.store(false, Ordering::SeqCst);
            if let Ok(json) = serde_json::to_string(&*h.cache.lock().unwrap()) {
                let _ = fs::create_dir_all(jarvis_dir());
                let _ = fs::write(cache_file(), json);
            }
        });
    }

    fn list_files() -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(dirs) = fs::read_dir(projects_dir()) else { return out };
        for d in dirs.filter_map(|e| e.ok()) {
            if !d.path().is_dir() {
                continue;
            }
            let Ok(files) = fs::read_dir(d.path()) else { continue };
            for f in files.filter_map(|e| e.ok()) {
                let p = f.path();
                if p.extension().is_some_and(|x| x == "jsonl") {
                    out.push(p);
                }
            }
        }
        out
    }

    pub fn scan(self: &Arc<Self>) {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for file in Self::list_files() {
            let key = file.to_string_lossy().into_owned();
            seen.insert(key.clone());
            let Ok(st) = fs::metadata(&file) else { continue };
            if st.len() < 200 {
                continue; // пустые/обрывки
            }
            let mtime = st
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let fresh = self
                .cache
                .lock()
                .unwrap()
                .get(&key)
                .is_some_and(|hit| hit.mtime == mtime);
            if fresh {
                continue; // не менялся
            }
            if let Some(meta) = parse_meta(&file, mtime) {
                self.cache.lock().unwrap().insert(key, meta);
            }
        }
        self.cache.lock().unwrap().retain(|k, _| seen.contains(k)); // удалённые
        self.persist();
        self.scanning.store(false, Ordering::SeqCst);
    }

    /// [{project, cwd, count, lastAt, sessions:[{id,title,model,tokens,cost,billing,lastAt}]}]
    pub fn projects(&self, usage: &crate::usage::Usage) -> Value {
        struct Group {
            project: String,
            cwd: Option<String>,
            last_at: i64,
            sessions: Vec<Value>,
        }
        let mut by_project: HashMap<String, Group> = HashMap::new();
        for meta in self.cache.lock().unwrap().values() {
            if meta.service || meta.title.is_empty() {
                continue;
            }
            let project = meta.project.clone().unwrap_or_else(|| "другое".into());
            let key = meta.cwd.clone().unwrap_or_else(|| project.clone());
            let g = by_project.entry(key).or_insert_with(|| Group {
                project: project.clone(),
                cwd: meta.cwd.clone(),
                last_at: 0,
                sessions: Vec::new(),
            });
            let u = usage.for_session(&meta.session_id).unwrap_or(Value::Null);
            let model = if meta.model.is_empty() {
                u.get("model").and_then(Value::as_str).unwrap_or("").to_string()
            } else {
                meta.model.clone()
            };
            g.sessions.push(serde_json::json!({
                "id": meta.session_id,
                "title": meta.title,
                "model": model,
                "tokens": u.get("tok").and_then(Value::as_f64).unwrap_or(0.0),
                "cost": u.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
                "billing": u.get("billing").and_then(Value::as_str).unwrap_or("plan"),
                "lastAt": meta.last_at,
            }));
            g.last_at = g.last_at.max(meta.last_at);
        }
        let mut out: Vec<Value> = by_project
            .into_values()
            .map(|mut g| {
                g.sessions.sort_by_key(|s| -s.get("lastAt").and_then(Value::as_i64).unwrap_or(0));
                let count = g.sessions.len();
                g.sessions.truncate(40); // на проект — последние 40
                serde_json::json!({
                    "project": g.project,
                    "cwd": g.cwd,
                    "count": count,
                    "lastAt": g.last_at,
                    "sessions": g.sessions,
                })
            })
            .collect();
        out.sort_by_key(|g| -g.get("lastAt").and_then(Value::as_i64).unwrap_or(0));
        Value::Array(out)
    }
}
