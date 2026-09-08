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
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
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
    parser_revision: u8,
    session_id: String,
    cwd: Option<String>,
    project: Option<String>,
    title: String,
    model: String,
    first_at: i64,
    last_at: i64,
    service: bool,
    /// Какой агент стоял за сессией: "claude" | "codex". Пусто (старый кэш) → claude.
    /// Нужно фронту, чтобы скопировать ВЕРНУЮ команду resume (codex ≠ claude).
    agent: String,
    instance_id: Option<String>,
    instance_label: Option<String>,
    provider_home: Option<String>,
    provider_session_id: Option<String>,
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

fn codex_sessions_dir() -> PathBuf {
    crate::util::codex_dir().join("sessions")
}

/// Rollout Codex → Meta (для истории). session_meta даёт id/cwd, turn_context —
/// модель, первая user-реплика — заголовок. service=false (codex-сессии не наши;
/// служебные codex exec идут с --ephemeral и rollout не пишут).
const CODEX_META_REVISION: u8 = 2;
const CLAUDE_META_REVISION: u8 = 1;
const CODEX_PREFIX_BUDGET: usize = 8 * 1024 * 1024;
const CODEX_CONTENT_BUDGET: usize = 1024 * 1024;
const CODEX_META_LINE_LIMIT: usize = 128 * 1024;

/// Consume one complete record with bounded memory. A large compacted record
/// is skipped in 64 KiB chunks without moving the fork ordinal boundary.
fn bounded_codex_line(reader: &mut impl BufRead, budget: &mut usize) -> Option<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        if *budget == 0 {
            return None;
        }
        let available = reader.fill_buf().ok()?;
        if available.is_empty() {
            return None;
        }
        let available = &available[..available.len().min(*budget)];
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|i| i + 1);
        let count = end.unwrap_or(available.len());
        if !oversized {
            if line.len() + count > CODEX_META_LINE_LIMIT {
                oversized = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..count]);
            }
        }
        reader.consume(count);
        *budget -= count;
        if end.is_some() {
            return Some((!oversized).then_some(line));
        }
    }
}

fn apply_codex_meta(meta: &mut Meta, value: &Value) {
    if crate::backend::codex_transcript::is_technical_session_entry(value) {
        meta.service = true;
    }
    let at = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(crate::transcript::parse_ts)
        .unwrap_or(0);
    meta.last_at = meta.last_at.max(at);
    match value.get("type").and_then(Value::as_str) {
        Some("session_meta") if meta.session_id.is_empty() => {
            let payload = &value["payload"];
            meta.session_id = payload["id"].as_str().unwrap_or("").into();
            meta.cwd = payload["cwd"].as_str().map(String::from);
            meta.first_at = payload["timestamp"]
                .as_str()
                .and_then(crate::transcript::parse_ts)
                .unwrap_or(at);
        }
        Some("turn_context") => {
            if let Some(model) = value["payload"]["model"].as_str() {
                meta.model = model.into();
            }
        }
        Some("event_msg")
            if meta.title.is_empty() && value["payload"]["type"] == "user_message" =>
        {
            if let Some(text) = value["payload"]["message"]
                .as_str()
                .filter(|text| !text.trim().is_empty())
            {
                let text = crate::backend::codex_transcript::normalize_user_text(text);
                if !text.is_empty() {
                    meta.title = ellipsize(&one_line(&text), 80);
                }
            }
        }
        Some("response_item") if meta.title.is_empty() => {
            for item in crate::backend::codex_transcript::to_chat_items(value) {
                if item.role == "user" && item.kind == "text" {
                    meta.title = ellipsize(&one_line(&item.text), 80);
                    break;
                }
            }
        }
        _ => {}
    }
}

fn parse_codex_meta(file: &Path, mtime: i64) -> Option<Meta> {
    let file = fs::File::open(file).ok()?;
    let info = file.metadata().ok()?;
    if !info.is_file() {
        return None;
    }
    let size = info.len();
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut scope = crate::rollout_scope::RolloutScope::default();
    let mut meta = Meta {
        mtime,
        parser_revision: CODEX_META_REVISION,
        agent: "codex".into(),
        ..Default::default()
    };
    let mut prefix_budget = CODEX_PREFIX_BUDGET;
    let mut owned_start = None;
    // Establish the first owner and pass the inherited prefix before reading
    // title/model. Copied records have spawn timestamps, so time cannot scope them.
    while let Some(line) = bounded_codex_line(&mut reader, &mut prefix_budget) {
        match line.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()) {
            Some(value) => {
                if scope.accept(&value) {
                    apply_codex_meta(&mut meta, &value);
                }
            }
            None => scope.skip_record(),
        }
        if scope.owner_id.is_some()
            && scope
                .inherited_before
                .map_or(true, |start| scope.next_ordinal >= start)
        {
            owned_start = Some(reader.stream_position().ok()?);
            break;
        }
    }
    if meta.session_id.is_empty() {
        return None;
    }
    if let Some(owned_start) = owned_start {
        let mut budget = CODEX_CONTENT_BUDGET;
        while let Some(line) = bounded_codex_line(&mut reader, &mut budget) {
            match line.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()) {
                Some(value) => {
                    if scope.accept(&value) {
                        apply_codex_meta(&mut meta, &value);
                    }
                }
                None => scope.skip_record(),
            }
        }
        let head_end = reader.stream_position().ok()?;
        if head_end < size {
            // The tail is never allowed to enter inherited context, even if a
            // fork contains only a header and a huge unfinished parent prefix.
            let start = size
                .saturating_sub(CODEX_CONTENT_BUDGET as u64)
                .max(owned_start);
            let mut budget = (size - start) as usize;
            let mut preceding = [b'\n'];
            if start > 0 {
                reader.seek(SeekFrom::Start(start - 1)).ok()?;
                reader.read_exact(&mut preceding).ok()?;
            } else {
                reader.seek(SeekFrom::Start(start)).ok()?;
            }
            if preceding[0] != b'\n' {
                let _ = bounded_codex_line(&mut reader, &mut budget);
            }
            while let Some(line) = bounded_codex_line(&mut reader, &mut budget) {
                match line.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()) {
                    Some(value) => {
                        if scope.accept(&value) {
                            apply_codex_meta(&mut meta, &value);
                        }
                    }
                    None => scope.skip_record(),
                }
            }
        }
    }
    meta.project = meta.cwd.as_deref().map(basename);
    if meta.title.is_empty() {
        meta.title = "Codex-сессия".into();
    }
    meta.model = crate::backend::backend(crate::backend::Agent::Codex).friendly_model(&meta.model);
    Some(meta)
}

fn codex_meta_fresh(meta: &Meta, mtime: i64) -> bool {
    meta.mtime == mtime && meta.instance_id.is_some() && meta.parser_revision == CODEX_META_REVISION
}

fn first_user_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .map(crate::backend::codex_transcript::normalize_user_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
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
        parser_revision: CLAUDE_META_REVISION,
        session_id: file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        last_at: mtime,
        agent: crate::backend::Agent::Claude.label().to_string(),
        ..Default::default()
    };

    let mut first_prompt = String::new();
    for line in head.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(d) = serde_json::from_str::<Value>(line) else {
            continue;
        };
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
            let t = one_line(&crate::backend::codex_transcript::normalize_user_text(
                &first_user_text(d.get("message").unwrap_or(&Value::Null)),
            ));
            if !t.is_empty() {
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
        let Ok(d) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ts) = d
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(crate::transcript::parse_ts)
        {
            meta.last_at = meta.last_at.max(ts);
        }
        match d.get("type").and_then(Value::as_str) {
            Some("ai-title") => {
                if let Some(t) = d.get("aiTitle").and_then(Value::as_str) {
                    let cleaned = crate::backend::codex_transcript::normalize_user_text(t);
                    if !crate::backend::codex_transcript::needs_title_repair(&cleaned) {
                        ai_title = one_line(&cleaned);
                    }
                }
            }
            Some("summary") => {
                if ai_title.is_empty() {
                    if let Some(t) = d.get("summary").and_then(Value::as_str) {
                        let cleaned = crate::backend::codex_transcript::normalize_user_text(t);
                        if !crate::backend::codex_transcript::needs_title_repair(&cleaned) {
                            ai_title = one_line(&cleaned);
                        }
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
    meta.project = Some(
        meta.cwd
            .as_deref()
            .map(basename)
            .unwrap_or_else(|| "другое".into()),
    );
    let title_src = if ai_title.is_empty() {
        &first_prompt
    } else {
        &ai_title
    };
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
        let Ok(dirs) = fs::read_dir(projects_dir()) else {
            return out;
        };
        for d in dirs.filter_map(|e| e.ok()) {
            if !d.path().is_dir() {
                continue;
            }
            let Ok(files) = fs::read_dir(d.path()) else {
                continue;
            };
            for f in files.filter_map(|e| e.ok()) {
                let p = f.path();
                if p.extension().is_some_and(|x| x == "jsonl") {
                    out.push(p);
                }
            }
        }
        out
    }

    fn list_codex_files() -> Vec<PathBuf> {
        fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
            if depth > 4 || out.len() >= 30_000 {
                return;
            }
            let Ok(rd) = fs::read_dir(dir) else { return };
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                let Ok(kind) = e.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    walk(&p, depth + 1, out);
                } else if kind.is_file() && p.extension().is_some_and(|x| x == "jsonl") {
                    out.push(p);
                }
            }
        }
        let mut out = Vec::new();
        if let Ok(registry) = crate::session_identity::registry() {
            for root in registry.roots(true) {
                walk(&root.path, 0, &mut out);
            }
        }
        out.sort();
        out.dedup();
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
            let Ok(st) = fs::metadata(&file) else {
                continue;
            };
            if st.len() < 200 {
                continue; // пустые/обрывки
            }
            let mtime = st
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            // `!agent.is_empty()` — миграция: записи старого кэша без метки агента
            // пере-парсим, иначе codex-сессии остались бы без agent (issue #10).
            let fresh = self.cache.lock().unwrap().get(&key).is_some_and(|hit| {
                hit.mtime == mtime
                    && !hit.agent.is_empty()
                    && hit.parser_revision == CLAUDE_META_REVISION
            });
            if fresh {
                continue; // не менялся
            }
            if let Some(meta) = parse_meta(&file, mtime) {
                self.cache.lock().unwrap().insert(key, meta);
            }
        }
        // Codex rollouts (~/.codex/sessions/**/*.jsonl)
        let registry = crate::session_identity::registry().ok();
        for file in Self::list_codex_files() {
            let key = file.to_string_lossy().into_owned();
            seen.insert(key.clone());
            let Ok(st) = fs::metadata(&file) else {
                continue;
            };
            if st.len() < 200 {
                continue;
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
                .is_some_and(|hit| codex_meta_fresh(hit, mtime));
            if fresh {
                continue;
            }
            if let Some(mut meta) = parse_codex_meta(&file, mtime) {
                if let Some(instance) = registry
                    .as_ref()
                    .and_then(|r| r.instance_for_transcript(&file))
                {
                    meta.provider_session_id = Some(meta.session_id.clone());
                    let home = instance.canonical_home.to_string_lossy().into_owned();
                    meta.session_id =
                        crate::session_identity::key(&instance.id, &home, &meta.session_id, None);
                    meta.instance_id = Some(instance.id.clone());
                    meta.instance_label = Some(instance.label.clone());
                    meta.provider_home = Some(home);
                }
                self.cache.lock().unwrap().insert(key, meta);
            }
        }
        self.cache.lock().unwrap().retain(|k, _| seen.contains(k)); // удалённые
        self.persist();
        self.scanning.store(false, Ordering::SeqCst);
    }

    /// [{project, cwd, count, lastAt, sessions:[{id,title,model,tokens,cost,billing,lastAt}]}]
    pub fn projects(&self, usage: &crate::usage::Usage) -> Value {
        let _t = crate::log::Step::new("history.projects");
        struct Group {
            project: String,
            cwd: Option<String>,
            last_at: i64,
            sessions: Vec<Value>,
        }
        let mut by_project: HashMap<String, Group> = HashMap::new();
        // Снимок под замком, сборка — без него. Раньше замок кэша держался всю
        // дорогу, а внутри цикла бралcя ещё и замок расхода: на большой истории
        // это ставило в очередь и сканер, и запись кэша. Копия дороже на одну
        // аллокацию и дешевле на всё остальное.
        let snapshot: Vec<Meta> = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for meta in &snapshot {
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
                u.get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            } else {
                meta.model.clone()
            };
            g.sessions.push(serde_json::json!({
                "id": meta.session_id,
                "title": meta.title,
                "agent": if meta.agent.is_empty() { "claude" } else { &meta.agent },
                "instanceId": meta.instance_id, "instanceLabel": meta.instance_label,
                "providerHome": meta.provider_home, "providerSessionId": meta.provider_session_id,
                "agentId": meta.provider_session_id.as_deref().unwrap_or(&meta.session_id),
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
                g.sessions
                    .sort_by_key(|s| -s.get("lastAt").and_then(Value::as_i64).unwrap_or(0));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_meta_uses_real_prompt_and_marks_only_explicit_guardian_sessions_service() {
        let mut meta = Meta::default();
        apply_codex_meta(
            &mut meta,
            &serde_json::json!({"type":"session_meta","payload":{"id":"normal","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}}}),
        );
        apply_codex_meta(
            &mut meta,
            &serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"<recommended_plugins>context</recommended_plugins><environment_context>cwd</environment_context>"}}),
        );
        assert!(meta.title.is_empty());
        assert!(!meta.service);
        apply_codex_meta(
            &mut meta,
            &serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"<environment_context>cwd</environment_context>\nFix the search"}}),
        );
        assert_eq!(meta.title, "Fix the search");
        let mut technical = Meta::default();
        apply_codex_meta(
            &mut technical,
            &serde_json::json!({"type":"session_meta","payload":{"id":"review","source":{"subagent":{"other":"guardian"}},"thread_source":"guardian_review"}}),
        );
        assert!(technical.service);
        let mut model_only = Meta::default();
        apply_codex_meta(
            &mut model_only,
            &serde_json::json!({"type":"turn_context","payload":{"model":"codex-auto-review"}}),
        );
        assert!(model_only.service);
    }

    #[test]
    fn claude_title_can_use_real_text_after_a_context_block() {
        let message = serde_json::json!({"content":[{"type":"text","text":"<system-reminder>context</system-reminder>"},{"type":"text","text":"<div>Real XML request</div>"}]});
        assert_eq!(first_user_text(&message), "<div>Real XML request</div>");
    }

    fn fork_history_file(compacted_bytes: usize) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "jarvis-fork-history-{}-{}.jsonl",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let row = |ordinal, kind, payload| serde_json::json!({"ordinal":ordinal,"type":kind,"timestamp":"2026-09-05T01:20:40.906Z","payload":payload});
        let mut rows = vec![
            row(
                0,
                "session_meta",
                serde_json::json!({"id":"child","forked_from_id":"parent","subagent_history_start_ordinal":17,"cwd":"/child-worktree","timestamp":"2026-09-05T01:20:40.602Z"}),
            ),
            row(
                1,
                "session_meta",
                serde_json::json!({"id":"parent","cwd":"/parent","timestamp":"2026-09-04T21:37:26.679Z"}),
            ),
            row(
                2,
                "compacted",
                serde_json::json!({"message":"x".repeat(compacted_bytes)}),
            ),
        ];
        // Match the actual 17-record inherited prefix, including retimestamped
        // parent model/title records after the >1.6 MiB compacted line.
        for ordinal in 3..17 {
            rows.push(if matches!(ordinal,4|12) { row(ordinal,"turn_context",serde_json::json!({"model":"parent-model","cwd":"/parent"})) }
            else { row(ordinal,"response_item",serde_json::json!({"type":"message","role":"user","content":[{"type":"input_text","text":"Parent title must never win"}]})) });
        }
        rows.push(row(
            17,
            "event_msg",
            serde_json::json!({"type":"thread_settings_applied"}),
        ));
        rows.push(row(
            18,
            "event_msg",
            serde_json::json!({"type":"task_started","turn_id":"child-turn"}),
        ));
        rows.push(row(19,"response_item",serde_json::json!({"type":"message","role":"user","content":[{"type":"input_text","text":"Own child task"}]})));
        rows.push(row(
            20,
            "turn_context",
            serde_json::json!({"model":"gpt-5.5"}),
        ));
        fs::write(
            &path,
            rows.into_iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
        path
    }

    #[test]
    fn codex_fork_history_skips_large_parent_prefix_and_keeps_own_metadata() {
        let path = fork_history_file(1_627_964);
        let meta = parse_codex_meta(&path, 42).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(meta.session_id, "child");
        assert_eq!(meta.cwd.as_deref(), Some("/child-worktree"));
        assert_eq!(meta.title, "Own child task");
        assert!(!meta.model.contains("parent"));
        assert!(!meta.model.is_empty());
        assert_eq!(
            meta.first_at,
            crate::transcript::parse_ts("2026-09-05T01:20:40.602Z").unwrap()
        );
        assert_eq!(meta.parser_revision, CODEX_META_REVISION);
    }

    #[test]
    fn unreached_fork_boundary_returns_owner_with_generic_title_not_inherited_tail() {
        let path = fork_history_file(CODEX_PREFIX_BUDGET + 1024);
        let meta = parse_codex_meta(&path, 42).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(meta.session_id, "child");
        assert_eq!(meta.cwd.as_deref(), Some("/child-worktree"));
        assert_eq!(meta.title, "Codex-сессия");
        assert!(!meta.model.contains("parent"));
    }

    #[test]
    fn unchanged_mtime_cannot_reuse_a_history_cache_from_before_fork_scoping() {
        let mut old = Meta {
            mtime: 42,
            instance_id: Some("profile".into()),
            parser_revision: 0,
            ..Default::default()
        };
        assert!(!codex_meta_fresh(&old, 42));
        old.parser_revision = CODEX_META_REVISION;
        assert!(codex_meta_fresh(&old, 42));
        assert!(!codex_meta_fresh(&old, 43));
    }

    /// История метит сессию агентом, чтобы фронт скопировал верную команду resume
    /// (issue #10: codex-сессия не должна давать `claude --resume`).
    #[test]
    fn parse_meta_labels_claude_and_codex() {
        let dir = std::env::temp_dir().join("jarvis-history-agent-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Claude-транскрипт (~/.claude/projects/**/<sid>.jsonl).
        let claude = dir.join("sid-claude.jsonl");
        fs::write(
            &claude,
            r#"{"type":"user","cwd":"/tmp/proj","timestamp":"2026-06-29T10:00:00.000Z","message":{"role":"user","content":"привет, мир, это обычный пользовательский промпт"}}
"#,
        )
        .unwrap();
        let m = parse_meta(&claude, 1).expect("claude meta");
        assert_eq!(m.agent, "claude");
        assert!(!m.service);

        // Codex rollout (~/.codex/sessions/**/rollout-*.jsonl).
        let codex = dir.join("rollout-1-abc.jsonl");
        fs::write(
            &codex,
            r#"{"type":"session_meta","timestamp":"2026-06-29T10:00:00.000Z","payload":{"id":"abc","cwd":"/tmp/proj"}}
"#,
        )
        .unwrap();
        let m = parse_codex_meta(&codex, 1).expect("codex meta");
        assert_eq!(m.agent, "codex");
        assert_eq!(m.session_id, "abc");

        let _ = fs::remove_dir_all(&dir);
    }
}
