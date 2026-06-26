//! Бэкенд-абстракция: один шов между Jarvis и разными CLI-агентами
//! (Claude Code, Codex). Принцип — `enum Agent` + sync dyn-safe `trait Backend`
//! для чистых данных/форматирования (диспетч `backend(agent)`), а вся
//! async/stateful-логика (контроль, usage, service-LLM, agent-host) живёт
//! свободными функциями `match agent` в своих модулях — как `claude_bin.rs`.
//!
//! Инвариант: поведение Claude байт-в-байт прежнее. Claude-методы делегируют в
//! существующий код; Codex-методы наполняются по инкрементам (см. план).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::transcript::ChatItem;

pub mod codex;
pub mod codex_transcript;

/// Какой CLI-агент стоит за сессией/вызовом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
}

impl Agent {
    /// Метка из конверта хука (`{"agent":"codex"}`). Неизвестное → Claude
    /// (обратная совместимость: старые state.json без метки = claude).
    pub fn from_label(s: &str) -> Agent {
        if s.eq_ignore_ascii_case("codex") {
            Agent::Codex
        } else {
            Agent::Claude
        }
    }
    pub fn from_opt(s: Option<&str>) -> Agent {
        s.map(Agent::from_label).unwrap_or(Agent::Claude)
    }
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }
    pub fn all() -> [Agent; 2] {
        [Agent::Claude, Agent::Codex]
    }
}

// Примечание: таблицы событий хуков (CLAUDE/CODEX) живут в install/mod.rs —
// он компилируется отдельным бинарём jarvis-setup (`#[path] mod install`) БЕЗ
// остального крейта, поэтому provisioning самодостаточен и не зависит от backend.

/// Sync, dyn-safe часть шва: чистые данные/форматирование для рантайма демона.
/// Всё async/stateful (контроль, usage-скан, service-LLM, agent-host) — свободными
/// функциями `match agent` в своих модулях. Provisioning — в install/mod.rs.
pub trait Backend: Send + Sync {
    fn agent(&self) -> Agent;

    /// Установлен ли настоящий бинарь агента (минуя наш шим).
    fn cli_found(&self) -> bool;

    // — transcript → ChatItem —
    /// Прочитать хвост транскрипта в массив записей (Claude: read+chain;
    /// Codex: просто read — лог линейный).
    fn read_entries(&self, file: &Path, max_bytes: u64) -> Vec<Value>;
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem>;
    fn extract_title(&self, entries: &[Value]) -> Option<String>;
    fn extract_branch(&self, entries: &[Value]) -> Option<String>;
    fn extract_model(&self, entries: &[Value]) -> Option<String>;
    fn transcript_dir_for(&self, cwd: &str) -> Option<PathBuf>;

    // — control / identity —
    fn resume_cmd(&self, sid: &str) -> String;
    fn friendly_model(&self, id: &str) -> String;
    fn models(&self) -> &'static [(&'static str, &'static str)];
    fn effort_levels(&self) -> &'static [&'static str];
    /// У Claude отдельный `/effort`; у Codex effort внутри `/model`-пикера → UI
    /// прячет отдельный effort-селектор.
    fn has_separate_effort(&self) -> bool;

    // — usage —
    /// $/1M токенов (input, output). Оценка/конфиг.
    fn price(&self, model: &str) -> (f64, f64);
}

/// Claude-бэкенд: делегирует в существующий код (поведение неизменно).
pub struct ClaudeBackend;

impl Backend for ClaudeBackend {
    fn agent(&self) -> Agent {
        Agent::Claude
    }
    fn cli_found(&self) -> bool {
        crate::claude_bin::resolve_claude_bin().is_some()
    }
    fn read_entries(&self, file: &Path, max_bytes: u64) -> Vec<Value> {
        crate::transcript::chain_from_entries(crate::transcript::read_recent_entries(file, max_bytes))
    }
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem> {
        crate::transcript::to_chat_items(entry)
    }
    // extract_* — Claude майнит из daemon.rs::refresh_meta; вынос за бэкенд в
    // инкременте 3 (тогда же подключаются call-sites). Пока не вызываются.
    fn extract_title(&self, _entries: &[Value]) -> Option<String> {
        None
    }
    fn extract_branch(&self, _entries: &[Value]) -> Option<String> {
        None
    }
    fn extract_model(&self, _entries: &[Value]) -> Option<String> {
        None
    }
    fn transcript_dir_for(&self, cwd: &str) -> Option<PathBuf> {
        Some(crate::transcript::project_dir_for(cwd))
    }
    fn resume_cmd(&self, sid: &str) -> String {
        format!("claude --resume {sid}")
    }
    fn friendly_model(&self, id: &str) -> String {
        crate::util::friendly_model(id)
    }
    fn models(&self) -> &'static [(&'static str, &'static str)] {
        &[("fable", "Fable"), ("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
    }
    fn effort_levels(&self) -> &'static [&'static str] {
        &["low", "medium", "high", "xhigh", "max"]
    }
    fn has_separate_effort(&self) -> bool {
        true
    }
    fn price(&self, model: &str) -> (f64, f64) {
        // зеркало usage::price (Claude) — единый прайс per friendly-model.
        match model {
            "Opus" | "Fable" => (15.0, 75.0),
            "Haiku" => (1.0, 5.0),
            _ => (3.0, 15.0),
        }
    }
}

static CLAUDE: ClaudeBackend = ClaudeBackend;

/// Диспетчер: статический бэкенд по агенту.
pub fn backend(a: Agent) -> &'static dyn Backend {
    match a {
        Agent::Claude => &CLAUDE,
        Agent::Codex => &codex::CODEX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_from_label_defaults_to_claude() {
        assert_eq!(Agent::from_label("codex"), Agent::Codex);
        assert_eq!(Agent::from_label("Codex"), Agent::Codex);
        assert_eq!(Agent::from_label("claude"), Agent::Claude);
        assert_eq!(Agent::from_label("whatever"), Agent::Claude);
        assert_eq!(Agent::from_opt(None), Agent::Claude);
        assert_eq!(Agent::Codex.label(), "codex");
        assert_eq!(Agent::Claude.label(), "claude");
    }

    #[test]
    fn dispatcher_returns_matching_agent() {
        assert_eq!(backend(Agent::Claude).agent(), Agent::Claude);
        assert_eq!(backend(Agent::Codex).agent(), Agent::Codex);
    }

    #[test]
    fn claude_backend_basics_unchanged() {
        let b = backend(Agent::Claude);
        assert_eq!(b.friendly_model("claude-opus-4-8"), "Opus");
        assert_eq!(b.resume_cmd("abc"), "claude --resume abc");
        assert!(b.has_separate_effort());
        assert_eq!(b.models().len(), 4);
    }

    #[test]
    fn codex_backend_basics() {
        let b = backend(Agent::Codex);
        assert!(!b.has_separate_effort());
        assert_eq!(b.resume_cmd("xyz"), "codex resume xyz");
        assert!(b.effort_levels().contains(&"minimal"));
    }
}
