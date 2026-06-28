//! Codex-бэкенд (OpenAI `codex`). Sync-методы шва; async/stateful-части —
//! свободными функциями в profильных модулях. Транскрипт/agent-host наполняются
//! по инкрементам (см. план); здесь то, что известно статически.

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{Agent, Backend};
use crate::transcript::ChatItem;

pub struct CodexBackend;

/// Статический инстанс для диспетчера `backend()`.
pub static CODEX: CodexBackend = CodexBackend;

/// Настоящий `codex` в PATH (+типовые каталоги), минуя наш шим `~/.jarvis/shims`.
pub fn resolve_codex_bin() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    for extra in [
        crate::util::home_dir().join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    let shims = crate::util::jarvis_dir().join("shims");
    for d in dirs {
        if d == shims {
            continue;
        }
        let p = d.join("codex");
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

/// Найти rollout-файл codex по `session_id` (хвост имени файла = uuid сессии):
/// `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<sid>.jsonl`. Safety-net на случай,
/// когда hook-payload codex НЕ принёс `transcript_path` (хуки пропущены по hook-trust):
/// демон всё равно находит транскрипт по sid и достаёт модель + переписку. Возвращает
/// самый свежий матч; обход ограничен глубиной (YYYY/MM/DD — 3-4 уровня).
pub fn find_rollout_by_sid(sid: &str) -> Option<PathBuf> {
    find_rollout_in(&crate::util::codex_dir().join("sessions"), sid)
}

/// Чистое ядро поиска (тестируется на temp-каталоге без env): рекурсивный обход
/// `root` в поисках файла с хвостом `-<sid>.jsonl`, самый свежий по mtime.
fn find_rollout_in(root: &Path, sid: &str) -> Option<PathBuf> {
    if sid.is_empty() {
        return None;
    }
    let needle = format!("-{sid}.jsonl");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    fn walk(dir: &Path, needle: &str, best: &mut Option<(std::time::SystemTime, PathBuf)>, depth: u8) {
        if depth > 4 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, needle, best, depth + 1);
            } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(needle)) {
                if let Ok(mt) = e.metadata().and_then(|m| m.modified()) {
                    if best.as_ref().is_none_or(|(bt, _)| mt > *bt) {
                        *best = Some((mt, p));
                    }
                }
            }
        }
    }
    walk(root, &needle, &mut best, 0);
    best.map(|(_, p)| p)
}

impl Backend for CodexBackend {
    fn agent(&self) -> Agent {
        Agent::Codex
    }
    fn cli_found(&self) -> bool {
        resolve_codex_bin().is_some()
    }
    fn read_entries(&self, file: &Path, max_bytes: u64) -> Vec<Value> {
        // Codex rollout линейный (без uuid/parentUuid) → просто хвост JSONL.
        crate::transcript::read_recent_entries(file, max_bytes)
    }
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem> {
        super::codex_transcript::to_chat_items(entry)
    }
    fn extract_title(&self, entries: &[Value]) -> Option<String> {
        super::codex_transcript::extract_title(entries)
    }
    fn extract_branch(&self, _entries: &[Value]) -> Option<String> {
        None // session_meta.git отсутствует в rollout — ветка недоступна
    }
    fn extract_model(&self, entries: &[Value]) -> Option<String> {
        super::codex_transcript::extract_model(entries)
    }
    fn transcript_dir_for(&self, _cwd: &str) -> Option<PathBuf> {
        None // Codex не кодирует cwd в путь; индекс — инкремент 6 (history)
    }
    fn resume_cmd(&self, sid: &str) -> String {
        format!("codex resume {sid}")
    }
    fn friendly_model(&self, id: &str) -> String {
        let v = id.to_lowercase();
        if v.contains("codex") {
            return "Codex".to_string();
        }
        if v.contains("gpt-5") || v.contains("gpt5") {
            return "GPT-5".to_string();
        }
        if v.contains("o3") {
            return "o3".to_string();
        }
        // дефолт: первый сегмент, как util::friendly_model
        id.split('-').next().unwrap_or("").to_string()
    }
    fn models(&self) -> &'static [(&'static str, &'static str)] {
        &[("gpt-5.5", "GPT-5.5"), ("gpt-5-codex", "Codex"), ("gpt-5", "GPT-5")]
    }
    fn effort_levels(&self) -> &'static [&'static str] {
        &["minimal", "low", "medium", "high", "xhigh"]
    }
    fn has_separate_effort(&self) -> bool {
        false
    }
    fn price(&self, _model: &str) -> (f64, f64) {
        // ОЦЕНКА (OpenAI прайс дрейфует) — gpt-5-класс, $/1M (in, out).
        (1.25, 10.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_model_codex_names() {
        assert_eq!(CODEX.friendly_model("gpt-5-codex"), "Codex");
        assert_eq!(CODEX.friendly_model("gpt-5.5"), "GPT-5");
        assert_eq!(CODEX.resume_cmd("xyz"), "codex resume xyz");
        assert!(!CODEX.has_separate_effort());
    }

    #[test]
    fn find_rollout_locates_by_sid_recursively() {
        // ~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<sid>.jsonl — обход вглубь.
        let root = std::env::temp_dir().join("jarvis-find-rollout-test");
        let _ = std::fs::remove_dir_all(&root);
        let day = root.join("2026/06/29");
        std::fs::create_dir_all(&day).unwrap();
        let want = day.join("rollout-200-AAA-BBB.jsonl");
        std::fs::write(&want, b"{}\n").unwrap();
        std::fs::write(day.join("rollout-100-CCC-DDD.jsonl"), b"{}\n").unwrap();

        assert_eq!(find_rollout_in(&root, "AAA-BBB").as_deref(), Some(want.as_path()));
        assert_eq!(find_rollout_in(&root, "ZZZ"), None, "нет матча → None");
        assert_eq!(find_rollout_in(&root, ""), None, "пустой sid → None");

        let _ = std::fs::remove_dir_all(&root);
    }
}
