//! Исполнение пайплайна: ход идёт по графу, а не по зашитому списку.
//!
//! Шаг за шагом: выполнить → записать в журнал → выбрать переход по результату.
//! Всё, что уже придумано для обычного цикла, остаётся на месте — песочница,
//! ограничители, журнал, остановка человеком; меняется только то, ЧТО и в каком
//! порядке выполняется.
//!
//! Три вещи взяты у Camunda и стоят того, чтобы их назвать:
//!
//! * попытки: шаг, сорвавшийся из-за моргнувшей сети, повторяется сам, и лишь
//!   исчерпав их, зовёт человека;
//! * инцидент: если идти некуда, прогон ВСТАЁТ и говорит, на каком шаге, а не
//!   делает вид, что всё хорошо;
//! * вопрос человеку — обычный шаг, а не особый случай: прогон замирает, ответ
//!   становится результатом шага и едет дальше по переходам.

use super::model::*;
use super::pipeline::{self, Outcome, Pipeline, Step, StepKind};
use super::runner;
use super::store::Store;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const STEP_TIMEOUT: Duration = Duration::from_secs(3600);
const SHELL_TIMEOUT: Duration = Duration::from_secs(1800);
const REVIEW_TIMEOUT: Duration = Duration::from_secs(600);
const DIFF_FOR_REVIEW: usize = 60_000;

/// Промт ревьюера в пайплайне: договор о первой строке — тот же, что у критика
/// обычного цикла, и по той же причине (гадать по тексту нельзя).
pub fn review_prompt(own: &str, diff: &str) -> String {
    let head = if own.trim().is_empty() {
        "Ты ревьюишь работу шага пайплайна. Смотри по существу: сделано ли то, \
         что просили, нет ли обхода задачи вместо решения."
    } else {
        own.trim()
    };
    format!(
        "{head}\n\nОтветь РОВНО так. Первая строка — вердикт одним словом:\n\
         OK — принять\nRETURN — вернуть на доработку\nASK — нужен человек\n\
         Со второй строки — коротко и по делу.\n\nДифф:\n{diff}"
    )
}

/// Итог шага словами — он же попадает в журнал.
pub fn summarize(step: &Step, out: &Outcome) -> String {
    let head = step.title();
    let tail = crate::util::ellipsize(&crate::util::one_line(out.output.trim()), 160);
    match (&step.kind, out.ok) {
        (StepKind::Shell { .. }, true) => format!("{head}: прошла"),
        (StepKind::Shell { .. }, false) => format!("{head}: код {} · {tail}", out.code),
        (StepKind::Review { .. }, _) => format!("{head}: {} · {tail}", verdict_word(&out.verdict)),
        (StepKind::Human { .. }, _) => format!("{head}: {tail}"),
        (StepKind::Wait { minutes }, _) => format!("{head}: подождали {minutes} мин"),
        (_, true) => format!("{head}: {tail}"),
        (_, false) => format!("{head}: сорвался · {tail}"),
    }
}

fn verdict_word(v: &str) -> &'static str {
    match v {
        "ok" => "принято",
        "ask" => "нужен человек",
        "return" => "возврат",
        _ => "без вердикта",
    }
}

/// Вердикт шага в вердикт журнала — чтобы строка прогона читалась как раньше.
fn verdict_of(step: &Step, out: &Outcome) -> Verdict {
    match (&step.kind, out.ok) {
        (StepKind::Shell { .. }, false) => Verdict::GateFailed,
        (StepKind::Review { .. }, _) if out.verdict == "return" => Verdict::Returned,
        (_, true) => Verdict::Passed,
        (_, false) => Verdict::Failed,
    }
}

/// Выполнить один шаг. Возвращает результат для условий переходов.
async fn run_step(
    item: &Loop,
    dir: &Path,
    step: &Step,
    vars: &HashMap<String, Outcome>,
) -> (Outcome, u64, f64) {
    match &step.kind {
        StepKind::Agent { prompt, model } => {
            let text = pipeline::interpolate(prompt, vars);
            let model = (!model.trim().is_empty()).then(|| model.trim());
            let out = runner::run_agent(&item.agent, dir, &text, model, STEP_TIMEOUT).await;
            (
                Outcome {
                    ok: !out.failed,
                    output: out.text.clone(),
                    code: 0,
                    verdict: String::new(),
                },
                out.tokens,
                out.cost_usd,
            )
        }
        StepKind::Shell { command } => {
            let cmd = pipeline::interpolate(command, vars);
            let (code, text) = runner::shell(dir, &cmd, SHELL_TIMEOUT).await;
            (
                Outcome {
                    ok: code == 0,
                    output: runner::tail(&text, 60),
                    code,
                    verdict: String::new(),
                },
                0,
                0.0,
            )
        }
        StepKind::Review { prompt, model } => {
            let diff = runner::diff(dir, DIFF_FOR_REVIEW).await;
            let text = review_prompt(&pipeline::interpolate(prompt, vars), &diff);
            let model = (!model.trim().is_empty()).then(|| model.trim());
            let out = runner::run_agent(&item.agent, dir, &text, model, REVIEW_TIMEOUT).await;
            let verdict = match super::engine::parse_critic(&out.text) {
                super::engine::CriticSays::Fine => "ok",
                super::engine::CriticSays::Ask(_) => "ask",
                super::engine::CriticSays::Return(_) => "return",
            };
            (
                Outcome {
                    // «Успех» ревью — это принято: на нём и стоит условие Ok.
                    ok: verdict == "ok" && !out.failed,
                    output: out.text.clone(),
                    code: 0,
                    verdict: verdict.into(),
                },
                out.tokens,
                out.cost_usd,
            )
        }
        StepKind::Wait { minutes } => {
            tokio::time::sleep(Duration::from_secs(*minutes as u64 * 60)).await;
            (
                Outcome {
                    ok: true,
                    ..Default::default()
                },
                0,
                0.0,
            )
        }
        // Вопрос человеку исполняется не здесь: прогон замирает, а ответ
        // приходит из панели. Сюда мы попадаем только при возобновлении.
        StepKind::Human { question } => (
            Outcome {
                ok: true,
                output: pipeline::interpolate(question, vars),
                ..Default::default()
            },
            0,
            0.0,
        ),
    }
}

/// Прогнать пайплайн. Возвращает управление, когда прогон кончился, встал на
/// вопросе или его остановил человек.
pub async fn run_pipeline(
    store: Arc<Store>,
    item: Loop,
    p: Pipeline,
    mut run: Run,
    dir: std::path::PathBuf,
    on_change: impl Fn(&Run),
) {
    let mut vars: HashMap<String, Outcome> = HashMap::new();
    // Возобновление: продолжаем с того шага, на котором встали, а ответ
    // человека уже лежит в его результате.
    let mut current = match run.ask.as_ref().map(|a| a.step.clone()) {
        Some(step) if !step.is_empty() => Some(step),
        _ => p.first().map(|s| s.id.clone()),
    };
    if let Some(a) = run.ask.take() {
        if !a.step.is_empty() {
            let answer = run.interventions.join("\n");
            let out = Outcome {
                ok: true,
                output: answer,
                ..Default::default()
            };
            if let Some(step) = p.step(&a.step) {
                vars.insert(a.step.clone(), out.clone());
                current = pipeline::next_step(step, &out);
            }
            run.interventions.clear();
        }
    }

    while let Some(id) = current.clone() {
        if let Some(live) = store.run(&item.id) {
            if live.state == RunState::Stopped {
                return;
            }
            run.interventions = live.interventions.clone();
        }
        let now = crate::util::now_ms();
        if let Some(reason) = run.tripped(&item.limits, now) {
            finish(&store, &mut run, reason, String::new(), &on_change);
            return;
        }
        let Some(step) = p.step(&id).cloned() else {
            // Ссылка в никуда: проверки её не пропускают, но файл могли
            // поправить руками — молчать об этом нельзя.
            finish(
                &store,
                &mut run,
                StopReason::Failed,
                format!("шаг «{id}» не найден"),
                &on_change,
            );
            return;
        };

        // Вопрос человеку: замираем и ждём ответа из панели.
        if let StepKind::Human { question } = &step.kind {
            run.state = RunState::Asking;
            run.ask = Some(Ask {
                at: crate::util::now_ms(),
                question: pipeline::interpolate(question, &vars),
                options: Vec::new(),
                iteration: run.iterations.len() as u32,
                step: step.id.clone(),
            });
            store.put_run(run.clone());
            on_change(&run);
            return;
        }

        let n = run.iterations.len() as u32 + 1;
        let mut it = Iteration {
            n,
            started_at: now,
            verdict: Verdict::Running,
            summary: format!("{} — идёт", step.title()),
            step: step.id.clone(),
            ..Default::default()
        };
        run.iterations.push(it.clone());
        store.put_run(run.clone());
        on_change(&run);

        // Попытки: моргнувшая сеть — не повод будить человека.
        let mut out;
        let mut tokens;
        let mut cost;
        let mut attempt = 0;
        loop {
            let (o, t, c) = run_step(&item, &dir, &step, &vars).await;
            out = o;
            tokens = t;
            cost = c;
            run.tokens += tokens;
            run.cost_usd += cost;
            if out.ok || attempt >= step.retries {
                break;
            }
            attempt += 1;
            it.summary = format!("{} — попытка {}", step.title(), attempt + 1);
            put_iteration(&mut run, it.clone());
            store.put_run(run.clone());
            on_change(&run);
        }

        it.tokens = tokens;
        it.cost_usd = cost;
        it.verdict = verdict_of(&step, &out);
        it.summary = summarize(&step, &out);
        it.ended_at = crate::util::now_ms();
        it.files = runner::touched_files(&dir).await;
        put_iteration(&mut run, it);
        vars.insert(step.id.clone(), out.clone());
        store.put_run(run.clone());
        on_change(&run);

        let next = pipeline::next_step(&step, &out);
        if next.is_none() && !out.ok {
            // Инцидент: шаг сорвался, а перехода на этот случай не задано.
            // Молча закончить «успешно» было бы худшим из возможных исходов.
            finish(
                &store,
                &mut run,
                StopReason::Failed,
                format!("шаг «{}» сорвался, а перехода на этот случай нет", step.title()),
                &on_change,
            );
            return;
        }
        current = next;
    }

    finish(&store, &mut run, StopReason::Exit, String::new(), &on_change);
}

fn put_iteration(run: &mut Run, it: Iteration) {
    match run.iterations.iter_mut().find(|x| x.n == it.n) {
        Some(slot) => *slot = it,
        None => run.iterations.push(it),
    }
}

fn finish(
    store: &Store,
    run: &mut Run,
    reason: StopReason,
    note: String,
    on_change: &impl Fn(&Run),
) {
    run.state = if reason == StopReason::Exit {
        RunState::Done
    } else {
        RunState::Stopped
    };
    run.stop = reason;
    run.ended_at = crate::util::now_ms();
    if !note.is_empty() {
        run.stop_note = note;
    } else if run.stop_note.is_empty() {
        run.stop_note = match reason {
            StopReason::Exit => "пайплайн дошёл до конца".into(),
            StopReason::Tokens => "ограничитель: токены за запуск".into(),
            StopReason::Iterations => "ограничитель: шаги за запуск".into(),
            StopReason::Time => "ограничитель: время запуска".into(),
            _ => String::new(),
        };
    }
    store.put_run(run.clone());
    on_change(run);
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::pipeline::Flow;

    fn step(id: &str, kind: StepKind, next: Vec<Flow>) -> Step {
        Step {
            id: id.into(),
            name: String::new(),
            kind,
            retries: 0,
            next,
        }
    }

    #[test]
    fn a_step_summary_says_what_happened() {
        let sh = step("тесты", StepKind::Shell { command: "cargo test".into() }, vec![]);
        let ok = Outcome { ok: true, ..Default::default() };
        assert_eq!(summarize(&sh, &ok), "тесты: прошла");
        let bad = Outcome { ok: false, code: 101, output: "3 failed".into(), ..Default::default() };
        assert!(summarize(&sh, &bad).contains("код 101"), "{}", summarize(&sh, &bad));

        let rev = step("ревью", StepKind::Review { prompt: String::new(), model: String::new() }, vec![]);
        let returned = Outcome { ok: false, verdict: "return".into(), output: "тесты сняты".into(), ..Default::default() };
        let text = summarize(&rev, &returned);
        assert!(text.contains("возврат") && text.contains("тесты сняты"), "{text}");
    }

    /// Вердикт журнала должен читаться так же, как у обычного цикла: красная
    /// команда — «красный гейт», возврат ревьюера — «возврат».
    #[test]
    fn journal_verdicts_match_the_old_words() {
        let sh = step("тесты", StepKind::Shell { command: "x".into() }, vec![]);
        assert_eq!(
            verdict_of(&sh, &Outcome { ok: false, ..Default::default() }),
            Verdict::GateFailed
        );
        let rev = step("ревью", StepKind::Review { prompt: String::new(), model: String::new() }, vec![]);
        assert_eq!(
            verdict_of(&rev, &Outcome { ok: false, verdict: "return".into(), ..Default::default() }),
            Verdict::Returned
        );
        let ag = step("правка", StepKind::Agent { prompt: "x".into(), model: String::new() }, vec![]);
        assert_eq!(verdict_of(&ag, &Outcome { ok: true, ..Default::default() }), Verdict::Passed);
        assert_eq!(verdict_of(&ag, &Outcome { ok: false, ..Default::default() }), Verdict::Failed);
    }

    /// Промт ревьюера обязан печатать договор о вердикте: без него первая
    /// строка ответа — что угодно, и разбор превращается в гадание.
    #[test]
    fn the_review_prompt_prints_the_contract() {
        let p = review_prompt("", "@@ -1 +1 @@");
        assert!(p.contains("OK") && p.contains("RETURN") && p.contains("ASK"));
        assert!(p.contains("Первая строка"));
        // Свой промт человека уважаем — он заменяет шапку, а не приписывается.
        let own = review_prompt("смотри только на тесты", "дифф");
        assert!(own.contains("смотри только на тесты"));
        assert!(!own.contains("Ты ревьюишь работу шага"));
    }
}
