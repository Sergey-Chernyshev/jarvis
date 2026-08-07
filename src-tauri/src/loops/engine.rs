//! Движок цикла: итерация за итерацией, пока не выполнено условие выхода или
//! не сработал ограничитель.
//!
//! Одна итерация — это: собрать промт (цель + задачи + дневник + реплики
//! человека) → позвать агента → прогнать гейты → спросить критика → записать в
//! журнал. Ограничители проверяются ПЕРЕД началом: смысл стены в том, чтобы не
//! начинать работу, на которую нет бюджета.

use super::model::*;
use super::runner;
use super::store::Store;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Сколько ждать одну итерацию агента.
const ITERATION_TIMEOUT: Duration = Duration::from_secs(3600);
/// Сколько ждать критика: он только читает дифф.
const CRITIC_TIMEOUT: Duration = Duration::from_secs(600);
/// Сколько диффа отдавать критику.
const DIFF_FOR_CRITIC: usize = 60_000;

/// Вердикт критика, разобранный из его ответа.
#[derive(Debug, PartialEq)]
pub enum CriticSays {
    Fine,
    Return(String),
    /// Критик увидел спорное решение и хочет человека.
    Ask(String),
}

/// Разбор ответа критика.
///
/// Договор простой и печатается в самом промте: первая строка — вердикт. Гадать
/// по тексту нельзя: «выглядит нормально, но тесты снял» не должно проходить за
/// одобрение только потому, что в нём есть слово «нормально».
pub fn parse_critic(text: &str) -> CriticSays {
    let body = text.trim();
    let first = body.lines().next().unwrap_or("").trim().to_uppercase();
    let rest = body.lines().skip(1).collect::<Vec<_>>().join("\n").trim().to_string();
    if first.starts_with("OK") {
        CriticSays::Fine
    } else if first.starts_with("ASK") {
        CriticSays::Ask(if rest.is_empty() { body.to_string() } else { rest })
    } else {
        // Всё, что не опознано, — возврат. Непонятый вердикт не имеет права
        // выпускать работу наружу.
        CriticSays::Return(if rest.is_empty() { body.to_string() } else { rest })
    }
}

fn critic_prompt(item: &Loop, iteration: &Iteration, diff: &str) -> String {
    let own = item.exit.critic.prompt.trim();
    let head = if own.is_empty() {
        "Ты ревьюишь работу автономного цикла. Смотри по существу: сделано ли то, \
         что просили, нет ли обхода проблемы вместо решения (снятые тесты, \
         заглушки, ослабленные проверки)."
    } else {
        own
    };
    format!(
        "{head}\n\nЦель цикла: {goal}\n\nЧто сделала итерация: {summary}\n\n\
         Ответь РОВНО в таком виде. Первая строка — вердикт одним словом:\n\
         OK — работу можно принять\n\
         RETURN — вернуть на доработку\n\
         ASK — решение спорное, нужен человек\n\
         Со второй строки — причина, коротко и по делу.\n\n\
         Дифф:\n{diff}",
        head = head,
        goal = item.source.goal,
        summary = iteration.summary,
        diff = diff,
    )
}

/// Промт итерации: цель, задачи, дневник и то, что человек успел сказать.
pub fn iteration_prompt(
    item: &Loop,
    run: &Run,
    tasks: &str,
    notes: &str,
    last_return: &str,
) -> String {
    let mut p = String::new();
    p.push_str(&format!("Ты — итерация {} автономного цикла «{}».\n\n", run.iterations.len() + 1, item.name));
    p.push_str(&format!("Цель цикла: {}\n\n", item.source.goal));
    if !tasks.trim().is_empty() {
        p.push_str(&format!("Задачи из источника:\n{}\n\n", tasks.trim()));
    }
    if !notes.trim().is_empty() {
        // Дневник — против дня сурка: без него агент второй раз наступает на
        // те же грабли и второй раз их описывает.
        p.push_str(&format!(
            "Дневник цикла — решения, грабли и что не делать. Прочитай прежде, чем начинать:\n{}\n\n",
            notes.trim()
        ));
    }
    if !last_return.trim().is_empty() {
        p.push_str(&format!("Прошлую итерацию вернули на доработку: {}\n\n", last_return.trim()));
    }
    if !run.interventions.is_empty() {
        p.push_str(&format!(
            "Человек вмешался в цикл — это важнее всего остального:\n{}\n\n",
            run.interventions.join("\n")
        ));
    }
    if !item.exit.gates.is_empty() {
        let names: Vec<&str> = item.exit.gates.iter().map(|g| g.name.as_str()).collect();
        p.push_str(&format!(
            "Работа считается сделанной, когда проходят гейты: {}. Их прогонят после тебя.\n\n",
            names.join(", ")
        ));
    }
    if item.memory.enabled {
        p.push_str(&format!(
            "Допиши в {} то, что стоит помнить следующей итерации: принятые решения, \
             грабли, что НЕ делать. Коротко, без пересказа сделанного.\n\n",
            item.memory.file
        ));
    }
    p.push_str(
        "Сделай ОДИН шаг к цели и остановись. Первой строкой ответа — что именно ты сделал, \
         одним предложением.",
    );
    p
}

/// Первая строка ответа агента — это и есть сводка итерации.
pub fn summarize(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("без ответа");
    crate::util::ellipsize(&crate::util::one_line(line), 160)
}

/// Попадает ли итерация в выборку.
pub fn is_sampled(sampling: &Sampling, n: u32) -> bool {
    sampling.every > 0 && n % sampling.every == 0
}

/// Прогнать один запуск цикла до конца.
///
/// Возвращает управление, когда запуск завершился: условие выхода, ограничитель
/// или остановка человеком. Промежуточные состояния кладутся в стор — панель
/// читает их оттуда.
pub async fn run_loop(store: Arc<Store>, item: Loop, run_n: u32, on_change: impl Fn(&Run)) {
    let started = crate::util::now_ms();
    let (dir, branch) = match runner::make_sandbox(&item, run_n).await {
        Ok(v) => v,
        Err(why) => {
            let run = Run {
                loop_id: item.id.clone(),
                n: run_n,
                state: RunState::Stopped,
                started_at: started,
                ended_at: crate::util::now_ms(),
                stop: StopReason::Failed,
                stop_note: why,
                ..Default::default()
            };
            store.put_run(run.clone());
            on_change(&run);
            return;
        }
    };

    let mut run = Run {
        loop_id: item.id.clone(),
        n: run_n,
        state: RunState::Running,
        started_at: started,
        branch: branch.clone(),
        worktree: dir.to_string_lossy().into_owned(),
        ..Default::default()
    };
    store.put_run(run.clone());
    on_change(&run);

    let mut last_return = String::new();
    loop {
        // Человек мог остановить цикл, пока шла итерация: стор — единственный
        // источник правды о том, чего он хочет прямо сейчас.
        if let Some(current) = store.run(&item.id) {
            if current.state == RunState::Stopped {
                return;
            }
            run.interventions = current.interventions.clone();
        }
        let now = crate::util::now_ms();
        if let Some(reason) = run.tripped(&item.limits, now) {
            finish(&store, &mut run, reason, &on_change);
            return;
        }

        let n = run.iterations.len() as u32 + 1;
        let mut it = Iteration { n, started_at: now, verdict: Verdict::Running, ..Default::default() };
        run.iterations.push(it.clone());
        store.put_run(run.clone());
        on_change(&run);

        // Источник задач — настоящая команда: список задач устарел бы к первой
        // же ночи.
        let tasks = if item.source.command.trim().is_empty() {
            String::new()
        } else {
            let (_, out) = runner::shell(&dir, &item.source.command, Duration::from_secs(300)).await;
            runner::tail(&out, 40)
        };
        let notes = read_notes(&item, &dir);
        let prompt = iteration_prompt(&item, &run, &tasks, &notes, &last_return);

        let out = runner::run_agent(&item.agent, &dir, &prompt, None, ITERATION_TIMEOUT).await;
        it.tokens = out.tokens;
        it.cost_usd = out.cost_usd;
        it.summary = summarize(&out.text);
        run.tokens += out.tokens;
        run.cost_usd += out.cost_usd;
        if out.failed {
            it.verdict = Verdict::Failed;
            it.ended_at = crate::util::now_ms();
            put_iteration(&mut run, it);
            finish(&store, &mut run, StopReason::Failed, &on_change);
            return;
        }
        // Реплики человека доехали до агента — очередь можно чистить.
        run.interventions.clear();
        it.files = runner::touched_files(&dir).await;

        it.gates = runner::run_gates(&item.exit.gates, &dir).await;
        let gates_ok = it.gates.iter().all(|g| g.ok);
        if !gates_ok {
            it.verdict = Verdict::GateFailed;
            last_return = it
                .gates
                .iter()
                .find(|g| !g.ok)
                .map(|g| format!("красный гейт «{}»:\n{}", g.name, runner::tail(&g.output, 20)))
                .unwrap_or_default();
            run.streak = 0;
        } else if item.exit.critic.enabled {
            let d = runner::diff(&dir, DIFF_FOR_CRITIC).await;
            let cp = critic_prompt(&item, &it, &d);
            let model = Some(item.exit.critic.model.as_str()).filter(|m| !m.is_empty());
            let verdict = runner::run_agent(&item.agent, &dir, &cp, model, CRITIC_TIMEOUT).await;
            run.tokens += verdict.tokens;
            run.cost_usd += verdict.cost_usd;
            match parse_critic(&verdict.text) {
                CriticSays::Fine => {
                    it.verdict = Verdict::Passed;
                    run.streak += 1;
                }
                CriticSays::Return(why) => {
                    it.verdict = Verdict::Returned;
                    it.critic = why.clone();
                    last_return = why;
                    run.streak = 0;
                }
                CriticSays::Ask(what) => {
                    // human by exception: спорное решение уходит человеку, а
                    // цикл встаёт. Продолжать «на своё усмотрение» — ровно то,
                    // из-за чего к автономности и теряют доверие.
                    it.verdict = Verdict::Returned;
                    it.critic = what.clone();
                    it.ended_at = crate::util::now_ms();
                    put_iteration(&mut run, it);
                    run.state = RunState::Asking;
                    run.ask = Some(Ask {
                        at: crate::util::now_ms(),
                        question: what,
                        options: Vec::new(),
                        iteration: n,
                    });
                    store.put_run(run.clone());
                    on_change(&run);
                    return;
                }
            }
        } else {
            it.verdict = Verdict::Passed;
            run.streak += 1;
        }

        it.sampled = is_sampled(&item.sampling, n);
        it.ended_at = crate::util::now_ms();
        put_iteration(&mut run, it);
        store.put_run(run.clone());
        on_change(&run);

        if run.streak >= item.exit.streak.max(1) {
            finish(&store, &mut run, StopReason::Exit, &on_change);
            return;
        }
    }
}

fn put_iteration(run: &mut Run, it: Iteration) {
    match run.iterations.iter_mut().find(|x| x.n == it.n) {
        Some(slot) => *slot = it,
        None => run.iterations.push(it),
    }
}

fn finish(store: &Store, run: &mut Run, reason: StopReason, on_change: &impl Fn(&Run)) {
    run.state = if reason == StopReason::Exit { RunState::Done } else { RunState::Stopped };
    run.stop = reason;
    run.ended_at = crate::util::now_ms();
    run.stop_note = match reason {
        StopReason::Exit => "условие выхода выполнено".into(),
        StopReason::Tokens => "ограничитель: токены за запуск".into(),
        StopReason::Iterations => "ограничитель: итерации за запуск".into(),
        StopReason::Time => "ограничитель: время запуска".into(),
        StopReason::Drift => "цикл ушёл от исходной цели".into(),
        StopReason::Stopped => "остановлен вручную".into(),
        StopReason::Failed => run.stop_note.clone(),
        StopReason::None => String::new(),
    };
    store.put_run(run.clone());
    on_change(run);
}

fn read_notes(item: &Loop, dir: &Path) -> String {
    if !item.memory.enabled {
        return String::new();
    }
    std::fs::read_to_string(dir.join(&item.memory.file))
        .map(|t| runner::tail(&t, 120))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critic_verdict_is_read_from_the_first_line_only() {
        assert_eq!(parse_critic("OK\nвыглядит хорошо"), CriticSays::Fine);
        assert_eq!(parse_critic("ok"), CriticSays::Fine);
        assert_eq!(
            parse_critic("RETURN\nснял тест вместо починки"),
            CriticSays::Return("снял тест вместо починки".into())
        );
        assert_eq!(
            parse_critic("ASK\nснимать ли флаки-тест"),
            CriticSays::Ask("снимать ли флаки-тест".into())
        );
    }

    #[test]
    fn an_unrecognised_verdict_never_passes_work_through() {
        // «выглядит нормально, но тесты снял» не должно проходить за одобрение
        // только потому, что в нём есть слово «нормально».
        match parse_critic("выглядит нормально, но тесты снял") {
            CriticSays::Return(why) => assert!(why.contains("тесты снял")),
            other => panic!("непонятый вердикт обязан быть возвратом, а не {other:?}"),
        }
        assert!(matches!(parse_critic(""), CriticSays::Return(_)));
    }

    #[test]
    fn sampling_shows_every_nth_iteration() {
        let s = Sampling { every: 3 };
        assert!(!is_sampled(&s, 1));
        assert!(!is_sampled(&s, 2));
        assert!(is_sampled(&s, 3));
        assert!(is_sampled(&s, 6));
        // Ноль — выборка выключена, а не «каждая».
        assert!(!is_sampled(&Sampling { every: 0 }, 5));
    }

    #[test]
    fn summary_is_the_first_meaningful_line() {
        assert_eq!(summarize("\n\n  Починил флаки-тест  \nдальше подробности"), "Починил флаки-тест");
        assert_eq!(summarize(""), "без ответа");
    }

    #[test]
    fn prompt_carries_goal_notes_and_human_words() {
        let mut item = Loop { name: "test-fix".into(), ..Default::default() };
        item.source.goal = "чинить флаки".into();
        item.exit.gates = vec![Gate { name: "тесты".into(), command: "cargo test".into() }];
        let run = Run { interventions: vec!["не трогай CI".into()], ..Default::default() };

        let p = iteration_prompt(&item, &run, "#12 упал тест", "не трогать adopt_tmux", "");
        assert!(p.contains("чинить флаки"));
        assert!(p.contains("#12 упал тест"));
        assert!(p.contains("не трогать adopt_tmux"), "дневник обязан попасть в промт");
        assert!(p.contains("не трогай CI"), "реплика человека обязана попасть в промт");
        assert!(p.contains("тесты"), "агент должен знать, чем его будут проверять");
        assert!(p.contains("ОДИН шаг"), "итерация — это шаг, а не вся работа разом");
    }

    #[test]
    fn memory_is_not_mentioned_when_it_is_off() {
        let mut item = Loop::default();
        item.memory.enabled = false;
        let p = iteration_prompt(&item, &Run::default(), "", "", "");
        assert!(!p.contains("Допиши"), "выключенная память не должна просачиваться в промт");
    }

    #[test]
    fn critic_prompt_states_the_contract() {
        let item = Loop::default();
        let it = Iteration { summary: "починил".into(), ..Default::default() };
        let p = critic_prompt(&item, &it, "diff --git");
        for word in ["OK", "RETURN", "ASK", "diff --git", "починил"] {
            assert!(p.contains(word), "в промте критика нет «{word}»");
        }
    }
}
