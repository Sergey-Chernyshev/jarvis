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
