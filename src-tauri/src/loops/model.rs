//! Модель цикла: конфигурация, запуск, итерация.
//!
//! Цикл — это рутина, которую агент крутит сам: ночью или по расписанию. От
//! обычной сессии он отличается тем, что у него есть КОНЕЦ (условие выхода) и
//! есть СТЕНЫ (ограничители). Без первого он не завершится, без вторых съест
//! лимит аккаунта за ночь — поэтому оба обязательны и оба живут в конфигурации,
//! а не в голове у человека.
//!
//! Сериализация — camelCase: конфигурация уезжает в панель как есть.

use serde::{Deserialize, Serialize};

/// Откуда цикл берёт задачи на итерацию.
///
/// Команда, а не список: список устарел бы к первой же ночи. `gh issue list
/// --label agent`, `cargo test 2>&1 | grep FAILED` — что угодно, что печатает
/// работу в stdout.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Source {
    /// Человеческое описание — оно же цель цикла, если команды нет.
    pub goal: String,
    /// Команда, чей stdout становится списком задач итерации.
    pub command: String,
}

/// Песочница: где цикл работает и чего ему нельзя.
///
/// Отдельный worktree — не украшение: агент правит файлы часами без надзора, и
/// делать это в рабочем дереве человека значит однажды застать его посреди
/// своей же несохранённой правки. Радиус поражения — ветка.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Sandbox {
    /// Репозиторий, вокруг которого крутится цикл.
    pub repo: String,
    /// Шаблон ветки: `{n}` подставляется номером запуска.
    pub branch: String,
    /// Ставить ли отдельный worktree (иначе — прямо в репозитории).
    pub worktree: bool,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self { repo: String::new(), branch: "loop/{name}-{n}".into(), worktree: true }
    }
}

/// Детерминированный гейт: команда, чей нулевой код выхода означает «прошло».
///
/// Именно детерминированный — в противовес критику. Мнение субагента полезно,
/// но выпускать работу в мир по одному лишь мнению нельзя.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Gate {
    pub name: String,
    pub command: String,
}

/// Субагент-критик: отдельный вызов агента, который ревьюит дифф итерации.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Critic {
    pub enabled: bool,
    /// Модель критика: на ревью обычно ставят сильнее, чем на исполнение.
    pub model: String,
    /// Свой промт; пустой — возьмётся встроенный.
    pub prompt: String,
}

impl Default for Critic {
    fn default() -> Self {
        Self { enabled: true, model: "opus".into(), prompt: String::new() }
    }
}

/// Условие выхода: когда цикл считает работу сделанной.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Exit {
    pub gates: Vec<Gate>,
    pub critic: Critic,
    /// Сколько итераций подряд всё должно быть зелёным.
    ///
    /// Одной мало: гейт мог пройти случайно (флаки-тест — ровно тот случай,
    /// ради которого такие циклы и заводят).
    pub streak: u32,
}

impl Default for Exit {
    fn default() -> Self {
        Self { gates: Vec::new(), critic: Critic::default(), streak: 2 }
    }
}

/// Память цикла: что переживает итерацию.
///
/// Без неё каждая итерация начинается с чистого листа — день сурка, в котором
/// агент второй раз наступает на те же грабли и второй раз их описывает.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Memory {
    pub enabled: bool,
    /// Файл дневника относительно репозитория.
    pub file: String,
}

impl Default for Memory {
    fn default() -> Self {
        Self { enabled: true, file: "notes.md".into() }
    }
}

/// Когда просыпаться.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Wake {
    /// Только руками.
    #[default]
    Manual,
    /// Каждый день в `HH:MM` местного времени.
    Daily { at: String },
    /// Каждые N минут.
    Every { minutes: u32 },
}

/// Расписание и поведение вокруг сна машины.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Schedule {
    pub wake: Wake,
    /// Возобновлять запуск после сброса лимита аккаунта.
    pub resume_after_limit: bool,
    /// Держать машину бодрствующей, пока цикл крутится.
    pub keep_awake: bool,
}

impl Default for Schedule {
    fn default() -> Self {
        Self { wake: Wake::Manual, resume_after_limit: true, keep_awake: true }
    }
}

/// Ограничители: цикл остановится сам.
///
/// Ноль означает «без ограничения» — но собрать цикл вообще без стен нельзя,
/// это проверяет [`Loop::problems`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    /// Токенов за запуск.
    pub tokens: u64,
    /// Итераций за запуск.
    pub iterations: u32,
    /// Минут за запуск.
    pub minutes: u32,
    /// Остановиться, если цикл ушёл от исходной цели.
    pub stop_on_drift: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self { tokens: 200_000, iterations: 20, minutes: 480, stop_on_drift: true }
    }
}

/// Выборочная проверка: приоткрытая дверь.
///
/// Не «показывать всё» и не «не показывать ничего»: первое убивает смысл
/// автономности, второе — доверие к ней.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Sampling {
    /// Каждая N-я итерация попадает человеку на глаза. 0 — выключено.
    pub every: u32,
}

impl Default for Sampling {
    fn default() -> Self {
        Self { every: 3 }
    }
}

/// Цикл целиком.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Loop {
    pub id: String,
    pub name: String,
    /// Агент, которым крутить: `claude` | `codex`.
    pub agent: String,
    pub source: Source,
    pub sandbox: Sandbox,
    pub exit: Exit,
    pub memory: Memory,
    pub schedule: Schedule,
    pub limits: Limits,
    pub sampling: Sampling,
    pub created_at: i64,
    /// Когда цикл в последний раз просыпался.
    pub last_run_at: i64,
}

impl Loop {
    /// Чего не хватает, чтобы цикл можно было запустить.
    ///
    /// Возвращается списком, а не первой ошибкой: человек заполняет форму
    /// целиком и вправе увидеть все дыры разом, а не по одной за подход.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push("у цикла нет имени".into());
        }
        if self.sandbox.repo.trim().is_empty() {
            out.push("не указан репозиторий".into());
        }
        if self.source.goal.trim().is_empty() && self.source.command.trim().is_empty() {
            out.push("не задан источник задач: ни цели, ни команды".into());
        }
        // Условие выхода — обязательное. Цикл без него не завершится никогда, и
        // единственным его концом будет ограничитель, то есть авария.
        if self.exit.gates.is_empty() && !self.exit.critic.enabled {
            out.push("нет условия выхода: ни гейтов, ни критика".into());
        }
        if self.limits.tokens == 0 && self.limits.iterations == 0 && self.limits.minutes == 0 {
            out.push("нет ни одного ограничителя".into());
        }
        out
    }

    /// Ветка запуска: `{name}` и `{n}` подставляются.
    pub fn branch_for(&self, run: u32) -> String {
        self.sandbox
            .branch
            .replace("{name}", &slug(&self.name))
            .replace("{n}", &run.to_string())
    }
}

/// Имя цикла в вид, пригодный для ветки git.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in name.chars() {
        if ch.is_alphanumeric() && ch.is_ascii() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    // Кириллица целиком выпадает из ветки: `loop/--3` — не имя. Пусть будет
    // честный запасной вариант, а не мусор.
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() { "loop".into() } else { trimmed }
}

/// Чем закончилась итерация.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    /// Ещё идёт.
    #[default]
    Running,
    /// Гейты зелёные, критик доволен.
    Passed,
    /// Критик вернул на доработку.
    Returned,
    /// Гейт красный.
    GateFailed,
    /// Сорвалась: агент не отработал.
    Failed,
}

/// Результат одного гейта.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GateRun {
    pub name: String,
    pub ok: bool,
    /// Хвост вывода — человеку в экран итерации.
    pub output: String,
}

/// Одна итерация в журнале.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Iteration {
    pub n: u32,
    pub started_at: i64,
    pub ended_at: i64,
    pub verdict: Verdict,
    /// Что агент сделал — своими словами, первой строкой ответа.
    pub summary: String,
    pub gates: Vec<GateRun>,
    /// Комментарий критика, если он возвращал.
    pub critic: String,
    pub tokens: u64,
    pub cost_usd: f64,
    /// Файлы, которых итерация коснулась.
    pub files: Vec<String>,
    /// Попала в выборку — ждёт человеческого взгляда.
    pub sampled: bool,
    /// Человек посмотрел.
    pub reviewed: bool,
}

/// Почему запуск закончился.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    #[default]
    None,
    /// Условие выхода выполнено — цикл сделал работу.
    Exit,
    /// Ограничитель: токены.
    Tokens,
    /// Ограничитель: итерации.
    Iterations,
    /// Ограничитель: время.
    Time,
    /// Цикл ушёл от исходной цели.
    Drift,
    /// Остановлен человеком.
    Stopped,
    /// Сорвался: агент недоступен, репозиторий пропал и подобное.
    Failed,
}

impl StopReason {
    /// Ограничитель — это не поломка: работа цела, продолжение возможно.
    pub fn is_limit(self) -> bool {
        matches!(self, StopReason::Tokens | StopReason::Iterations | StopReason::Time)
    }
}

/// Состояние запуска.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RunState {
    #[default]
    Idle,
    Running,
    /// Цикл упёрся в спорное решение и ждёт человека.
    Asking,
    Stopped,
    Done,
}

/// Вопрос цикла человеку — human by exception.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Ask {
    pub at: i64,
    pub question: String,
    pub options: Vec<String>,
    /// Итерация, на которой цикл встал.
    pub iteration: u32,
}

/// Один запуск цикла.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Run {
    pub loop_id: String,
    /// Порядковый номер запуска — он же попадает в имя ветки.
    pub n: u32,
    pub state: RunState,
    pub started_at: i64,
    pub ended_at: i64,
    pub branch: String,
    pub worktree: String,
    pub iterations: Vec<Iteration>,
    pub stop: StopReason,
    /// Человеческое пояснение к остановке.
    pub stop_note: String,
    pub tokens: u64,
    pub cost_usd: f64,
    /// Незакрытый вопрос цикла.
    pub ask: Option<Ask>,
    /// Реплики человека, которые уйдут в следующую итерацию.
    pub interventions: Vec<String>,
    /// Сколько итераций подряд всё зелёное — по этому и выходим.
    pub streak: u32,
}

impl Run {
    /// Итерации, ждущие человеческого взгляда.
    pub fn pending_review(&self) -> usize {
        self.iterations.iter().filter(|i| i.sampled && !i.reviewed).count()
    }

    /// Сколько минут идёт запуск.
    pub fn minutes(&self, now: i64) -> u32 {
        let end = if self.ended_at > 0 { self.ended_at } else { now };
        ((end - self.started_at).max(0) / 60_000) as u32
    }

    /// Какой ограничитель сработал прямо сейчас, если сработал.
    ///
    /// Проверяется ПЕРЕД началом итерации, а не после: смысл ограничителя в
    /// том, чтобы не начинать работу, на которую нет бюджета.
    pub fn tripped(&self, limits: &Limits, now: i64) -> Option<StopReason> {
        if limits.tokens > 0 && self.tokens >= limits.tokens {
            return Some(StopReason::Tokens);
        }
        if limits.iterations > 0 && self.iterations.len() as u32 >= limits.iterations {
            return Some(StopReason::Iterations);
        }
        if limits.minutes > 0 && self.minutes(now) >= limits.minutes {
            return Some(StopReason::Time);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_survives_cyrillic_and_punctuation() {
        assert_eq!(slug("ночной test-fix"), "test-fix");
        assert_eq!(slug("Site Redesign!"), "site-redesign");
        // Целиком кириллическое имя не даёт мусорной ветки вроде `loop/--3`.
        assert_eq!(slug("утренний триаж"), "loop");
    }

    #[test]
    fn branch_substitutes_name_and_run() {
        let mut l = Loop { name: "ночной test-fix".into(), ..Default::default() };
        l.sandbox.branch = "loop/{name}-{n}".into();
        assert_eq!(l.branch_for(7), "loop/test-fix-7");
    }

    #[test]
    fn loop_without_exit_or_limits_is_rejected() {
        let bare = Loop::default();
        let problems = bare.problems();
        assert!(problems.iter().any(|p| p.contains("условия выхода")));
        assert!(problems.iter().any(|p| p.contains("имени")));

        let mut ok = Loop {
            name: "test-fix".into(),
            source: Source { goal: "чинить флаки".into(), ..Default::default() },
            ..Default::default()
        };
        ok.sandbox.repo = "/repo".into();
        ok.exit.gates.push(Gate { name: "тесты".into(), command: "cargo test".into() });
        assert!(ok.problems().is_empty(), "{:?}", ok.problems());

        // Снять все стены разом нельзя: цикл без них крутится до утра и до
        // исчерпания лимита аккаунта.
        ok.limits = Limits { tokens: 0, iterations: 0, minutes: 0, stop_on_drift: false };
        assert!(ok.problems().iter().any(|p| p.contains("ограничителя")));
    }

    #[test]
    fn limits_trip_before_the_iteration_starts() {
        let limits = Limits { tokens: 200_000, iterations: 20, minutes: 480, stop_on_drift: true };
        let mut run = Run { started_at: 0, tokens: 199_999, ..Default::default() };
        assert_eq!(run.tripped(&limits, 1_000), None);
        run.tokens = 200_000;
        assert_eq!(run.tripped(&limits, 1_000), Some(StopReason::Tokens));

        run.tokens = 0;
        run.iterations = (0..20).map(|n| Iteration { n, ..Default::default() }).collect();
        assert_eq!(run.tripped(&limits, 1_000), Some(StopReason::Iterations));

        run.iterations.clear();
        // 480 минут = 8 часов
        assert_eq!(run.tripped(&limits, 480 * 60_000), Some(StopReason::Time));
    }

    #[test]
    fn zero_limit_means_no_wall() {
        let limits = Limits { tokens: 0, iterations: 5, minutes: 0, stop_on_drift: false };
        let run = Run { tokens: u64::MAX, ..Default::default() };
        assert_eq!(run.tripped(&limits, i64::MAX / 2), None, "нули — это «без ограничения»");
    }

    #[test]
    fn pending_review_counts_only_unseen_samples() {
        let run = Run {
            iterations: vec![
                Iteration { n: 1, sampled: true, reviewed: true, ..Default::default() },
                Iteration { n: 2, sampled: true, reviewed: false, ..Default::default() },
                Iteration { n: 3, sampled: false, reviewed: false, ..Default::default() },
            ],
            ..Default::default()
        };
        assert_eq!(run.pending_review(), 1);
    }

    #[test]
    fn limit_stop_is_not_a_failure() {
        assert!(StopReason::Tokens.is_limit());
        assert!(!StopReason::Failed.is_limit());
        assert!(!StopReason::Exit.is_limit());
    }
}
