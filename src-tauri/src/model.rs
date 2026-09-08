//! Модель сессии Claude Code в реестре демона.
//!
//! Сериализация — camelCase и skip-None: JSON для панели и state.json на диске
//! полностью совместимы с Electron-версией (рендерер и старые файлы не заметят
//! смены рантайма).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Idle,
    Working,
    Waiting,
    Done,
    Limit,
}

impl Status {
    /// Порядок сортировки списка: кто требует внимания — выше.
    pub fn order(self) -> u8 {
        match self {
            Status::Waiting => 0,
            Status::Limit => 1,
            Status::Working => 2,
            Status::Done => 3,
            Status::Idle => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct QuestionItem {
    pub id: String,
    pub question: String,
    pub header: String,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
    /// An explicit capability of this question, not an assumption about its agent.
    pub custom_allowed: Option<bool>,
    /// notes retains the selected option; alternative replaces it.
    pub custom_mode: String,
    pub is_secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScreenQuestionState {
    /// Content identity excludes cursor and checkbox state.
    pub fingerprint: String,
    pub cursor: u32,
    pub selected: Vec<u32>,
    /// Original terminal option numbers, including gaps left by Other.
    pub option_numbers: Vec<u32>,
    pub custom_index: Option<u32>,
    pub picker: String,
    pub editing: bool,
}

/// Вопрос, ждущий ответа: из хука AskUserQuestion либо распознанный на экране
/// tmux-паны (`from_screen`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Question {
    pub request_id: String,
    pub revision: u64,
    /// tmux, codex-rpc, or external (readable but not owned by this connection).
    pub transport: String,
    pub provider_turn_id: Option<String>,
    pub provider_item_id: Option<String>,
    pub rpc_request_id: Option<serde_json::Value>,
    pub screen: Option<ScreenQuestionState>,
    pub at: i64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub from_screen: bool,
    pub questions: Vec<QuestionItem>,
}

/// Одна задача доски. Источник — оркестратор сессии (TodoWrite / Task-тулы),
/// Jarvis её только читает. `status`: completed | in_progress | pending |
/// interrupted (последнее — задача была в работе на момент смерти сессии).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskItem {
    /// Позиционный номер (1-based) — то, что в UI показывается как «Task N».
    pub n: i64,
    pub text: String,
    pub status: String,
    /// «Exploring …» — живая форма для строки активности in-progress задачи.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    /// Модель — только из УВЕРЕННО скоррелированного сабагента, иначе None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Длительность мс: in_progress→completed, best-effort по снапшотам.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dur_ms: Option<i64>,
    /// Когда задача стала in_progress (для живого таймера), мс.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
}

/// Сабагент сессии. Старт — PreToolUse(Task), стоп — PostToolUse(Task).
/// `task_ref` = номер задачи, если описание уверенно ссылается на «Task N».
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Subagent {
    pub name: String,
    /// subagent_type из tool_input (code-reviewer, general-purpose…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<i64>,
}

/// Доска задач сессии. Появляется в панели только если была хоть раз заполнена.
/// `stopped` — сессия умерла, доска заморожена (in_progress → interrupted).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskBoard {
    pub tasks: Vec<TaskItem>,
    /// Сабагенты без уверенной привязки — для отдельной полоски в UI.
    pub subagents: Vec<Subagent>,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Session {
    pub id: String,
    pub status: Status,
    pub detail: String,
    pub created_at: i64,
    pub updated_at: i64,

    /// Локальная ревизия хода для асинхронных эффектов; после рестарта
    /// незавершённых задач в памяти уже нет, на диск её писать незачем.
    #[serde(skip)]
    pub lifecycle_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Provider configuration identity is independent of the raw provider SID.
    /// Two accounts may contain copies of the same conversation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// `hook` or `rollout`: monitoring does not imply a writable transport.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_event_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_last_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_pane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_name: Option<String>,
    /// TERM_PROGRAM / TERMINAL_EMULATOR терминала, где живёт сессия.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    /// pid процесса claude (= $PPID хука).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    /// GUI-приложение-владелец терминала (WebStorm, iTerm2…), резолвится по pid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Имя удалённого узла, откуда пришла сессия; None — локальная. Ключ реестра
    /// для удалённой — `<remote>:<id>`: id с разных машин не обязаны различаться.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,

    /// «Последняя задача» от юзера — живёт дольше detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,
    /// Момент последнего завершённого ответа — по нему сортируется список.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_at: Option<i64>,
    /// Ждёт авто-«продолжай» после сброса лимита провайдера.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub limit_wait: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<Question>,

    /* ----- идентичность: ветка, заголовок, модель, effort ----- */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Момент ручного выбора модели — транскрипт не должен сразу перетирать.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_at: Option<i64>,
    /// Effort снаружи не читается — ведём оптимистично (что сами выставили).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,

    /* ----- «чем занята сейчас»: задачи и саммари ----- */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_progress: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todo_list: Option<Vec<String>>,
    /// Структурная доска задач (TodoWrite / Task-тулы). Источник истины —
    /// оркестратор сессии; Jarvis читает и отображает, не мутирует.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<TaskBoard>,
    /// Реестр сабагентов сессии (Task pre/post). Ведётся отдельно от задач;
    /// привязка к задаче — эвристическая, см. [`TaskBoard`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<Subagent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_at: Option<i64>,

    /* ----- живая активность из tool-событий ----- */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_cmd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub touched: Option<Vec<String>>,

    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Имя, которым уже подписали tmux-окно (не дёргаем rename повторно).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_to: Option<String>,
}

impl Session {
    /// Идентификатор сессии в том виде, в котором её знает САМ агент.
    ///
    /// Ключ реестра у сессии с узла — `<узел>:<id>`: id с разных машин не
    /// обязаны различаться. Но `claude --resume` на той машине про наш префикс
    /// ничего не знает, и подсказка с ним отправляет человека выполнять
    /// команду, которая заведомо не сработает.
    pub fn agent_id(&self) -> &str {
        if let Some(id) = self.provider_session_id.as_deref().filter(|id| !id.is_empty()) {
            return id;
        }
        match &self.remote {
            Some(node) => self.id.strip_prefix(&format!("{node}:")).unwrap_or(&self.id),
            None => &self.id,
        }
    }

    pub fn new(id: String, now: i64) -> Self {
        Session {
            id,
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
    }
}

/// Снапшот для панели: ждущие выше, свежие выше.
pub fn sort_snapshot(list: &mut [Session]) {
    list.sort_by(|a, b| {
        a.status
            .order()
            .cmp(&b.status.order())
            .then(b.updated_at.cmp(&a.updated_at))
    });
}

#[cfg(test)]
mod id_tests {
    use super::*;

    #[test]
    fn agent_id_drops_the_node_prefix() {
        let mut s = Session::new("vps:abc-123".into(), 0);
        s.remote = Some("vps".into());
        assert_eq!(s.agent_id(), "abc-123", "агенту его собственный id, без нашего ключа");

        // локальная сессия префикса не имеет — трогать нечего
        let l = Session::new("abc-123".into(), 0);
        assert_eq!(l.agent_id(), "abc-123");

        // двоеточие внутри самого id: снимаем ровно префикс узла, не больше
        let mut odd = Session::new("vps:a:b".into(), 0);
        odd.remote = Some("vps".into());
        assert_eq!(odd.agent_id(), "a:b");
    }
}
