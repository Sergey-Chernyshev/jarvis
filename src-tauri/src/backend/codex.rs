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
}
