//! Бэкенд-абстракция: один шов между Jarvis и разными CLI-агентами
//! (Claude Code, Codex, Kimi Code). Принцип — `enum Agent` + sync dyn-safe `trait Backend`
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
pub mod codex_agent;
pub mod codex_transcript;
pub mod kimi;
pub mod kimi_transcript;

/// Какой CLI-агент стоит за сессией/вызовом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
    Kimi,
}

impl Agent {
    /// Метка из конверта хука (`{"agent":"codex"}`). Неизвестное → Claude
    /// (обратная совместимость: старые state.json без метки = claude).
    ///
    /// Таблично по `label()`: добавляя агента, правишь одно место, а не цепочку
    /// `if`-ов, где легко забыть ветку и молча получить чужое поведение.
    pub fn from_label(s: &str) -> Agent {
        Agent::all()
            .iter()
            .copied()
            .find(|a| s.eq_ignore_ascii_case(a.label()))
            .unwrap_or(Agent::Claude)
    }
    pub fn from_opt(s: Option<&str>) -> Agent {
        s.map(Agent::from_label).unwrap_or(Agent::Claude)
    }
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Kimi => "kimi",
        }
    }
    /// Все известные агенты. Срез, а не массив фиксированной длины: список растёт,
    /// и типу незачем ломаться на каждом новом бэкенде.
    pub fn all() -> &'static [Agent] {
        &[Agent::Claude, Agent::Codex, Agent::Kimi]
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
    /// То же над уже прочитанным текстом. Транскрипт удалённой сессии приезжает
    /// по HTTP с узла: файла на этой машине нет, а разбор нужен тот же самый.
    fn entries_from_text(&self, text: &str) -> Vec<Value>;
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem>;
    fn extract_title(&self, entries: &[Value]) -> Option<String>;
    fn extract_branch(&self, entries: &[Value]) -> Option<String>;
    fn extract_model(&self, entries: &[Value]) -> Option<String>;
    fn transcript_dir_for(&self, cwd: &str) -> Option<PathBuf>;

    // — control / identity —
    /// Умеет ли пикер агента строку «Other» (свой ответ текстом). У Claude есть,
    /// у Codex нет. Раньше это выражалось сравнением с `Agent::Codex` в трёх
    /// местах сразу (ipc, tmux, UI) — теперь одна точка правды.
    fn supports_custom_answer(&self) -> bool {
        true
    }
    /// Проверить модель перед вставкой в `/model …`.
    ///
    /// Дефолт: значение «чистое» И входит в `models()`. Проверка на «чистоту»
    /// (SEC-3: без пробелов и control-символов) обязательна для ЛЮБОГО агента —
    /// строка уходит слэш-командой в tmux-пану.
    fn validate_model(&self, model: &str) -> Result<(), String> {
        crate::convo::skills::ensure_clean(model, "модель")?;
        if self.models().iter().any(|(id, _)| *id == model) {
            Ok(())
        } else {
            Err(format!("неизвестная модель: {model}"))
        }
    }
    /// Проверить уровень effort перед вставкой в `/effort …`. Дефолт — как у модели.
    fn validate_effort(&self, level: &str) -> Result<(), String> {
        crate::convo::skills::ensure_clean(level, "effort")?;
        if self.effort_levels().contains(&level) {
            Ok(())
        } else {
            Err(format!("неизвестный effort: {level}"))
        }
    }
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
    fn entries_from_text(&self, text: &str) -> Vec<Value> {
        crate::transcript::chain_from_entries(crate::transcript::entries_from_text(text))
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
        Agent::Kimi => &kimi::KIMI,
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

    /// Аллоулист Claude — тот же, что был в `convo::skills` до переезда в трейт
    /// (SEC-3). Ассерты перенесены дословно: инвариант «Claude байт-в-байт».
    #[test]
    fn claude_validates_model_and_effort_by_allowlist() {
        let b = backend(Agent::Claude);
        assert!(b.validate_model("opus").is_ok());
        assert!(b.validate_model("sonnet").is_ok());
        assert!(b.validate_model("gpt-4").is_err());
        assert!(b.validate_model("opus; rm -rf").is_err());
        assert!(b.validate_effort("high").is_ok());
        assert!(b.validate_effort("ultra").is_err());
        // пробелы и control-символы — инъекция в slash-команду
        assert!(b.validate_model("op us").is_err());
        assert!(b.validate_model("opus\n").is_err());
        assert!(b.validate_effort("hi gh").is_err());
    }

    /// У Codex набор моделей дрейфует между релизами, поэтому аллоулистом его не
    /// ограничиваем — но «чистоту» строки проверяем, она уходит в tmux-пану.
    #[test]
    fn codex_validates_only_cleanliness() {
        let b = backend(Agent::Codex);
        assert!(b.validate_model("gpt-5.1-something-new").is_ok());
        assert!(b.validate_model("gpt-5; rm -rf /").is_err(), "инъекция обязана падать");
        assert!(b.validate_model("").is_err());
    }

    #[test]
    fn custom_answer_capability_matches_pickers() {
        assert!(backend(Agent::Claude).supports_custom_answer(), "у Claude есть «Other»");
        assert!(!backend(Agent::Codex).supports_custom_answer());
        assert!(!backend(Agent::Kimi).supports_custom_answer());
    }

    #[test]
    fn kimi_backend_basics() {
        let b = backend(Agent::Kimi);
        assert_eq!(b.agent(), Agent::Kimi);
        assert_eq!(b.resume_cmd("session_abc"), "kimi -S session_abc");
        assert!(b.has_separate_effort(), "effort у Kimi отдельный, как у Claude");
        assert!(b.validate_model("kimi-code/k3").is_ok());
        assert!(b.validate_model("kimi-code/k3; rm -rf").is_err());
        assert!(b.validate_effort("max").is_ok());
        assert!(b.validate_effort("medium").is_err(), "у Kimi нет medium");
    }

    #[test]
    fn all_agents_are_dispatchable_and_labels_round_trip() {
        for a in Agent::all() {
            assert_eq!(backend(*a).agent(), *a, "диспетчер обязан вернуть тот же агент");
            assert_eq!(Agent::from_label(a.label()), *a, "метка обязана читаться обратно");
        }
    }

    #[test]
    fn codex_backend_basics() {
        let b = backend(Agent::Codex);
        assert!(!b.has_separate_effort());
        assert_eq!(b.resume_cmd("xyz"), "codex resume xyz");
        assert!(b.effort_levels().contains(&"minimal"));
    }
}
