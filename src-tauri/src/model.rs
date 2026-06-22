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
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct QuestionItem {
    pub question: String,
    pub header: String,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

/// Вопрос, ждущий ответа: из хука AskUserQuestion либо распознанный на экране
/// tmux-паны (`from_screen`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Question {
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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
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
