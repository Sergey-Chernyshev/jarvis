//! Пайплайн: цикл как граф узлов, а не одна зашитая последовательность.
//!
//! Обычный цикл — это ровно один сценарий: позвать агента → прогнать гейты →
//! спросить критика → повторить. Он покрывает «чини, пока не позеленеет», но
//! как только работа состоит из разных шагов («сначала разведка планом, потом
//! правка, потом тесты, а если тесты красные — вернуться к правке с их
//! выводом»), зашитая последовательность становится смирительной рубашкой.
//!
//! Взято у BPMN и у Camunda — и взято целиком, а не наполовину:
//!
//! * ЗАДАЧИ вместо одной итерации: агент, команда, ревьюер, вопрос человеку,
//!   пауза;
//! * ПЕРЕХОДЫ с условиями вместо зашитого «дальше по списку»;
//! * РАЗВИЛКА (`Choice`, exclusive gateway): первый подходящий переход и
//!   выигрывает;
//! * ВЕТВЛЕНИЕ и СЛИЯНИЕ (`Fork`/`Join`, parallel gateway): ветки идут
//!   одновременно — **каждая в своём git worktree**, а слияние сводит их
//!   обратно. Без разных рабочих деревьев параллель здесь была бы ложью: два
//!   агента в одном каталоге затирают друг другу файлы;
//! * ПЕРЕМЕННЫЕ: результат узла виден следующим (`${узел.вывод}`, `${узел.код}`);
//! * ИНЦИДЕНТЫ: узел исчерпал попытки — прогон встаёт и ждёт человека, а не
//!   молча идёт дальше.
//!
//! Формат на диске — свой (JSON рядом с циклом), но он ОДНОЗНАЧНО отображается
//! в BPMN 2.0 и обратно: см. [`super::bpmn`]. Именно поэтому у узла есть `x`/`y`
//! — расстановку, сделанную человеком в Camunda Modeler, стирать нельзя.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Что делает узел.
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
    /// Подождать. Полезно, когда узел ждёт чужой сборки или деплоя.
    #[serde(rename_all = "camelCase")]
    Wait { minutes: u32 },
    /// Развилка: уходит РОВНО ОДИН переход — первый, чьё условие сошлось.
    ///
    /// Отдельный узел, а не условия на переходах задачи (так тоже можно и так
    /// работают старые пайплайны), потому что в Camunda развилку рисуют
    /// ромбом. Пайплайн, открытый в модельере, должен выглядеть как то, чем он
    /// является.
    Choice,
    /// Ветвление: уходят ВСЕ переходы разом, каждый в своём рабочем дереве.
    Fork,
    /// Слияние: ждём все входящие ветки и сводим их worktree'ы в один.
    ///
    /// Правило конфликта живёт не здесь, а на узле (`Step::on_conflict`): с
    /// появлением «для каждого» сливать ветки стало можно двумя способами, и
    /// два поля с одним смыслом однажды разошлись бы.
    Join,
}

impl StepKind {
    pub fn word(&self) -> &'static str {
        match self {
            StepKind::Agent { .. } => "агент",
            StepKind::Shell { .. } => "команда",
            StepKind::Review { .. } => "ревью",
            StepKind::Human { .. } => "человек",
            StepKind::Wait { .. } => "пауза",
            StepKind::Choice => "развилка",
            StepKind::Fork => "ветвление",
            StepKind::Join => "слияние",
        }
    }

    /// Шлюз — узел, который ничего не делает, а только направляет ход.
    /// Он не занимает итерацию журнала и не тратит токенов.
    pub fn is_gateway(&self) -> bool {
        matches!(self, StepKind::Choice | StepKind::Fork | StepKind::Join)
    }
}

/// Как разрешать конфликт слияния веток.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnConflict {
    /// Остановить прогон и позвать человека. Умолчание: молча выбрать одну из
    /// сторон — значит потерять чью-то ночь работы без единого слова.
    Stop,
    /// Взять свою сторону (ту ветку, в которую вливаем).
    Ours,
    /// Взять чужую (ту, которую вливаем).
    Theirs,
    /// Отдать конфликт агенту: он видел контекст правок, мы — нет.
    Agent,
}

impl OnConflict {
    pub fn parse(s: &str) -> OnConflict {
        match s.trim() {
            "ours" | "своя" => OnConflict::Ours,
            "theirs" | "чужая" => OnConflict::Theirs,
            "agent" | "агент" => OnConflict::Agent,
            _ => OnConflict::Stop,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            OnConflict::Stop => "остановиться",
            OnConflict::Ours => "оставить свою",
            OnConflict::Theirs => "взять чужую",
            OnConflict::Agent => "отдать агенту",
        }
    }
}

/// Когда переход срабатывает.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cond", rename_all = "camelCase")]
pub enum Cond {
    /// Всегда. Последний переход обычно такой — это «иначе».
    Always,
    /// Узел отработал успешно: нулевой код у команды, ответ у агента, «OK» у
    /// ревьюера.
    Ok,
    /// Узел не удался.
    Fail,
    /// Ревьюер сказал именно это: `ok` | `return` | `ask`.
    #[serde(rename_all = "camelCase")]
    Verdict { verdict: String },
    /// В выводе узла встретилось это (без регулярок: их пишут с ошибками, а
    /// молча не сработавшее условие — худший вид поломки).
    #[serde(rename_all = "camelCase")]
    Contains { text: String },
    /// Выражение, набранное в Camunda Modeler и не уложившееся в кнопки
    /// конструктора. Хранится ТЕКСТОМ и таким же возвращается в файл: чужую
    /// правку нельзя молча упрощать до ближайшей знакомой.
    #[serde(rename_all = "camelCase")]
    Expr { text: String },
}

/// Переход к следующему узлу.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flow {
    /// Куда. Пусто — конец пайплайна (успешный).
    #[serde(default)]
    pub to: String,
    /// Подпись стрелки. В Camunda её пишут на переходах из развилки, и терять
    /// её при обратном чтении нельзя.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Идентификатор перехода в BPMN. Пусто — сгенерируем при выгрузке.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bpmn_id: String,
    #[serde(flatten)]
    pub when: Cond,
}

impl Flow {
    pub fn to(to: &str, when: Cond) -> Flow {
        Flow { to: to.into(), label: String::new(), bpmn_id: String::new(), when }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub kind: StepKind,
    /// Сколько раз повторить узел, прежде чем звать человека. Как в Camunda:
    /// сеть моргнула — не повод будить.
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub next: Vec<Flow>,
    /// «Для каждого»: выражение, дающее СПИСОК (по элементу на строку).
    ///
    /// Узел с этим полем выполняется столько раз, сколько строк в списке, и
    /// каждый раз — В СВОЕЙ ВЕТКЕ, со своим рабочим деревом. В самом шаге
    /// элемент виден как `${элемент}`. Это multi-instance из BPMN, и нужен он
    /// ровно там, где список заранее неизвестен: «для каждого упавшего теста»,
    /// «для каждого файла из вывода».
    ///
    /// Ветвление (`Fork`) для этого не годится: у него ветки нарисованы
    /// заранее, а тут их столько, сколько окажется строк.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub over: String,
    /// Что делать с конфликтом слияния веток: `stop` | `ours` | `theirs` |
    /// `agent`. Читается у слияния и у узла «для каждого» — сливают оба.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub on_conflict: String,
    /// Где узел стоит на полотне. Ставит человек в Camunda Modeler — и наша
    /// автораскладка не имеет права затирать его работу.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub x: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub y: i32,
}

fn is_zero(v: &i32) -> bool {
    *v == 0
}

impl Step {
    /// Узел с работой и переходами — то, из чего собирают пайплайн в коде.
    ///
    /// Конструктор, а не литерал со всеми полями: полей у узла стало восемь, и
    /// каждое новое иначе означало бы правку двух десятков мест, ни одно из
    /// которых про него не знает.
    pub fn node(id: &str, kind: StepKind, next: Vec<Flow>) -> Step {
        Step {
            id: id.into(),
            name: id.into(),
            kind,
            retries: 0,
            next,
            over: String::new(),
            on_conflict: String::new(),
            x: 0,
            y: 0,
        }
    }

    /// Узел размножается по списку.
    pub fn is_each(&self) -> bool {
        !self.over.trim().is_empty() && !self.kind.is_gateway()
    }

    pub fn title(&self) -> String {
        if self.name.trim().is_empty() {
            self.id.clone()
        } else {
            self.name.clone()
        }
    }

    /// Узел не сделан до конца — вернуть текст претензии.
    fn hole(&self) -> Option<String> {
        let empty = |s: &String| s.trim().is_empty();
        match &self.kind {
            StepKind::Agent { prompt, .. } if empty(prompt) => {
                Some(format!("{}: агенту не сказано, что делать", self.title()))
            }
            StepKind::Shell { command } if empty(command) => {
                Some(format!("{}: пустая команда", self.title()))
            }
            StepKind::Human { question } if empty(question) => {
                Some(format!("{}: не задан вопрос человеку", self.title()))
            }
            _ => None,
        }
    }
}

/// Пайплайн целиком.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Pipeline {
    /// С какого узла начинать. Пусто — первый в списке.
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Файл `.bpmn`, с которым пайплайн держат синхронным. Пусто — связи нет.
    ///
    /// Живёт в модели, а не в настройках цикла, потому что это свойство именно
    /// ЭТОГО графа: выгрузили, поправили в Camunda, забрали обратно.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bpmn_file: String,
    /// Время правки файла, которое мы уже забрали. По нему видно, что человек
    /// сохранил в модельере что-то новое.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub bpmn_mtime: i64,
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
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

    /// Кто ведёт в этот узел. Слиянию это и есть «сколько веток ждать».
    pub fn incoming(&self, id: &str) -> Vec<&Step> {
        self.steps
            .iter()
            .filter(|s| s.next.iter().any(|f| f.to == id))
            .collect()
    }

    /// Сколько веток должно прийти в слияние, прежде чем оно пропустит ход.
    ///
    /// Считаем ПЕРЕХОДЫ, а не узлы: две стрелки из одной развилки в одно
    /// слияние — это две ветки, и ждать надо обе.
    pub fn join_arity(&self, id: &str) -> usize {
        self.steps
            .iter()
            .map(|s| s.next.iter().filter(|f| f.to == id).count())
            .sum()
    }

    /// Чего не хватает, чтобы пайплайн можно было запустить.
    ///
    /// Списком, а не первой ошибкой: конструктор показывает все дыры разом.
    /// Проверяем ровно то, что убивает прогон молча: некуда идти, ссылка в
    /// никуда, недостижимый узел, узел без содержания, ветвление без слияния.
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
            if let Some(hole) = s.hole() {
                out.push(hole);
            }
            if !s.on_conflict.trim().is_empty()
                && !["stop", "ours", "theirs", "agent"].contains(&s.on_conflict.trim())
            {
                out.push(format!("{}: неизвестное решение конфликта «{}»", s.title(), s.on_conflict));
            }
            // «Для каждого» на шлюзе бессмысленно: шлюз ничего не выполняет, и
            // размножать там нечего.
            if !s.over.trim().is_empty() && s.kind.is_gateway() {
                out.push(format!("{}: «для каждого» бывает только у шага с работой", s.title()));
            }
            // «Для каждого» имеет смысл там, где есть что делать по разу на
            // элемент. У вопроса человеку и у паузы работы нет: первый всё
            // равно замораживает прогон целиком, вторая просто спит.
            if s.is_each()
                && matches!(s.kind, StepKind::Human { .. } | StepKind::Wait { .. })
            {
                out.push(format!(
                    "{}: «для каждого» бывает у агента, команды и ревью — размножать {} нечего",
                    s.title(),
                    s.kind.word()
                ));
            }
            if s.is_each() && !s.over.contains("${") {
                out.push(format!(
                    "{}: «для каждого» ждёт выражение со списком — например ${{тесты.вывод}}",
                    s.title()
                ));
            }
            for f in &s.next {
                if !f.to.trim().is_empty() && self.step(&f.to).is_none() {
                    out.push(format!("{}: переход в несуществующий шаг «{}»", s.title(), f.to));
                }
                if let Cond::Expr { text } = &f.when {
                    if parse_expr(text).is_none() {
                        out.push(format!(
                            "{}: условие «{}» не разобрано — переход не сработает никогда",
                            s.title(),
                            crate::util::ellipsize(text.trim(), 40)
                        ));
                    }
                }
            }
            out.extend(self.gateway_problems(s));
        }
        if self.first().is_none() {
            out.push(format!("стартовый шаг «{}» не найден", self.start));
        }
        // Недостижимые узлы — не ошибка исполнения, но почти всегда ошибка
        // сборки: человек думает, что узел работает, а до него не доходит ход.
        for id in self.unreachable() {
            out.push(format!("до шага «{id}» никогда не дойдёт очередь"));
        }
        out
    }

    /// Претензии, которые бывают только у шлюзов.
    fn gateway_problems(&self, s: &Step) -> Vec<String> {
        let mut out = Vec::new();
        let outs = s.next.iter().filter(|f| !f.to.trim().is_empty()).count();
        match &s.kind {
            StepKind::Fork => {
                if outs < 2 {
                    out.push(format!("{}: ветвление без ветвей — веток должно быть хотя бы две", s.title()));
                }
                // Ветвление без слияния значит, что ветки просто разбегутся:
                // их worktree'ы останутся не влитыми, и работа ночи пропадёт.
                if self.join_after(&s.id).is_none() {
                    out.push(format!("{}: ветви никуда не сходятся — добавь слияние", s.title()));
                }
            }
            StepKind::Join => {
                if self.join_arity(&s.id) < 2 {
                    let from = self
                        .incoming(&s.id)
                        .iter()
                        .map(|x| x.title())
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push(match from.is_empty() {
                        true => format!("{}: в слияние не ведёт ни одна ветка", s.title()),
                        false => format!("{}: в слияние ведёт только «{from}» — сливать не с чем", s.title()),
                    });
                }
            }
            StepKind::Choice if outs == 0 => {
                out.push(format!("{}: развилка никуда не ведёт", s.title()));
            }
            _ => {}
        }
        out
    }

    /// Первое слияние, достижимое из ветвления. `None` — ветви расходятся
    /// навсегда.
    pub fn join_after(&self, fork: &str) -> Option<String> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut queue: Vec<&str> = self
            .step(fork)?
            .next
            .iter()
            .filter(|f| !f.to.trim().is_empty())
            .map(|f| f.to.as_str())
            .collect();
        while let Some(id) = queue.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(s) = self.step(id) else { continue };
            if matches!(s.kind, StepKind::Join) {
                return Some(s.id.clone());
            }
            queue.extend(s.next.iter().filter(|f| !f.to.trim().is_empty()).map(|f| f.to.as_str()));
        }
        None
    }

    /// Узлы, до которых не ведёт ни один путь от старта.
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

    /// Есть ли в пайплайне параллель. От этого зависит, нужны ли прогону
    /// отдельные рабочие деревья.
    pub fn has_parallel(&self) -> bool {
        self.steps.iter().any(|s| matches!(s.kind, StepKind::Fork) || s.is_each())
    }
}

/// Чем закончился узел — вход для условий переходов.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// Успех: нулевой код, ответ агента, «OK» ревьюера.
    pub ok: bool,
    /// Что узел сказал: вывод команды, ответ агента, текст ревью.
    #[serde(default)]
    pub output: String,
    /// Код возврата команды; у остальных узлов ноль.
    #[serde(default)]
    pub code: i32,
    /// Вердикт ревьюера: `ok` | `return` | `ask`.
    #[serde(default)]
    pub verdict: String,
}

/// Куда идти после узла. `None` — конец пайплайна.
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

/// Все переходы ветвления. В отличие от развилки условия здесь не спрашивают:
/// смысл `Fork` в том, что уходят ВСЕ ветки сразу.
pub fn all_steps(step: &Step) -> Vec<String> {
    step.next
        .iter()
        .filter(|f| !f.to.trim().is_empty())
        .map(|f| f.to.clone())
        .collect()
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
        // Неразобранное выражение НЕ срабатывает: «на всякий случай пропустим»
        // означало бы ход по стрелке, смысла которой мы не поняли.
        Cond::Expr { text } => parse_expr(text).map(|c| matches(&c, out)).unwrap_or(false),
    }
}

/* ======================= выражения переходов ======================= */

/// Разобрать выражение перехода в условие.
///
/// Язык нарочно крошечный: его пишут не программисты и не в редакторе с
/// подсветкой, а в поле свойств Camunda Modeler. Всё, что сюда не уложилось,
/// остаётся текстом и честно объявляется непонятым — молча не сработавшее
/// условие есть худший вид поломки.
///
/// Понимаем: `${ok}`, `${fail}`, `${verdict == 'return'}`, `${code != 0}`,
/// `${contains(output, 'FAILED')}`, `${true}`.
pub fn parse_expr(raw: &str) -> Option<Cond> {
    let mut t = raw.trim();
    if let Some(inner) = t.strip_prefix("${") {
        t = inner.strip_suffix('}')?.trim();
    }
    let low = t.to_lowercase();
    match low.as_str() {
        "" | "true" | "always" => return Some(Cond::Always),
        "ok" | "success" => return Some(Cond::Ok),
        "fail" | "failed" | "!ok" | "not ok" => return Some(Cond::Fail),
        _ => {}
    }
    if let Some(args) = low.strip_prefix("contains(").and_then(|s| s.strip_suffix(')')) {
        // contains('текст') и contains(output, 'текст') — обе формы живые:
        // первую пишем мы, вторую человек, знакомый с EL.
        let arg = args.rsplit(',').next().unwrap_or(args);
        let text = unquote(arg.trim(), t)?;
        return Some(Cond::Contains { text });
    }
    if let Some((lhs, rhs)) = split_cmp(t) {
        let (op, rhs) = rhs;
        let field = lhs.trim().to_lowercase();
        let field = field.trim_start_matches("execution.").trim();
        let value = unquote(rhs.trim(), t)?;
        return match (field, op) {
            ("verdict", "==") => Some(Cond::Verdict { verdict: value }),
            ("code", "==") if value == "0" => Some(Cond::Ok),
            ("code", "!=") if value == "0" => Some(Cond::Fail),
            _ => None,
        };
    }
    None
}

/// Снять кавычки со строкового литерала. Число возвращаем как есть — сравнения
/// с кодом пишут без кавычек.
fn unquote(s: &str, whole: &str) -> Option<String> {
    let s = s.trim();
    for q in ['\'', '"'] {
        if let Some(inner) = s.strip_prefix(q).and_then(|x| x.strip_suffix(q)) {
            // Регистр литерала берём из ИСХОДНОГО текста: выражение мы
            // приводили к нижнему, а «FAILED» в поиске должно остаться собой.
            return Some(original_case(inner, whole));
        }
    }
    s.chars().all(|c| c.is_ascii_digit()).then(|| s.to_string())
}

/// Найти в исходной строке кусок, совпавший с приведённым к нижнему регистру.
fn original_case(lowered: &str, whole: &str) -> String {
    let hay = whole.to_lowercase();
    match hay.find(lowered) {
        Some(at) if whole.is_char_boundary(at) && whole.is_char_boundary(at + lowered.len()) => {
            whole[at..at + lowered.len()].to_string()
        }
        _ => lowered.to_string(),
    }
}

/// Разбить `a == b` / `a != b`. Возвращает (левое, (оператор, правое)).
fn split_cmp(t: &str) -> Option<(&str, (&'static str, &str))> {
    for op in ["==", "!="] {
        if let Some((l, r)) = t.split_once(op) {
            return Some((l, (if op == "==" { "==" } else { "!=" }, r)));
        }
    }
    None
}

/// Условие обратно в выражение — то, что уедет в файл `.bpmn`.
pub fn expr_of(cond: &Cond) -> String {
    match cond {
        Cond::Always => String::new(), // безусловный переход выражения не несёт
        Cond::Ok => "${ok}".into(),
        Cond::Fail => "${fail}".into(),
        Cond::Verdict { verdict } => format!("${{verdict == '{}'}}", verdict.trim()),
        Cond::Contains { text } => format!("${{contains(output, '{}')}}", escape_quotes(text.trim())),
        Cond::Expr { text } => text.clone(),
    }
}

fn escape_quotes(s: &str) -> String {
    s.replace('\'', "\\'")
}

/// Подстановка переменных: `${шаг.вывод}`, `${шаг.код}`, `${шаг.вердикт}`.
///
/// Без этого пайплайн — просто список: «отдай правщику вывод тестов» выразить
/// нечем. Неизвестная переменная остаётся текстом КАК ЕСТЬ: молча подставленная
/// пустота — это промт, в котором половина задачи испарилась, и понять это по
/// ответу агента невозможно.
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
    let expr = expr.trim();
    let Some((id, field)) = expr.split_once('.') else {
        // Без точки — сам вывод: `${элемент}` у шага «для каждого» и `${тесты}`
        // как короткая запись `${тесты.вывод}`. Требовать точку там, где поле
        // одно, значит заставлять писать лишнее и ошибаться в нём.
        return vars.get(expr).map(|o| o.output.clone());
    };
    let o = vars.get(id.trim())?;
    Some(match field.trim() {
        "вывод" | "output" => o.output.clone(),
        "код" | "code" => o.code.to_string(),
        "вердикт" | "verdict" => o.verdict.clone(),
        _ => return None,
    })
}

/// Список, по которому размножается узел «для каждого».
///
/// Строка на элемент — потому что так печатают всё: `ls`, `grep`, вывод тестов,
/// ответ агента списком. Пустые строки и повторы выкидываем: два одинаковых
/// элемента дали бы две ветки, правящие одно и то же, и гарантированный
/// конфликт на слиянии.
pub fn items_of(text: &str, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim().trim_start_matches(['-', '*', '•']).trim();
        if t.is_empty() || out.iter().any(|x| x == t) {
            continue;
        }
        out.push(t.to_string());
        if out.len() >= limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, next: Vec<Flow>) -> Step {
        Step::node(id, StepKind::Agent { prompt: "делай".into(), model: String::new() }, next)
    }

    fn gate(id: &str, kind: StepKind, next: Vec<Flow>) -> Step {
        Step::node(id, kind, next)
    }

    fn flow(to: &str, when: Cond) -> Flow {
        Flow::to(to, when)
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
        // И узел вовсе без переходов — тоже конец.
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

    /// Ветвление уводит ВСЕ ветки разом — условий у него не спрашивают.
    #[test]
    fn a_fork_takes_every_branch_at_once() {
        let f = gate(
            "разойтись",
            StepKind::Fork,
            vec![flow("фронт", Cond::Always), flow("бэк", Cond::Always), flow("", Cond::Always)],
        );
        assert_eq!(all_steps(&f), vec!["фронт".to_string(), "бэк".to_string()]);
    }

    /// Список «для каждого»: строка на элемент, без пустых и без повторов —
    /// два одинаковых элемента дали бы две ветки, правящие одно и то же.
    #[test]
    fn a_list_is_one_item_per_line_without_repeats() {
        let text = "  tests::a\n\n- tests::b\n* tests::a\n  \n• tests::c\n";
        assert_eq!(items_of(text, 10), ["tests::a", "tests::b", "tests::c"]);
        assert_eq!(items_of(text, 2).len(), 2, "потолок соблюдается");
        assert!(items_of("   \n\n", 10).is_empty());
    }

    /// `${элемент}` без точки — это его вывод. Требовать точку там, где поле
    /// одно, значит заставлять писать лишнее и ошибаться в нём.
    #[test]
    fn a_bare_variable_is_its_output() {
        let mut vars = HashMap::new();
        vars.insert("элемент".to_string(), Outcome { output: "tests::flaky".into(), ..Default::default() });
        assert_eq!(interpolate("почини ${элемент}", &vars), "почини tests::flaky");
        assert_eq!(interpolate("почини ${элемент.вывод}", &vars), "почини tests::flaky");
        assert_eq!(interpolate("${нет}", &vars), "${нет}", "неизвестная остаётся текстом");
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
                gate("a", StepKind::Shell { command: "  ".into() }, vec![flow("призрак", Cond::Always)]),
                agent("a", vec![]),
            ],
            ..Default::default()
        };
        let problems = p.problems().join(" | ");
        assert!(problems.contains("дважды"), "{problems}");
        assert!(problems.contains("пустая команда"), "{problems}");
        assert!(problems.contains("призрак"), "{problems}");
        assert!(problems.contains("стартовый шаг"), "{problems}");
    }

    /// Узел, до которого не ведёт ни один путь, — почти всегда ошибка сборки:
    /// человек думает, что он работает, а очередь до него не доходит.
    #[test]
    fn unreachable_steps_are_reported() {
        let p = Pipeline {
            start: String::new(),
            steps: vec![
                agent("старт", vec![flow("", Cond::Always)]),
                agent("забытый", vec![]),
            ],
            ..Default::default()
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
                gate(
                    "тесты",
                    StepKind::Shell { command: "cargo test".into() },
                    vec![flow("правка", Cond::Fail), flow("", Cond::Always)],
                ),
            ],
            ..Default::default()
        };
        assert!(p.problems().is_empty(), "{:?}", p.problems());
        assert!(p.unreachable().is_empty());
    }

    /* ---------------- параллель ---------------- */

    fn parallel_pipeline() -> Pipeline {
        Pipeline {
            start: "разойтись".into(),
            steps: vec![
                gate("разойтись", StepKind::Fork, vec![flow("фронт", Cond::Always), flow("бэк", Cond::Always)]),
                agent("фронт", vec![flow("свести", Cond::Always)]),
                agent("бэк", vec![flow("свести", Cond::Always)]),
                gate("свести", StepKind::Join, vec![flow("", Cond::Always)]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_join_knows_how_many_branches_to_wait_for() {
        let p = parallel_pipeline();
        assert_eq!(p.join_arity("свести"), 2);
        assert_eq!(p.incoming("свести").len(), 2);
        assert!(p.has_parallel());
        assert!(p.problems().is_empty(), "{:?}", p.problems());
        assert_eq!(p.join_after("разойтись").as_deref(), Some("свести"));
    }

    /// Ветви, которые никуда не сходятся, — это потерянные worktree'ы: работа
    /// сделана и не влита никуда.
    #[test]
    fn a_fork_without_a_join_is_a_problem() {
        let p = Pipeline {
            start: "разойтись".into(),
            steps: vec![
                gate("разойтись", StepKind::Fork, vec![flow("фронт", Cond::Always), flow("бэк", Cond::Always)]),
                agent("фронт", vec![]),
                agent("бэк", vec![]),
            ],
            ..Default::default()
        };
        let problems = p.problems().join(" | ");
        assert!(problems.contains("сходятся"), "{problems}");
        assert!(p.join_after("разойтись").is_none());
    }

    /// «Для каждого» — свойство шага с работой. На шлюзе размножать нечего, у
    /// вопроса человеку работы нет, а список обязан быть выражением: текстом
    /// его пишут по ошибке, и молча размножиться по одной строке было бы худшим
    /// исходом.
    #[test]
    fn each_is_only_for_steps_that_do_work() {
        let mut s = agent("починить", vec![flow("", Cond::Always)]);
        s.over = "${найти.вывод}".into();
        let ok = Pipeline { start: "починить".into(), steps: vec![s.clone()], ..Default::default() };
        assert!(ok.problems().is_empty(), "{:?}", ok.problems());
        assert!(ok.has_parallel(), "«для каждого» — это тоже параллель");

        s.over = "просто текст".into();
        let text = Pipeline { start: "починить".into(), steps: vec![s.clone()], ..Default::default() };
        assert!(text.problems().iter().any(|x| x.contains("выражение со списком")), "{:?}", text.problems());

        let mut g = gate("развилка", StepKind::Choice, vec![flow("", Cond::Always)]);
        g.over = "${x.вывод}".into();
        let onGate = Pipeline { start: "развилка".into(), steps: vec![g], ..Default::default() };
        assert!(onGate.problems().iter().any(|x| x.contains("только у шага с работой")), "{:?}", onGate.problems());

        let mut h = gate("спросить", StepKind::Human { question: "как?".into() }, vec![flow("", Cond::Always)]);
        h.over = "${x.вывод}".into();
        let onHuman = Pipeline { start: "спросить".into(), steps: vec![h], ..Default::default() };
        assert!(onHuman.problems().iter().any(|x| x.contains("размножать человек нечего")), "{:?}", onHuman.problems());
    }

    #[test]
    fn a_fork_needs_at_least_two_branches() {
        let p = Pipeline {
            start: "разойтись".into(),
            steps: vec![
                gate("разойтись", StepKind::Fork, vec![flow("один", Cond::Always)]),
                agent("один", vec![flow("свести", Cond::Always)]),
                gate("свести", StepKind::Join, vec![]),
            ],
            ..Default::default()
        };
        let problems = p.problems().join(" | ");
        assert!(problems.contains("хотя бы две"), "{problems}");
        // И слияние честно называет ту единственную ветку, что до него дошла:
        // «меньше двух» без имени заставляет искать её глазами по схеме.
        assert!(problems.contains("ведёт только «один»"), "{problems}");
    }

    /* ---------------- выражения ---------------- */

    #[test]
    fn camunda_expressions_are_understood_both_ways() {
        for (text, cond) in [
            ("${ok}", Cond::Ok),
            ("${fail}", Cond::Fail),
            ("${verdict == 'return'}", Cond::Verdict { verdict: "return".into() }),
            ("${contains(output, 'FAILED')}", Cond::Contains { text: "FAILED".into() }),
        ] {
            assert_eq!(parse_expr(text), Some(cond.clone()), "{text}");
            assert_eq!(expr_of(&cond), text, "обратно в файл — тем же текстом");
        }
        // Пустое и `true` — безусловный переход.
        assert_eq!(parse_expr(""), Some(Cond::Always));
        assert_eq!(parse_expr("${true}"), Some(Cond::Always));
        assert_eq!(expr_of(&Cond::Always), "", "у безусловного перехода выражения нет");
        // Код возврата — тот же успех, но так его пишут в EL.
        assert_eq!(parse_expr("${code == 0}"), Some(Cond::Ok));
        assert_eq!(parse_expr("${code != 0}"), Some(Cond::Fail));
    }

    /// Непонятое выражение не срабатывает НИКОГДА и говорит об этом вслух.
    /// Молчаливый пропуск здесь означал бы ход по стрелке, смысла которой мы
    /// не поняли.
    #[test]
    fn an_unparsed_expression_never_fires_and_is_reported() {
        let weird = Cond::Expr { text: "${myBean.decide(execution)}".into() };
        assert!(!matches(&weird, &Outcome { ok: true, ..Default::default() }));
        assert!(parse_expr("${myBean.decide(execution)}").is_none());
        let p = Pipeline {
            start: "a".into(),
            steps: vec![agent("a", vec![flow("", weird.clone())])],
            ..Default::default()
        };
        assert!(p.problems().iter().any(|x| x.contains("не разобрано")), "{:?}", p.problems());
        // А в файл оно уезжает ровно тем же текстом — чужую правку не упрощаем.
        assert_eq!(expr_of(&weird), "${myBean.decide(execution)}");
    }

    /// Формат на диске общий с панелью: узел обязан читаться и писаться так,
    /// как его положит конструктор.
    #[test]
    fn the_wire_format_is_flat_and_readable() {
        let mut s = Step::node("тесты", StepKind::Shell { command: "cargo test".into() }, vec![flow("правка", Cond::Fail)]);
        s.name = "прогнать тесты".into();
        s.retries = 2;
        s.x = 320;
        s.y = 80;
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains(r#""kind":"shell""#), "{text}");
        assert!(text.contains(r#""command":"cargo test""#), "{text}");
        assert!(text.contains(r#""cond":"fail""#), "{text}");
        let back: Step = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    /// Старый пайплайн (без координат и шлюзов) обязан читаться как прежде:
    /// на диске у людей лежат заведённые циклы.
    #[test]
    fn an_old_pipeline_still_reads() {
        let old = r#"{"start":"a","steps":[
            {"id":"a","name":"правка","kind":"agent","prompt":"делай","model":"",
             "retries":0,"next":[{"to":"","cond":"always"}]}]}"#;
        let p: Pipeline = serde_json::from_str(old).unwrap();
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].x, 0, "координат не было — и не выдумываем");
        assert!(p.bpmn_file.is_empty());
        assert!(p.problems().is_empty(), "{:?}", p.problems());
    }
}
