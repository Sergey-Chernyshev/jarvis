//! Пайплайн: цикл как граф шагов, а не одна зашитая последовательность.
//!
//! Обычный цикл — это ровно один сценарий: позвать агента → прогнать гейты →
//! спросить критика → повторить. Он покрывает «чини, пока не позеленеет», но
//! как только работа состоит из разных шагов («сначала разведка планом, потом
//! правка, потом тесты, а если тесты красные — вернуться к правке с их
//! выводом»), зашитая последовательность становится смирительной рубашкой.
//!
//! Идея взята у Camunda и урезана до того, что имеет смысл здесь:
//!
//! * ШАГИ вместо одной итерации: агент, команда, ревьюер, вопрос человеку,
//!   пауза;
//! * ПЕРЕХОДЫ с условиями вместо зашитого «дальше по списку»: развилка выбирает
//!   первый подходящий переход (у Camunda это exclusive gateway);
//! * ПЕРЕМЕННЫЕ: результат шага виден следующим (`${шаг.вывод}`, `${шаг.код}`) —
//!   без этого «отдай тесты правщику» не выразить;
//! * ИНЦИДЕНТЫ: шаг исчерпал попытки — прогон встаёт и ждёт человека, а не
//!   молча идёт дальше.
//!
//! Чего здесь СОЗНАТЕЛЬНО нет — параллельных ветвей. У Camunda они естественны,
//! потому что задачи независимы; здесь два агента в одном рабочем дереве
//! наступят друг другу на файлы. Для параллельной работы в Jarvis есть
//! «Связка»: там у каждой руки свой worktree и очередь слияний. Пайплайн —
//! про порядок и ветвление, связка — про параллель.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Что делает шаг.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StepKind {
    /// Позвать агента с этим промтом.
    #[serde(rename_all = "camelCase")]
    Agent {
        prompt: String,
        #[serde(default)]
        model: String,
    },
    /// Выполнить команду (сборка, тесты, линт).
    #[serde(rename_all = "camelCase")]
    Shell { command: String },
    /// Позвать агента ревьюером: он отвечает вердиктом первой строкой.
    #[serde(rename_all = "camelCase")]
    Review {
        #[serde(default)]
        prompt: String,
        #[serde(default)]
        model: String,
    },
    /// Спросить человека и ждать ответа.
    #[serde(rename_all = "camelCase")]
    Human { question: String },
    /// Подождать. Полезно, когда шаг ждёт чужой сборки или деплоя.
    #[serde(rename_all = "camelCase")]
    Wait { minutes: u32 },
}

impl StepKind {
    pub fn word(&self) -> &'static str {
        match self {
            StepKind::Agent { .. } => "агент",
            StepKind::Shell { .. } => "команда",
            StepKind::Review { .. } => "ревью",
            StepKind::Human { .. } => "человек",
            StepKind::Wait { .. } => "пауза",
        }
    }
}

/// Когда переход срабатывает.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cond", rename_all = "camelCase")]
pub enum Cond {
    /// Всегда. Последний переход обычно такой — это «иначе».
    Always,
    /// Шаг отработал успешно: нулевой код у команды, ответ у агента, «OK» у
    /// ревьюера.
    Ok,
    /// Шаг не удался.
    Fail,
    /// Ревьюер сказал именно это: `ok` | `return` | `ask`.
    #[serde(rename_all = "camelCase")]
    Verdict { verdict: String },
    /// В выводе шага встретилось это (без регулярок: их пишут с ошибками, а
    /// молча не сработавшее условие — худший вид поломки).
    #[serde(rename_all = "camelCase")]
    Contains { text: String },
}

/// Переход к следующему шагу.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flow {
    /// Куда. Пусто — конец пайплайна (успешный).
    #[serde(default)]
    pub to: String,
    #[serde(flatten)]
    pub when: Cond,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub kind: StepKind,
    /// Сколько раз повторить шаг, прежде чем звать человека. Как в Camunda:
    /// сеть моргнула — не повод будить.
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub next: Vec<Flow>,
}

impl Step {
    pub fn title(&self) -> String {
        if self.name.trim().is_empty() {
            self.id.clone()
        } else {
            self.name.clone()
        }
    }
}

/// Пайплайн целиком.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Pipeline {
    /// С какого шага начинать. Пусто — первый в списке.
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

impl Pipeline {
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    pub fn first(&self) -> Option<&Step> {
        if self.start.trim().is_empty() {
            self.steps.first()
        } else {
            self.step(&self.start)
        }
    }

    /// Чего не хватает, чтобы пайплайн можно было запустить.
    ///
    /// Списком, а не первой ошибкой: конструктор показывает все дыры разом.
    /// Проверяем ровно то, что убивает прогон молча: некуда идти, ссылка в
    /// никуда, недостижимый шаг, шаг без содержания.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.steps.is_empty() {
            out.push("в пайплайне нет ни одного шага".into());
            return out;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for s in &self.steps {
            if s.id.trim().is_empty() {
                out.push("у шага нет идентификатора".into());
            } else if !seen.insert(s.id.as_str()) {
                out.push(format!("шаг «{}» объявлен дважды", s.id));
            }
            match &s.kind {
                StepKind::Agent { prompt, .. } if prompt.trim().is_empty() => {
                    out.push(format!("{}: агенту не сказано, что делать", s.title()));
                }
                StepKind::Shell { command } if command.trim().is_empty() => {
                    out.push(format!("{}: пустая команда", s.title()));
                }
                StepKind::Human { question } if question.trim().is_empty() => {
                    out.push(format!("{}: не задан вопрос человеку", s.title()));
                }
                _ => {}
            }
            for f in &s.next {
                if !f.to.trim().is_empty() && self.step(&f.to).is_none() {
                    out.push(format!("{}: переход в несуществующий шаг «{}»", s.title(), f.to));
                }
            }
        }
        if self.first().is_none() {
            out.push(format!("стартовый шаг «{}» не найден", self.start));
        }
        // Недостижимые шаги — не ошибка исполнения, но почти всегда ошибка
        // сборки: человек думает, что шаг работает, а до него не доходит ход.
        for id in self.unreachable() {
            out.push(format!("до шага «{id}» никогда не дойдёт очередь"));
        }
        out
    }

    /// Шаги, до которых не ведёт ни один путь от старта.
    pub fn unreachable(&self) -> Vec<String> {
        let Some(start) = self.first() else {
            return Vec::new();
        };
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack = vec![start.id.as_str()];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(s) = self.step(id) {
                for f in &s.next {
                    if !f.to.trim().is_empty() {
                        stack.push(f.to.as_str());
                    }
                }
            }
        }
        self.steps
            .iter()
            .filter(|s| !seen.contains(s.id.as_str()))
            .map(|s| s.id.clone())
            .collect()
    }
}

/// Чем закончился шаг — вход для условий переходов.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// Успех: нулевой код, ответ агента, «OK» ревьюера.
    pub ok: bool,
    /// Что шаг сказал: вывод команды, ответ агента, текст ревью.
    #[serde(default)]
    pub output: String,
    /// Код возврата команды; у остальных шагов ноль.
    #[serde(default)]
    pub code: i32,
    /// Вердикт ревьюера: `ok` | `return` | `ask`.
    #[serde(default)]
    pub verdict: String,
}

/// Куда идти после шага. `None` — конец пайплайна.
///
/// Первый подходящий переход и выигрывает: порядок задаёт человек, и «сработали
/// оба» здесь означало бы недетерминированный прогон.
pub fn next_step(step: &Step, out: &Outcome) -> Option<String> {
    for f in &step.next {
        if matches(&f.when, out) {
            return if f.to.trim().is_empty() {
                None
            } else {
                Some(f.to.clone())
            };
        }
    }
    None
}

pub fn matches(cond: &Cond, out: &Outcome) -> bool {
    match cond {
        Cond::Always => true,
        Cond::Ok => out.ok,
        Cond::Fail => !out.ok,
        Cond::Verdict { verdict } => out.verdict.eq_ignore_ascii_case(verdict.trim()),
        Cond::Contains { text } => {
            let t = text.trim();
            !t.is_empty() && out.output.to_lowercase().contains(&t.to_lowercase())
        }
    }
}

/// Подстановка переменных: `${шаг.вывод}`, `${шаг.код}`, `${шаг.вердикт}`.
///
/// Без этого пайплайн — просто список: «отдай правщику вывод тестов» выразить
/// нечем. Неизвестная переменная остаётся текстом КАК ЕСТЬ: молча подставленная
/// пустота — это промт, в котором半 задачи испарилось, и понять это по ответу
/// агента невозможно.
pub fn interpolate(text: &str, vars: &HashMap<String, Outcome>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 2..];
        let Some(end) = tail.find('}') else {
            out.push_str(&rest[at..]);
            return out;
        };
        let expr = &tail[..end];
        match value_of(expr, vars) {
            Some(v) => out.push_str(&v),
            None => {
                out.push_str("${");
                out.push_str(expr);
                out.push('}');
            }
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

fn value_of(expr: &str, vars: &HashMap<String, Outcome>) -> Option<String> {
    let (id, field) = expr.trim().split_once('.')?;
    let o = vars.get(id.trim())?;
    Some(match field.trim() {
        "вывод" | "output" => o.output.clone(),
        "код" | "code" => o.code.to_string(),
        "вердикт" | "verdict" => o.verdict.clone(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, next: Vec<Flow>) -> Step {
        Step {
            id: id.into(),
            name: String::new(),
            kind: StepKind::Agent {
                prompt: "делай".into(),
                model: String::new(),
            },
            retries: 0,
            next,
        }
    }

    fn flow(to: &str, when: Cond) -> Flow {
        Flow {
            to: to.into(),
            when,
        }
    }

    #[test]
    fn the_first_matching_flow_wins() {
        let s = agent(
            "a",
            vec![
                flow("чинить", Cond::Fail),
                flow("дальше", Cond::Always),
            ],
        );
        let bad = Outcome { ok: false, ..Default::default() };
        let good = Outcome { ok: true, ..Default::default() };
        assert_eq!(next_step(&s, &bad).as_deref(), Some("чинить"));
        assert_eq!(next_step(&s, &good).as_deref(), Some("дальше"));
    }

    /// Пустой «куда» — это конец пайплайна, а не ошибка: так рисуют выход.
    #[test]
    fn an_empty_target_ends_the_pipeline() {
        let s = agent("a", vec![flow("", Cond::Always)]);
        assert_eq!(next_step(&s, &Outcome::default()), None);
        // И шаг вовсе без переходов — тоже конец.
        assert_eq!(next_step(&agent("a", vec![]), &Outcome::default()), None);
    }

    #[test]
    fn conditions_read_the_outcome() {
        let out = Outcome {
            ok: false,
            output: "FAILED: 3 tests".into(),
            code: 1,
            verdict: "return".into(),
        };
        assert!(matches(&Cond::Fail, &out));
        assert!(!matches(&Cond::Ok, &out));
        assert!(matches(&Cond::Verdict { verdict: "RETURN".into() }, &out), "вердикт без регистра");
        assert!(matches(&Cond::Contains { text: "failed".into() }, &out), "поиск без регистра");
        assert!(!matches(&Cond::Contains { text: "passed".into() }, &out));
        // Пустой текст не должен совпадать со всем подряд.
        assert!(!matches(&Cond::Contains { text: "  ".into() }, &out));
    }

    #[test]
    fn variables_carry_the_previous_step_into_the_next_prompt() {
        let mut vars = HashMap::new();
        vars.insert(
            "тесты".to_string(),
            Outcome { ok: false, output: "3 упали".into(), code: 1, verdict: String::new() },
        );
        let t = interpolate("почини: ${тесты.вывод} (код ${тесты.код})", &vars);
        assert_eq!(t, "почини: 3 упали (код 1)");
    }

    /// Неизвестная переменная остаётся текстом: подставленная пустота — это
    /// промт, из которого половина задачи испарилась незаметно.
    #[test]
    fn an_unknown_variable_stays_visible() {
        let vars = HashMap::new();
        assert_eq!(interpolate("дай ${нет.вывод}", &vars), "дай ${нет.вывод}");
        assert_eq!(interpolate("${", &vars), "${", "оборванная скобка не съедает текст");
        assert_eq!(interpolate("без переменных", &vars), "без переменных");
    }

    #[test]
    fn problems_name_every_hole_at_once() {
        let p = Pipeline {
            start: "нет".into(),
            steps: vec![
                Step {
                    id: "a".into(),
                    name: "правка".into(),
                    kind: StepKind::Shell { command: "  ".into() },
                    retries: 0,
                    next: vec![flow("призрак", Cond::Always)],
                },
                agent("a", vec![]),
            ],
        };
        let problems = p.problems().join(" | ");
        assert!(problems.contains("дважды"), "{problems}");
        assert!(problems.contains("пустая команда"), "{problems}");
        assert!(problems.contains("призрак"), "{problems}");
        assert!(problems.contains("стартовый шаг"), "{problems}");
    }

    /// Шаг, до которого не ведёт ни один путь, — почти всегда ошибка сборки:
    /// человек думает, что он работает, а очередь до него не доходит.
    #[test]
    fn unreachable_steps_are_reported() {
        let p = Pipeline {
            start: String::new(),
            steps: vec![
                agent("старт", vec![flow("", Cond::Always)]),
                agent("забытый", vec![]),
            ],
        };
        assert_eq!(p.unreachable(), vec!["забытый".to_string()]);
        assert!(p.problems().iter().any(|x| x.contains("забытый")));
    }

    /// Цикл в графе — это нормально: «тесты красные → назад к правке» и есть
    /// петля. Проверка достижимости не должна на ней зависать.
    #[test]
    fn a_loop_in_the_graph_is_fine() {
        let p = Pipeline {
            start: "правка".into(),
            steps: vec![
                agent("правка", vec![flow("тесты", Cond::Always)]),
                Step {
                    id: "тесты".into(),
                    name: String::new(),
                    kind: StepKind::Shell { command: "cargo test".into() },
                    retries: 0,
                    next: vec![flow("правка", Cond::Fail), flow("", Cond::Always)],
                },
            ],
        };
        assert!(p.problems().is_empty(), "{:?}", p.problems());
        assert!(p.unreachable().is_empty());
    }

    /// Формат на диске общий с панелью: шаг обязан читаться и писаться так,
    /// как его положит конструктор.
    #[test]
    fn the_wire_format_is_flat_and_readable() {
        let s = Step {
            id: "тесты".into(),
            name: "прогнать тесты".into(),
            kind: StepKind::Shell { command: "cargo test".into() },
            retries: 2,
            next: vec![flow("правка", Cond::Fail)],
        };
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains(r#""kind":"shell""#), "{text}");
        assert!(text.contains(r#""command":"cargo test""#), "{text}");
        assert!(text.contains(r#""cond":"fail""#), "{text}");
        let back: Step = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
    }
}
