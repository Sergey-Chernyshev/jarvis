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

/// Кто поднял сессию через `sessions.spawn`: кто именно, откуда, зачем и когда.
/// Есть только у дочерних сессий — человек должен видеть дерево, а не россыпь
/// окон. Ручной запуск из панели/терминала этого поля не имеет.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SpawnOrigin {
    /// Потребитель гейта: `agent`, `plugin:x`.
    pub by: String,
    /// Чат/сессия, из которой поднимали.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Зачем подняли — первый промпт, урезанный до строки.
    pub task: String,
    pub at: i64,
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
    /// ВИДИМЫЙ заголовок чата — им пользуются все потребители (список, тосты,
    /// голосовой снимок, tmux). Не пишется напрямую: собирается `retitle()` из
    /// `name` и `auto_title`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Имя, которое дал человек. Живёт не здесь: источник истины — настройки
    /// (`chatNames`), потому что сессия уходит из реестра по session-end, а имя
    /// обязано пережить и её, и перезапуск.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Автозаголовок из транскрипта. Хранится отдельно от `title`, чтобы снятие
    /// имени вернуло заголовок сразу, а не после следующего разбора транскрипта.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_title: Option<String>,
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
    /// Сессия поднята через `sessions.resume`, а не начата заново.
    ///
    /// Нужно в списке: «та самая сессия с прежней памятью» и «новая с похожим
    /// именем» — разные вещи, и по одному имени их не различить. Старые записи
    /// состояния поля не знают, `default` читается как «обычная».
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub revived: bool,

    /* ----- живая активность из tool-событий ----- */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_cmd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub touched: Option<Vec<String>>,

    /// Родитель: кто поднял эту сессию, откуда и зачем (см. [`SpawnOrigin`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawned_by: Option<SpawnOrigin>,

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

    /// Единственное место сборки видимого заголовка: имя человека сильнее
    /// автозаголовка. Звать после любой правки `name`/`auto_title` — иначе
    /// автогенерация затрёт имя, а сброс не вернёт заголовок.
    pub fn retitle(&mut self) {
        self.title = self.name.clone().or_else(|| self.auto_title.clone());
    }

    /// Старый state.json знает только `title`, и это был автозаголовок. Без
    /// усыновления первый же `retitle()` стёр бы заголовок восстановленной сессии.
    pub fn adopt_legacy_title(&mut self) {
        if self.auto_title.is_none() {
            self.auto_title = self.title.clone();
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
mod title_tests {
    use super::*;

    #[test]
    fn user_name_beats_the_generated_title() {
        let mut s = Session::new("sid".into(), 0);
        s.auto_title = Some("Fix the migration parser".into());
        s.retitle();
        assert_eq!(s.title.as_deref(), Some("Fix the migration parser"));

        s.name = Some("БД".into());
        s.retitle();
        assert_eq!(s.title.as_deref(), Some("БД"), "имя человека сильнее генерации");

        // автогенерация продолжает работать, но видимое имя не трогает
        s.auto_title = Some("Другой заголовок из транскрипта".into());
        s.retitle();
        assert_eq!(s.title.as_deref(), Some("БД"), "транскрипт затёр имя");
    }

    #[test]
    fn dropping_the_name_brings_the_auto_title_back() {
        let mut s = Session::new("sid".into(), 0);
        s.auto_title = Some("Разбор транскрипта".into());
        s.name = Some("БД".into());
        s.retitle();

        s.name = None;
        s.retitle();
        assert_eq!(
            s.title.as_deref(),
            Some("Разбор транскрипта"),
            "сброс обязан вернуть автозаголовок сразу, не дожидаясь разбора"
        );

        // автозаголовка ещё не было — снятие имени оставляет строку без заголовка
        let mut fresh = Session::new("sid2".into(), 0);
        fresh.name = Some("БД".into());
        fresh.retitle();
        fresh.name = None;
        fresh.retitle();
        assert_eq!(fresh.title, None);
    }

    // Обратная совместимость: в старом state.json есть только `title`.
    #[test]
    fn legacy_state_file_keeps_its_title() {
        let raw = r#"{"id":"abc","status":"idle","detail":"","createdAt":1,"updatedAt":2,
            "title":"Старый заголовок","project":"jarvis"}"#;
        let mut s: Session = serde_json::from_str(raw).expect("старая запись обязана читаться");
        assert_eq!(s.auto_title, None);
        s.adopt_legacy_title();
        s.retitle();
        assert_eq!(s.title.as_deref(), Some("Старый заголовок"), "заголовок потерян");
        assert_eq!(s.auto_title.as_deref(), Some("Старый заголовок"));
        assert_eq!(s.project.as_deref(), Some("jarvis"), "остальные поля целы");
    }

    // Панель читает те же поля, что демон пишет в state.json.
    #[test]
    fn name_and_auto_title_survive_a_round_trip() {
        let mut s = Session::new("abc".into(), 7);
        s.auto_title = Some("Auto".into());
        s.name = Some("БД".into());
        s.retitle();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"autoTitle\""), "camelCase для панели: {json}");
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name.as_deref(), Some("БД"));
        assert_eq!(back.auto_title.as_deref(), Some("Auto"));
        assert_eq!(back.title.as_deref(), Some("БД"));

        // безымянная сессия лишних ключей в файл не льёт
        let plain = serde_json::to_string(&Session::new("x".into(), 0)).unwrap();
        assert!(!plain.contains("autoTitle") && !plain.contains("\"name\""));
    }
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
