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
pub mod events;
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
    /// Имя агента для текстов интерфейса, тостов и озвучки (`label()` — машинная
    /// метка в нижнем регистре, её людям не показываем).
    pub fn title(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Kimi => "Kimi",
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
    /// Найти транскрипт по `session_id`, когда хук не принёс путь.
    ///
    /// У Claude путь всегда в payload. У Codex это safety-net на случай
    /// невыданного hook-trust. У Kimi — ЕДИНСТВЕННЫЙ способ: его хуки поля
    /// `transcript_path` не содержат вовсе.
    ///
    /// Ищет по ЭТОЙ файловой системе, поэтому для удалённой сессии не годится —
    /// звать только при `remote.is_none()`.
    fn find_transcript_by_sid(&self, _sid: &str) -> Option<PathBuf> {
        None
    }
    /// Фолбэк модели по рабочей папке, когда в транскрипте её нет. Только для
    /// агентов, чьи логи разложены по проектам (Claude) — остальным чужой
    /// каталог подсунул бы неверную модель. Возвращает уже «человеческое» имя.
    fn fallback_model_for_cwd(&self, _cwd: &str) -> Option<String> {
        None
    }
    /// Сколько хвоста транскрипта читать для финального ответа. У Claude JSONL
    /// плотнее (цепочка), остальным нужно больше.
    fn transcript_tail_bytes(&self) -> u64 {
        512 * 1024
    }
    /// Полный финальный ответ агента из записей транскрипта.
    fn final_reply(&self, entries: &[Value]) -> Option<String>;
    /// Финальный ответ прямо из payload Stop-хука, если агент его туда кладёт.
    ///
    /// Так умеет только Codex (`last_assistant_message`). Claude и Kimi финал в
    /// хук не приносят — им остаётся транскрипт. Знание о ключе живёт здесь,
    /// а не в редьюсере: имя поля — часть формата конкретного агента.
    ///
    /// Ценно не ради экономии: у Codex rollout на момент Stop может быть ещё не
    /// дописан, и payload оказывается единственным надёжным источником.
    fn final_reply_from_stop(&self, _payload: &serde_json::Map<String, Value>) -> Option<String> {
        None
    }

    // — подъём сессии —
    /// Заводит ли CLI сессию САМ, при старте, — до того как ему что-то написали.
    ///
    /// Claude и Codex заводят: `session-start` приходит через секунду после
    /// открытия терминала, и первый промпт можно отдать уже готовой сессии.
    ///
    /// Kimi Code — нет: он прямым текстом пишет на приветственном экране «No
    /// session yet — one will be created on your first message», поле `Session:`
    /// пустое, и хука `session-start` не приходит вовсе. Ждать его — взаимная
    /// блокировка: мы ждём сессию, чтобы отдать реплику, а сессии не будет, пока
    /// реплики нет. Такому агенту задача отдаётся прямо в пану, как только CLI
    /// готов её принять (`launch::ready`), и сессия заводится как следствие.
    ///
    /// Проверено вживую: kimi 0.38.0 против claude 2.1.x — у второго талон
    /// связался с сессией за секунду, у первого не связался никогда.
    fn session_before_prompt(&self) -> bool {
        true
    }

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

    /// Размер окна контекста модели, в токенах. ОЦЕНКА по имени — потому и
    /// живёт у бэкенда, а не константой в счётчике: у K3 миллион, у K3-256k
    /// четверть, у claude зависит от строки запуска. Настоящее число приносит
    /// сам CLI (`modelUsage.contextWindow`) и всегда важнее этой таблицы.
    /// `None` — модель незнакомая: соврать числом хуже, чем сказать «не знаю».
    fn context_window(&self, _model: &str) -> Option<u64> {
        None
    }
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
    /// Заголовок: Claude пишет его сам записями `ai-title`/`summary`.
    fn extract_title(&self, entries: &[Value]) -> Option<String> {
        entries.iter().rev().find_map(|e| {
            let t = match e.get("type").and_then(Value::as_str) {
                Some("ai-title") => e.get("aiTitle"),
                Some("summary") => e.get("summary"),
                _ => None,
            };
            t.and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
        })
    }
    /// Ветка: у Claude `gitBranch` есть в каждой записи (у остальных агентов нет —
    /// им остаётся общий фолбэк по `.git/HEAD` от cwd).
    fn extract_branch(&self, entries: &[Value]) -> Option<String> {
        entries.iter().rev().find_map(|e| {
            e.get("gitBranch")
                .and_then(Value::as_str)
                .filter(|b| !b.is_empty() && *b != "HEAD")
                .map(String::from)
        })
    }
    /// Модель — СЫРЫМ идентификатором; «человеческое» имя наводит вызывающий
    /// через `friendly_model`. Так одинаково у всех трёх бэкендов.
    fn extract_model(&self, entries: &[Value]) -> Option<String> {
        entries.iter().rev().find_map(|e| {
            (e.get("type").and_then(Value::as_str) == Some("assistant"))
                .then(|| e.pointer("/message/model").and_then(Value::as_str))
                .flatten()
                .map(String::from)
        })
    }
    fn transcript_dir_for(&self, cwd: &str) -> Option<PathBuf> {
        Some(crate::transcript::project_dir_for(cwd))
    }
    fn fallback_model_for_cwd(&self, cwd: &str) -> Option<String> {
        crate::transcript::read_model_from_project(cwd)
    }
    fn transcript_tail_bytes(&self) -> u64 {
        256 * 1024
    }
    fn final_reply(&self, entries: &[Value]) -> Option<String> {
        crate::transcript::final_reply_from(entries.to_vec())
    }
    fn resume_cmd(&self, sid: &str) -> String {
        format!("claude --resume {sid}")
    }
    fn friendly_model(&self, id: &str) -> String {
        crate::util::friendly_model(id)
    }
    fn models(&self) -> &'static [(&'static str, &'static str)] {
        // Порядок — это подсказка. Fable последней намеренно: по умолчанию её
        // не берут, а первая строка списка однажды будет нажата не глядя.
        &[("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku"), ("fable", "Fable")]
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
    /// У Claude окно задаёт СТРОКА ЗАПУСКА (`opus[1m]` в настройках), а в
    /// транскрипте от неё не остаётся и следа: там стоит голый `claude-opus-5`
    /// и при миллионе, и при двухстах тысячах. Отсюда 200 000 — только догадка
    /// по умолчанию, и опровергнуть её может само занятое (см. `context::gauge`).
    fn context_window(&self, model: &str) -> Option<u64> {
        let m = model.to_lowercase();
        if m.contains("1m") {
            return Some(1_000_000);
        }
        ["opus", "sonnet", "haiku", "fable"]
            .iter()
            .any(|k| m.contains(k))
            .then_some(200_000)
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

    /// Строку «Other» рисуют Claude и Kimi — проверено на живых пикерах.
    /// У Codex её нет: его вопрос детектится скрин-скрейпом и своего текста не
    /// принимает.
    #[test]
    fn custom_answer_capability_matches_pickers() {
        assert!(backend(Agent::Claude).supports_custom_answer(), "у Claude есть «Other»");
        assert!(backend(Agent::Kimi).supports_custom_answer(), "у Kimi «Other» тоже есть");
        assert!(!backend(Agent::Codex).supports_custom_answer());
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

    /// Майнинг меты Claude переехал из `daemon::refresh_meta` за трейт. Логика
    /// перенесена дословно — тест фиксирует именно её, чтобы переезд не оказался
    /// молчаливой сменой поведения.
    #[test]
    fn claude_mines_meta_from_transcript_entries() {
        use serde_json::json;
        let b = backend(Agent::Claude);
        let entries = vec![
            json!({"type": "assistant", "gitBranch": "main", "message": {"model": "claude-opus-4-8"}}),
            json!({"type": "summary", "summary": "  старая сводка  "}),
            json!({"type": "assistant", "gitBranch": "feat/x", "message": {"model": "claude-sonnet-4-5"}}),
            json!({"type": "ai-title", "aiTitle": "Свежий заголовок"}),
        ];
        // берётся ПОСЛЕДНЕЕ вхождение — идём с конца
        assert_eq!(b.extract_branch(&entries).as_deref(), Some("feat/x"));
        assert_eq!(b.extract_title(&entries).as_deref(), Some("Свежий заголовок"));
        assert_eq!(b.extract_model(&entries).as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(b.friendly_model("claude-sonnet-4-5"), "Sonnet");

        // мусорные значения ветки игнорируются
        let junk = vec![json!({"gitBranch": "HEAD"}), json!({"gitBranch": ""})];
        assert_eq!(b.extract_branch(&junk), None);
        assert_eq!(b.extract_title(&[]), None);
    }

    /// Ветка и заголовок Claude не должны «находиться» у других агентов: формат
    /// чужой, и совпадение поля означало бы случайную мету.
    #[test]
    fn other_agents_do_not_mine_claude_fields() {
        use serde_json::json;
        let claude_shaped = vec![json!({"type": "assistant", "gitBranch": "main"})];
        for a in [Agent::Codex, Agent::Kimi] {
            assert_eq!(backend(a).extract_branch(&claude_shaped), None, "{a:?}");
        }
    }

    /// Фолбэк модели по каталогу проектов есть только у Claude: у остальных он
    /// рылся бы в ~/.claude/projects и подсунул чужую модель.
    #[test]
    fn project_model_fallback_is_claude_only() {
        assert!(backend(Agent::Codex).fallback_model_for_cwd("/tmp/x").is_none());
        assert!(backend(Agent::Kimi).fallback_model_for_cwd("/tmp/x").is_none());
    }

    #[test]
    fn all_agents_are_dispatchable_and_labels_round_trip() {
        for a in Agent::all() {
            assert_eq!(backend(*a).agent(), *a, "диспетчер обязан вернуть тот же агент");
            assert_eq!(Agent::from_label(a.label()), *a, "метка обязана читаться обратно");
        }
    }

    /// Потолок контекста берётся ИЗ МОДЕЛИ, а не из общей константы: у K3
    /// миллион, у K3-256k четверть, у claude — двести тысяч по умолчанию и
    /// миллион с пометкой `1m`. Незнакомая модель — `None`, а не чужое число.
    #[test]
    fn context_window_comes_from_the_model() {
        let c = backend(Agent::Claude);
        assert_eq!(c.context_window("claude-opus-5"), Some(200_000));
        assert_eq!(c.context_window("opus[1m]"), Some(1_000_000), "пометка про миллион");
        assert_eq!(c.context_window("claude-sonnet-4-5-1m"), Some(1_000_000));
        assert_eq!(c.context_window("gpt-5"), None, "чужая модель — не наше дело");

        let k = backend(Agent::Kimi);
        assert_eq!(k.context_window("kimi-code/k3"), Some(1_000_000));
        assert_eq!(k.context_window("k3"), Some(1_000_000), "короткое имя — та же модель");
        assert_eq!(k.context_window("kimi-code/k3-256k"), Some(256_000));
        assert_eq!(k.context_window("что-то новое"), None);

        assert_eq!(backend(Agent::Codex).context_window("gpt-5-codex"), Some(400_000));
        assert_eq!(backend(Agent::Codex).context_window("k3"), None);
    }

    #[test]
    fn codex_backend_basics() {
        let b = backend(Agent::Codex);
        assert!(!b.has_separate_effort());
        assert_eq!(b.resume_cmd("xyz"), "codex resume xyz");
        assert!(b.effort_levels().contains(&"minimal"));
    }
}
