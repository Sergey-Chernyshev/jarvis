//! Исполнение пайплайна: ход идёт по графу, а не по зашитому списку.
//!
//! Ход — это ТОКЕН в терминах BPMN, и здесь он назван так же не ради красивого
//! слова: токенов бывает несколько. Обычный пайплайн — один токен, шагающий от
//! узла к узлу. Ветвление (`Fork`) размножает его, слияние (`Join`) сводит
//! обратно. Всё остальное — попытки, инциденты, вопрос человеку — работает
//! одинаково что для одного хода, что для пяти.
//!
//! Параллель здесь настоящая, и стоит она на одном условии: **у каждой ветки
//! своё рабочее дерево**. Два агента в одном каталоге затирают друг другу
//! файлы — «параллельно» без отдельных worktree было бы просто ложью. Поэтому
//! ветвление поднимает `git worktree` на своей ветке от ТЕКУЩЕГО состояния
//! дорожки, а слияние сводит ветки обратно честным `git merge` и не делает вид,
//! что конфликтов не бывает.
//!
//! Три вещи взяты у Camunda и стоят того, чтобы их назвать:
//!
//! * попытки: узел, сорвавшийся из-за моргнувшей сети, повторяется сам, и лишь
//!   исчерпав их, зовёт человека;
//! * инцидент: если идти некуда — прогон ВСТАЁТ и говорит, на каком узле, а не
//!   делает вид, что всё хорошо. Слияние, до которого пришли не все ветки, —
//!   тоже инцидент, а не вечное ожидание;
//! * вопрос человеку — обычный узел, а не особый случай: прогон замирает, ответ
//!   становится результатом узла и едет дальше по переходам.
//!
//! Про вопрос в параллельном прогоне честно: замирает ВЕСЬ прогон, а не одна
//! ветка. Дать соседним веткам работать дальше, пока человек спит, значит
//! получить к утру слияние с работой, о которой он ещё не высказался.

use super::model::*;
use super::pipeline::{self, Outcome, Pipeline, Step, StepKind};
use super::runner::{self, Merge};
use super::store::Store;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const STEP_TIMEOUT: Duration = Duration::from_secs(3600);
const SHELL_TIMEOUT: Duration = Duration::from_secs(1800);
const REVIEW_TIMEOUT: Duration = Duration::from_secs(600);
/// Разбор конфликта агенту: он читает файлы с маркерами, а не весь репозиторий.
const CONFLICT_TIMEOUT: Duration = Duration::from_secs(900);
const DIFF_FOR_REVIEW: usize = 60_000;
/// Потолок экземпляров у «для каждого».
///
/// Список приходит из вывода команды или ответа агента, то есть может оказаться
/// каким угодно. Двадцать worktree — это уже гигабайты на диске и двадцать
/// параллельных агентов на один лимит аккаунта; всё, что сверху, почти наверняка
/// означает, что список получился не тот, какой человек имел в виду.
const EACH_LIMIT: usize = 20;

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

/// Промт разбора конфликта слияния.
///
/// Агенту отдаём ровно то, чего у нас нет: он видел контекст своих правок, мы
/// нет. Требование одно и жёсткое — убрать маркеры, а не выбрать сторону
/// наугад: «взял верхнюю» здесь означает выбросить чужую ночь работы.
pub fn conflict_prompt(branch: &str, files: &[String]) -> String {
    format!(
        "Слияние ветки «{branch}» дало конфликт. Ты в рабочем дереве, где он и лежит.\n\n\
         Конфликтные файлы:\n{}\n\n\
         Разбери КАЖДЫЙ: открой, пойми обе стороны и оставь версию, в которой сохранены \
         оба намерения. Выбирать сторону целиком нельзя — это выбрасывает чужую работу. \
         Убери все маркеры <<<<<<<, ======= и >>>>>>>. Ничего не коммить: это сделаю я. \
         Если какой-то конфликт разобрать нельзя без человека — так и напиши первой строкой.",
        files.iter().map(|f| format!("  · {f}")).collect::<Vec<_>>().join("\n")
    )
}

/// Итог узла словами — он же попадает в журнал.
pub fn summarize(step: &Step, out: &Outcome) -> String {
    let head = step.title();
    let tail = crate::util::ellipsize(&crate::util::one_line(out.output.trim()), 160);
    match (&step.kind, out.ok) {
        (StepKind::Shell { .. }, true) => format!("{head}: прошла"),
        (StepKind::Shell { .. }, false) => format!("{head}: код {} · {tail}", out.code),
        (StepKind::Review { .. }, _) => format!("{head}: {} · {tail}", verdict_word(&out.verdict)),
        (StepKind::Human { .. }, _) => format!("{head}: {tail}"),
        (StepKind::Wait { minutes }, _) => format!("{head}: подождали {minutes} мин"),
        (StepKind::Join, _) => format!("{head}: {tail}"),
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

/// Вердикт узла в вердикт журнала — чтобы строка прогона читалась как раньше.
fn verdict_of(step: &Step, out: &Outcome) -> Verdict {
    match (&step.kind, out.ok) {
        (StepKind::Shell { .. }, false) => Verdict::GateFailed,
        (StepKind::Review { .. }, _) if out.verdict == "return" => Verdict::Returned,
        (_, true) => Verdict::Passed,
        (_, false) => Verdict::Failed,
    }
}

/// Выполнить один узел. Возвращает результат для условий переходов.
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
            (Outcome { ok: true, ..Default::default() }, 0, 0.0)
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
        // Шлюзы ходом не занимаются — их разбирает сам движок.
        k if k.is_gateway() => (Outcome { ok: true, ..Default::default() }, 0, 0.0),
        _ => (Outcome { ok: true, ..Default::default() }, 0, 0.0),
    }
}

/* ======================= дорожки ======================= */

/// Имя дорожки-ветки: `<родитель>/<узел>`. Читается в журнале как путь, и это
/// ровно то, чем оно является.
fn lane_name(parent: &str, target: &str) -> String {
    if parent.is_empty() {
        target.to_string()
    } else {
        format!("{parent}/{target}")
    }
}

fn lane_of<'a>(run: &'a Run, name: &str) -> Option<&'a LaneState> {
    run.lanes.iter().find(|l| l.name == name)
}

/// Куда положить рабочее дерево ветки: РЯДОМ с деревом родительской дорожки,
/// её именем плюс имя ветки.
///
/// От родителя, а не от `~/.jarvis/worktrees` напрямую, по двум причинам:
/// вложенная параллель тогда читается путём (`проект-3-фронт-стили`), и
/// песочница запуска остаётся единственным местом, которое решает, ГДЕ вообще
/// живут деревья этого прогона.
fn lane_dir(parent_dir: &Path, target: &str) -> PathBuf {
    let base = parent_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "loop".into());
    let next = format!("{base}-{}", ref_slug(target));
    match parent_dir.parent() {
        Some(up) => up.join(next),
        None => PathBuf::from(next),
    }
}

/* ======================= прогон ======================= */

/// Что случилось с одним ходом на этом круге.
struct Done {
    token: TokenState,
    outcome: Outcome,
    iteration: Iteration,
    tokens: u64,
    cost: f64,
}

/// Раскрытое «для каждого»: узел, его экземпляры и их рабочие места.
///
/// Это неявные ветвление и слияние вокруг ОДНОГО узла: рисовать их отдельными
/// шлюзами нельзя — веток столько, сколько строк окажется в списке, а он
/// известен только на ходу.
struct Spread {
    node: String,
    from: String,
    /// Дорожка, в которую сливать экземпляры.
    parent: String,
    /// Имена дорожек экземпляров, по порядку списка.
    lanes: Vec<String>,
}

/// Прогнать пайплайн. Возвращает управление, когда прогон кончился, встал на
/// вопросе или его остановил человек.
pub async fn run_pipeline(
    store: Arc<Store>,
    item: Loop,
    p: Pipeline,
    mut run: Run,
    dir: PathBuf,
    on_change: impl Fn(&Run),
) {
    let repo = PathBuf::from(&item.sandbox.repo);
    if run.lanes.is_empty() {
        run.lanes.push(LaneState {
            name: String::new(),
            parent: String::new(),
            dir: dir.to_string_lossy().into_owned(),
            branch: run.branch.clone(),
        });
    }

    // Ходы: восстановленные из запуска (перезапуск приложения, ответ на вопрос)
    // или один-единственный на стартовом узле.
    let mut pending: Vec<TokenState> = if run.tokens_at.is_empty() {
        p.first()
            .map(|s| vec![TokenState { node: s.id.clone(), from: String::new(), lane: String::new() }])
            .unwrap_or_default()
    } else {
        std::mem::take(&mut run.tokens_at)
    };

    // Ответ человека: он и есть результат того узла, на котором прогон встал.
    if let Some(a) = run.ask.take() {
        if !a.step.is_empty() {
            let out = Outcome {
                ok: true,
                output: run.interventions.join("\n"),
                ..Default::default()
            };
            run.vars.insert(a.step.clone(), out.clone());
            if let Some(step) = p.step(&a.step) {
                let next = pipeline::next_step(step, &out);
                for t in pending.iter_mut().filter(|t| t.node == a.step) {
                    match &next {
                        Some(to) => {
                            t.from = a.step.clone();
                            t.node = to.clone();
                        }
                        // Ответ увёл ход в конец — снимаем токен ниже.
                        None => t.node = String::new(),
                    }
                }
                pending.retain(|t| !t.node.is_empty());
            }
            run.interventions.clear();
        }
    }

    loop {
        // Человек мог остановить прогон, пока шёл круг: стор — единственный
        // источник правды о том, чего он хочет прямо сейчас.
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

        /* ---- 1. шлюзы: гоняем, пока они двигают ход ---- */
        let mut queue = std::mem::take(&mut pending);
        let mut runnable: Vec<TokenState> = Vec::new();
        let mut each: Vec<TokenState> = Vec::new();
        let mut human: Option<TokenState> = None;
        while let Some(t) = queue.pop() {
            let Some(step) = p.step(&t.node).cloned() else {
                // Ссылка в никуда: проверки её не пропускают, но файл могли
                // поправить руками — молчать об этом нельзя.
                finish(&store, &mut run, StopReason::Failed, format!("шаг «{}» не найден", t.node), &on_change);
                return;
            };
            if matches!(step.kind, StepKind::Human { .. }) {
                human = Some(t);
                continue;
            }
            match &step.kind {
                StepKind::Fork => {
                    match fork(&item, &mut run, &repo, &t, &step).await {
                        Ok(born) => queue.extend(born),
                        Err(why) => {
                            finish(&store, &mut run, StopReason::Failed, why, &on_change);
                            return;
                        }
                    }
                }
                StepKind::Join => {
                    let arity = p.join_arity(&t.node);
                    run.waiting_at.push(t.clone());
                    let arrived = run.waiting_at.iter().filter(|w| w.node == t.node).count();
                    if arrived < arity {
                        continue; // ждём остальные ветки
                    }
                    let j = join(&item, &mut run, &repo, &t.node, &step.on_conflict, &on_change).await;
                    run.vars.insert(t.node.clone(), j.out.clone());
                    push_iteration(&mut run, &step, &j.out, &j.lane, j.note);
                    store.put_run(run.clone());
                    on_change(&run);
                    let out = j.out;
                    match pipeline::next_step(&step, &out) {
                        Some(to) => queue.push(TokenState {
                            node: to,
                            from: t.node.clone(),
                            lane: j.lane.clone(),
                        }),
                        None if !out.ok => {
                            finish(&store, &mut run, StopReason::Failed, out.output.clone(), &on_change);
                            return;
                        }
                        None => {}
                    }
                }
                StepKind::Choice => {
                    // Развилка смотрит на результат ПРЕДЫДУЩЕГО узла: своего у
                    // неё нет и быть не может — она ничего не делает.
                    let prev = run.vars.get(&t.from).cloned().unwrap_or_default();
                    if let Some(to) = pipeline::next_step(&step, &prev) {
                        queue.push(TokenState { node: to, from: t.from.clone(), lane: t.lane.clone() });
                    } else if !prev.ok {
                        finish(
                            &store,
                            &mut run,
                            StopReason::Failed,
                            format!("развилка «{}»: ни один переход не подошёл", step.title()),
                            &on_change,
                        );
                        return;
                    }
                }
                // «Для каждого» раскрывается ниже: сначала надо посчитать
                // список, а он берётся из результатов уже сделанных шагов.
                _ if step.is_each() => each.push(t),
                _ => runnable.push(t),
            }
        }

        /* ---- 2. вопрос человеку замораживает ВЕСЬ прогон ---- */
        if let Some(t) = human {
            let step = p.step(&t.node).cloned().unwrap_or_else(|| unreachable_step(&t.node));
            let question = match &step.kind {
                StepKind::Human { question } => pipeline::interpolate(question, &run.vars),
                _ => String::new(),
            };
            run.tokens_at = runnable;
            run.tokens_at.extend(each);
            run.tokens_at.push(t.clone());
            run.state = RunState::Asking;
            run.ask = Some(Ask {
                at: crate::util::now_ms(),
                question,
                options: Vec::new(),
                iteration: run.iterations.len() as u32,
                step: t.node.clone(),
            });
            store.put_run(run.clone());
            on_change(&run);
            return;
        }

        /* ---- 3. некуда идти ---- */
        if runnable.is_empty() && each.is_empty() {
            if !run.waiting_at.is_empty() {
                // Слияние, до которого пришли не все ветки. Ждать вечно — худшее
                // из возможного: прогон выглядел бы работающим, ничего не делая.
                let stuck = &run.waiting_at[0].node;
                let note = format!(
                    "слияние «{stuck}»: пришло {} из {} веток, остальные никуда не ведут",
                    run.waiting_at.iter().filter(|w| &w.node == stuck).count(),
                    p.join_arity(stuck)
                );
                finish(&store, &mut run, StopReason::Failed, note, &on_change);
                return;
            }
            finish(&store, &mut run, StopReason::Exit, String::new(), &on_change);
            return;
        }

        /* ---- 4. раскрыть «для каждого»: дорожка на элемент ---- */
        let mut spreads: Vec<Spread> = Vec::new();
        let mut instances: Vec<(TokenState, String)> = Vec::new(); // ход + его элемент
        for t in each {
            let Some(step) = p.step(&t.node).cloned() else { continue };
            let list = pipeline::interpolate(&step.over, &run.vars);
            let items = pipeline::items_of(&list, EACH_LIMIT);
            if items.is_empty() {
                // Пустой список — не ошибка: чинить нечего, и это нормальный
                // исход. Но и молчать нельзя: строка в журнале обязана быть,
                // иначе шаг выглядит пропущенным без причины.
                let out = Outcome { ok: true, output: String::new(), ..Default::default() };
                run.vars.insert(t.node.clone(), out.clone());
                push_iteration(&mut run, &step, &out, &t.lane, "список пуст — делать нечего".into());
                if let Some(to) = pipeline::next_step(&step, &out) {
                    pending.push(TokenState { node: to, from: t.node.clone(), lane: t.lane.clone() });
                }
                continue;
            }
            match spread(&mut run, &repo, &t, &step, &items).await {
                Ok(sp) => {
                    for (lane, it) in sp.lanes.iter().zip(items.iter()) {
                        instances.push((
                            TokenState { node: t.node.clone(), from: t.from.clone(), lane: lane.clone() },
                            it.clone(),
                        ));
                    }
                    crate::log::line(&format!(
                        "[loops] {}: «{}» для каждого — {} веток",
                        item.name,
                        step.title(),
                        sp.lanes.len()
                    ));
                    spreads.push(sp);
                }
                Err(why) => {
                    finish(&store, &mut run, StopReason::Failed, why, &on_change);
                    return;
                }
            }
        }

        /* ---- 5. круг: все ходы идут ОДНОВРЕМЕННО ---- */
        let mut set = tokio::task::JoinSet::new();
        for (t, what) in instances {
            let Some(step) = p.step(&t.node).cloned() else { continue };
            let n = run.iterations.len() as u32 + 1;
            let Some(lane) = lane_of(&run, &t.lane).cloned() else { continue };
            let mut it = Iteration {
                n,
                started_at: crate::util::now_ms(),
                verdict: Verdict::Running,
                summary: format!("{} · {what} — идёт", step.title()),
                step: step.id.clone(),
                lane: t.lane.clone(),
                ..Default::default()
            };
            run.iterations.push(it.clone());
            let (item, mut vars) = (item.clone(), run.vars.clone());
            // Элемент виден шагу как `${элемент}` — ровно то, ради чего всё это.
            vars.insert(
                "элемент".to_string(),
                Outcome { ok: true, output: what.clone(), ..Default::default() },
            );
            vars.insert(
                "item".to_string(),
                Outcome { ok: true, output: what.clone(), ..Default::default() },
            );
            set.spawn(async move {
                let dir = PathBuf::from(&lane.dir);
                let (mut out, mut tokens, mut cost) = run_step(&item, &dir, &step, &vars).await;
                let mut attempt = 0;
                while !out.ok && attempt < step.retries {
                    attempt += 1;
                    let (o, t2, c2) = run_step(&item, &dir, &step, &vars).await;
                    out = o;
                    tokens += t2;
                    cost += c2;
                }
                it.tokens = tokens;
                it.cost_usd = cost;
                it.verdict = verdict_of(&step, &out);
                it.summary = format!("{} · {what}: {}", step.title(), short(&out));
                it.ended_at = crate::util::now_ms();
                it.files = runner::touched_files(&dir).await;
                Done { token: t, outcome: out, iteration: it, tokens, cost }
            });
        }
        for t in runnable {
            let Some(step) = p.step(&t.node).cloned() else { continue };
            let n = run.iterations.len() as u32 + 1;
            let Some(lane) = lane_of(&run, &t.lane).cloned() else {
                let why = format!("дорожка «{}» пропала — шагу негде работать", t.lane);
                finish(&store, &mut run, StopReason::Failed, why, &on_change);
                return;
            };
            let mut it = Iteration {
                n,
                started_at: crate::util::now_ms(),
                verdict: Verdict::Running,
                summary: format!("{} — идёт", step.title()),
                step: step.id.clone(),
                lane: t.lane.clone(),
                ..Default::default()
            };
            run.iterations.push(it.clone());
            let (item, vars) = (item.clone(), run.vars.clone());
            set.spawn(async move {
                let dir = PathBuf::from(&lane.dir);
                // Попытки: моргнувшая сеть — не повод будить человека.
                let (mut out, mut tokens, mut cost) = run_step(&item, &dir, &step, &vars).await;
                let mut attempt = 0;
                while !out.ok && attempt < step.retries {
                    attempt += 1;
                    let (o, t2, c2) = run_step(&item, &dir, &step, &vars).await;
                    out = o;
                    tokens += t2;
                    cost += c2;
                }
                it.tokens = tokens;
                it.cost_usd = cost;
                it.verdict = verdict_of(&step, &out);
                it.summary = if attempt > 0 {
                    format!("{} (попыток {})", summarize(&step, &out), attempt + 1)
                } else {
                    summarize(&step, &out)
                };
                it.ended_at = crate::util::now_ms();
                it.files = runner::touched_files(&dir).await;
                Done { token: t, outcome: out, iteration: it, tokens, cost }
            });
        }
        store.put_run(run.clone());
        on_change(&run);

        let mut results: Vec<Done> = Vec::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(d) => results.push(d),
                // Задача упала целиком (паника в подпроцессной обвязке). Молчать
                // нельзя: ход исчез бы, и прогон завис бы на слиянии.
                Err(e) => {
                    finish(&store, &mut run, StopReason::Failed, format!("шаг сорвался: {e}"), &on_change);
                    return;
                }
            }
        }
        // Порядок результатов у параллельных ходов недетерминирован — журнал
        // обязан читаться по номерам, а не по тому, кто первым финишировал.
        results.sort_by_key(|d| d.iteration.n);

        let mut incident: Option<String> = None;
        // Экземпляры «для каждого» идут не своей дорогой, а собираются обратно:
        // одному узлу — один результат и один ход дальше.
        let mut gathered: HashMap<String, Vec<Done>> = HashMap::new();
        let mut plain: Vec<Done> = Vec::new();
        for d in results {
            run.tokens += d.tokens;
            run.cost_usd += d.cost;
            put_iteration(&mut run, d.iteration.clone());
            match spreads
                .iter()
                .find(|sp| sp.node == d.token.node && sp.lanes.contains(&d.token.lane))
            {
                Some(sp) => gathered.entry(sp.node.clone()).or_default().push(d),
                None => plain.push(d),
            }
        }

        for d in plain {
            run.vars.insert(d.token.node.clone(), d.outcome.clone());
            let step = match p.step(&d.token.node) {
                Some(s) => s.clone(),
                None => continue,
            };
            match pipeline::next_step(&step, &d.outcome) {
                Some(to) => pending.push(TokenState {
                    node: to,
                    from: d.token.node.clone(),
                    lane: d.token.lane.clone(),
                }),
                None if !d.outcome.ok => {
                    // Инцидент: узел сорвался, а перехода на этот случай нет.
                    // Молча закончить «успешно» было бы худшим из исходов.
                    incident.get_or_insert(format!(
                        "{} «{}» сорвался, а перехода на этот случай нет",
                        step.kind.word(),
                        step.title()
                    ));
                }
                None => {}
            }
        }

        for sp in &spreads {
            let done = gathered.remove(&sp.node).unwrap_or_default();
            let Some(step) = p.step(&sp.node).cloned() else { continue };
            let Some(parent) = lane_of(&run, &sp.parent).cloned() else {
                finish(&store, &mut run, StopReason::Failed, "дорожка «для каждого» пропала".into(), &on_change);
                return;
            };
            // Сливаем ВСЕ ветки, даже если какой-то экземпляр сорвался: его
            // сосед мог сделать работу, и выбрасывать её из-за чужой неудачи
            // не за что. О самой неудаче скажет результат узла.
            let (merged, trouble) =
                merge_lanes(&item, &mut run, &repo, &parent, &sp.lanes, &step.on_conflict, &on_change).await;
            let failed = done.iter().filter(|d| !d.outcome.ok).count();
            let body: Vec<String> = done.iter().map(|d| short(&d.outcome)).collect();
            let out = match trouble {
                Some(why) => Outcome {
                    ok: false,
                    output: format!("слияние не вышло — {why}"),
                    code: 1,
                    ..Default::default()
                },
                None => Outcome {
                    ok: failed == 0,
                    output: body.join("\n"),
                    code: failed as i32,
                    ..Default::default()
                },
            };
            let note = match (&out.ok, failed) {
                (true, _) => format!("для каждого: {} из {}, слито {}", done.len(), sp.lanes.len(), merged.len()),
                (false, 0) => out.output.clone(),
                (false, n) => format!("для каждого: сорвалось {n} из {}", done.len()),
            };
            run.vars.insert(sp.node.clone(), out.clone());
            push_iteration(&mut run, &step, &out, &sp.parent, note);
            match pipeline::next_step(&step, &out) {
                Some(to) => pending.push(TokenState {
                    node: to,
                    from: sp.from.clone(),
                    lane: sp.parent.clone(),
                }),
                None if !out.ok => {
                    incident.get_or_insert(format!(
                        "«{}» сорвался, а перехода на этот случай нет",
                        step.title()
                    ));
                }
                None => {}
            }
        }
        run.tokens_at = pending.clone();
        store.put_run(run.clone());
        on_change(&run);
        if let Some(why) = incident {
            finish(&store, &mut run, StopReason::Failed, why, &on_change);
            return;
        }
    }
}

/// Узел-заглушка для сообщений об ошибке — до него доходит только сломанный файл.
fn unreachable_step(id: &str) -> Step {
    Step::node(id, StepKind::Human { question: String::new() }, Vec::new())
}

/* ======================= ветвление и слияние ======================= */

/// Развести ход по веткам: каждой — свой worktree на своей ветке.
///
/// Ветка отпочковывается от ТЕКУЩЕГО состояния дорожки, а не от HEAD
/// репозитория: то, что сделали шаги до ветвления, ветки обязаны видеть.
/// Поэтому перед ветвлением работа дорожки коммитится — `git merge` сводит
/// коммиты, а незакоммиченные файлы для него не существуют.
async fn fork(
    item: &Loop,
    run: &mut Run,
    repo: &Path,
    t: &TokenState,
    step: &Step,
) -> Result<Vec<TokenState>, String> {
    let parent = lane_of(run, &t.lane).cloned().ok_or_else(|| format!("дорожка «{}» пропала", t.lane))?;
    let pdir = PathBuf::from(&parent.dir);
    runner::commit_all(&pdir, &format!("перед ветвлением «{}»", step.title())).await?;
    let base = runner::head_sha(&pdir)
        .await
        .ok_or_else(|| format!("{}: нет ни одного коммита — ветвиться не от чего", parent.dir))?;

    let mut born = Vec::new();
    for target in pipeline::all_steps(step) {
        let name = lane_name(&t.lane, &target);
        let branch = format!("{}-{}", run.branch, ref_slug(&name));
        let dir = lane_dir(&pdir, &target);
        runner::add_lane(repo, &dir, &branch, &base).await?;
        run.lanes.retain(|l| l.name != name);
        run.lanes.push(LaneState {
            name: name.clone(),
            parent: t.lane.clone(),
            dir: dir.to_string_lossy().into_owned(),
            branch,
        });
        // `from` тянем прежний, а не имя шлюза: по нему развилка смотрит
        // результат ПРЕДЫДУЩЕЙ РАБОТЫ, а у ветвления работы нет.
        born.push(TokenState { node: target, from: t.from.clone(), lane: name });
    }
    crate::log::line(&format!(
        "[loops] {}: ветвление «{}» → {} веток",
        item.name,
        step.title(),
        born.len()
    ));
    Ok(born)
}

/// Родительская дорожка слияния — та, в которую вливаются ветки.
fn parent_lane(run: &Run, join: &str) -> String {
    run.waiting_at
        .iter()
        .find(|w| w.node == join)
        .and_then(|w| lane_of(run, &w.lane))
        .map(|l| l.parent.clone())
        .unwrap_or_default()
}

/// Чем кончилось слияние: результат узла, заметка в журнал и дорожка, по
/// которой ход пойдёт дальше.
struct Joined {
    out: Outcome,
    note: String,
    lane: String,
}

fn join_failed(why: String, lane: String) -> Joined {
    Joined {
        out: Outcome { ok: false, output: why.clone(), code: 1, ..Default::default() },
        note: why,
        lane,
    }
}

/// Свести ветки обратно в родительскую дорожку.
///
/// Результат узла — не формальность: на нём стоят переходы, и «слияние не
/// вышло» человек вправе обработать в самом пайплайне (например, увести на шаг,
/// который позовёт агента), а не только инцидентом.
async fn join(
    item: &Loop,
    run: &mut Run,
    repo: &Path,
    node: &str,
    on_conflict: &str,
    on_change: &impl Fn(&Run),
) -> Joined {
    let arrived: Vec<TokenState> = run.waiting_at.iter().filter(|w| w.node == node).cloned().collect();
    // Родителя спрашиваем ДО того, как снимем ожидание: после снятия спрашивать
    // будет уже не у кого.
    let parent_name = parent_lane(run, node);
    run.waiting_at.retain(|w| w.node != node);
    let parent = match lane_of(run, &parent_name).cloned() {
        Some(l) => l,
        None => return join_failed("родительская дорожка слияния пропала".into(), parent_name),
    };
    let pdir = PathBuf::from(&parent.dir);
    if let Err(e) = runner::commit_all(&pdir, "перед слиянием").await {
        return join_failed(e, parent.name);
    }

    let names: Vec<String> = arrived.iter().map(|a| a.lane.clone()).collect();
    let (merged, trouble) =
        merge_lanes(item, run, repo, &parent, &names, on_conflict, on_change).await;

    match trouble {
        None => {
            let note = format!("слито веток: {} ({})", merged.len(), merged.join(", "));
            Joined {
                out: Outcome { ok: true, output: note.clone(), ..Default::default() },
                note,
                lane: parent.name,
            }
        }
        Some(why) => join_failed(format!("слияние не вышло — {why}"), parent.name),
    }
}

/// Свести перечисленные дорожки в родительскую.
///
/// Общая для слияния (`Join`) и для «для каждого»: сводить ветки в обоих
/// случаях надо одинаково, и два похожих цикла с чуть разной обработкой
/// конфликта однажды разошлись бы в поведении.
///
/// Возвращает имена влитых и причину, если на какой-то ветке споткнулись.
/// Рабочие места влитых убираются, ветки git остаются — в них работа.
async fn merge_lanes(
    item: &Loop,
    run: &mut Run,
    repo: &Path,
    parent: &LaneState,
    lanes: &[String],
    on_conflict: &str,
    on_change: &impl Fn(&Run),
) -> (Vec<String>, Option<String>) {
    let policy = pipeline::OnConflict::parse(on_conflict);
    let strategy = match policy {
        pipeline::OnConflict::Ours => Some("ours"),
        pipeline::OnConflict::Theirs => Some("theirs"),
        _ => None,
    };
    let pdir = PathBuf::from(&parent.dir);
    let mut merged: Vec<String> = Vec::new();
    let mut trouble: Option<String> = None;
    for name in lanes {
        if name == &parent.name {
            continue; // ветка, идущая по родительской дорожке, уже на месте
        }
        let Some(lane) = lane_of(run, name).cloned() else { continue };
        let ldir = PathBuf::from(&lane.dir);
        if let Err(e) = runner::commit_all(&ldir, &format!("ветка {}", lane.name)).await {
            trouble = Some(format!("{}: {e}", lane.name));
            break;
        }
        match runner::merge_lane(&pdir, &lane.branch, strategy).await {
            Merge::Done | Merge::Nothing => merged.push(lane.name.clone()),
            Merge::Failed(why) => {
                trouble = Some(format!("{}: {why}", lane.name));
                break;
            }
            Merge::Conflict(files) => {
                match resolve_conflict(item, &pdir, &lane, &files, policy, run, on_change).await {
                    Ok(()) => merged.push(lane.name.clone()),
                    Err(why) => {
                        runner::abort_merge(&pdir).await;
                        trouble = Some(why);
                        break;
                    }
                }
            }
        }
    }
    for name in &merged {
        if let Some(l) = lane_of(run, name).cloned() {
            runner::remove_lane(repo, Path::new(&l.dir)).await;
        }
    }
    run.lanes.retain(|l| !merged.contains(&l.name));
    (merged, trouble)
}

/// Развести узел «для каждого» по элементам списка: дорожка на элемент.
///
/// Отличие от `fork` одно, но существенное: веток здесь столько, сколько строк
/// в списке, и узнаём мы это только на ходу. Всё остальное то же самое —
/// коммит перед ветвлением и worktree от текущего состояния дорожки.
async fn spread(
    run: &mut Run,
    repo: &Path,
    t: &TokenState,
    step: &Step,
    items: &[String],
) -> Result<Spread, String> {
    let parent = lane_of(run, &t.lane)
        .cloned()
        .ok_or_else(|| format!("дорожка «{}» пропала", t.lane))?;
    let pdir = PathBuf::from(&parent.dir);
    runner::commit_all(&pdir, &format!("перед «{}»", step.title())).await?;
    let base = runner::head_sha(&pdir)
        .await
        .ok_or_else(|| format!("{}: нет ни одного коммита — ветвиться не от чего", parent.dir))?;

    let mut lanes = Vec::with_capacity(items.len());
    for (i, _) in items.iter().enumerate() {
        // Имя по НОМЕРУ, а не по элементу: элементом бывает путь с косыми и
        // пробелами, и делать из него имя ветки git — напрашиваться на беду.
        let name = lane_name(&t.lane, &format!("{}-{}", step.id, i + 1));
        let branch = format!("{}-{}", run.branch, ref_slug(&name));
        let dir = lane_dir(&pdir, &format!("{}-{}", step.id, i + 1));
        runner::add_lane(repo, &dir, &branch, &base).await?;
        run.lanes.retain(|l| l.name != name);
        run.lanes.push(LaneState {
            name: name.clone(),
            parent: t.lane.clone(),
            dir: dir.to_string_lossy().into_owned(),
            branch,
        });
        lanes.push(name);
    }
    Ok(Spread { node: step.id.clone(), from: t.from.clone(), parent: parent.name, lanes })
}

/// Итог шага в одну строку — для журнала экземпляра.
fn short(out: &Outcome) -> String {
    let text = crate::util::ellipsize(&crate::util::one_line(out.output.trim()), 100);
    if out.ok {
        if text.is_empty() { "готово".into() } else { text }
    } else {
        format!("сорвался · {text}")
    }
}

/// Разобрать конфликт по выбранному правилу.
///
/// `ours`/`theirs` сюда не доходят обычным путём — они уже отработали как `-X`
/// при самом слиянии; если конфликт остался и после них (добавили один файл с
/// двух сторон), выбирать всё равно нечего и это честный отказ.
async fn resolve_conflict(
    item: &Loop,
    pdir: &Path,
    lane: &LaneState,
    files: &[String],
    policy: pipeline::OnConflict,
    run: &Run,
    on_change: &impl Fn(&Run),
) -> Result<(), String> {
    let list = crate::util::ellipsize(&files.join(", "), 200);
    if policy != pipeline::OnConflict::Agent {
        return Err(format!("{}: конфликт в {list} (правило — {})", lane.name, policy.word()));
    }
    crate::log::line(&format!("[loops] {}: конфликт в {list} — отдаю агенту", item.name));
    let mut hint = run.clone();
    hint.stop_note = format!("разбираю конфликт слияния: {list}");
    on_change(&hint);

    let prompt = conflict_prompt(&lane.name, files);
    let out = runner::run_agent(&item.agent, pdir, &prompt, None, CONFLICT_TIMEOUT).await;
    if out.failed {
        return Err(format!("{}: агент не разобрал конфликт", lane.name));
    }
    // Верим не словам агента, а файлам: маркер, оставшийся в дереве, — это
    // сломанный исходник, и коммитить его нельзя ни при каких обещаниях.
    let (_, left) = runner::shell(
        pdir,
        "git diff --name-only --diff-filter=U; grep -rl '^<<<<<<< ' . --exclude-dir=.git || true",
        Duration::from_secs(120),
    )
    .await;
    if !left.trim().is_empty() {
        return Err(format!(
            "{}: маркеры конфликта остались в {}",
            lane.name,
            crate::util::ellipsize(&crate::util::one_line(left.trim()), 120)
        ));
    }
    runner::finish_merge(pdir, &format!("слияние ветки {} (конфликт разобрал агент)", lane.name))
        .await
        .map_err(|e| format!("{}: {e}", lane.name))
}

/* ======================= журнал ======================= */

fn push_iteration(run: &mut Run, step: &Step, out: &Outcome, lane: &str, note: String) {
    let n = run.iterations.len() as u32 + 1;
    let now = crate::util::now_ms();
    run.iterations.push(Iteration {
        n,
        started_at: now,
        ended_at: now,
        verdict: if out.ok { Verdict::Passed } else { Verdict::Failed },
        summary: if note.is_empty() { summarize(step, out) } else { format!("{}: {note}", step.title()) },
        step: step.id.clone(),
        lane: lane.to_string(),
        ..Default::default()
    });
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
    // Ходы снимаем только когда прогон действительно дошёл до конца. Упёрся в
    // ограничитель — они и есть то место, с которого «Продолжить» продолжит;
    // стереть их значит начать ночь заново.
    if reason == StopReason::Exit {
        run.tokens_at.clear();
        run.waiting_at.clear();
    }
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
    use super::super::pipeline::Flow;
    use super::*;

    fn step(id: &str, kind: StepKind, next: Vec<Flow>) -> Step {
        Step::node(id, kind, next)
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

    /// Разбор конфликта — единственное место, где агенту дают тронуть чужую
    /// работу. Промт обязан запрещать самое соблазнительное: выбрать сторону.
    #[test]
    fn the_conflict_prompt_forbids_picking_a_side() {
        let p = conflict_prompt("фронт", &["src/a.rs".into(), "src/b.rs".into()]);
        assert!(p.contains("src/a.rs") && p.contains("src/b.rs"));
        assert!(p.contains("Выбирать сторону целиком нельзя"));
        assert!(p.contains("<<<<<<<"), "маркеры надо назвать — их и убирать");
        assert!(p.contains("Ничего не коммить"), "коммит — наше дело, иначе слияние не закроется");
    }

    /// Имя дорожки читается как путь, потому что путь и есть: ветка внутри
    /// ветки — обычное дело для вложенной параллели.
    #[test]
    fn lane_names_read_as_paths() {
        assert_eq!(lane_name("", "фронт"), "фронт");
        assert_eq!(lane_name("фронт", "стили"), "фронт/стили");
    }

    /* ---------------- сквозная проверка параллели ----------------
     *
     * Настоящий git, настоящие worktree, настоящее слияние. Мокать здесь
     * нечего: вся суть фичи в том, что ветки РЕАЛЬНО работают в разных
     * каталогах и РЕАЛЬНО сводятся обратно, — а это либо так, либо нет.
     * Агента не зовём: узлы-команды проверяют ровно движок, не завися ни от
     * сети, ни от установленного claude. */

    async fn git(dir: &Path, cmd: &str) -> String {
        let (code, out) = runner::shell(dir, cmd, Duration::from_secs(60)).await;
        assert_eq!(code, 0, "git: {cmd}\n{out}");
        out
    }

    /// Репозиторий с одним коммитом + песочница запуска отдельным worktree.
    async fn sandbox(tag: &str) -> Option<(PathBuf, PathBuf)> {
        if runner::shell(Path::new("/"), "command -v git", Duration::from_secs(10)).await.0 != 0 {
            return None; // без git проверять нечего — и это не провал теста
        }
        let root = std::env::temp_dir().join(format!("jarvis-pipe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, "git init -q -b main").await;
        std::fs::write(repo.join("README"), "старт\n").unwrap();
        git(&repo, "git add -A && git -c user.name=t -c user.email=t@t commit -qm первый").await;
        let base = root.join("wt").join("прогон-1");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        runner::add_lane(&repo, &base, "loop/прогон-1", "HEAD").await.unwrap();
        Some((repo, base))
    }

    fn parallel_loop(repo: &Path, on_conflict: &str) -> (Loop, Pipeline) {
        let item = Loop {
            id: "t".into(),
            name: "прогон".into(),
            agent: "claude".into(),
            sandbox: Sandbox { repo: repo.to_string_lossy().into_owned(), branch: "loop/прогон-{n}".into(), worktree: true },
            limits: Limits { tokens: 0, iterations: 50, minutes: 30, stop_on_drift: false },
            ..Default::default()
        };
        let p = Pipeline {
            start: "разойтись".into(),
            steps: vec![
                step("разойтись", StepKind::Fork, vec![Flow::to("фронт", super::super::pipeline::Cond::Always), Flow::to("бэк", super::super::pipeline::Cond::Always)]),
                step("фронт", StepKind::Shell { command: "echo фронт > фронт.txt".into() }, vec![Flow::to("свести", super::super::pipeline::Cond::Always)]),
                step("бэк", StepKind::Shell { command: "echo бэк > бэк.txt".into() }, vec![Flow::to("свести", super::super::pipeline::Cond::Always)]),
                {
                    let mut j = step("свести", StepKind::Join, vec![Flow::to("", super::super::pipeline::Cond::Always)]);
                    j.on_conflict = on_conflict.into();
                    j
                },
            ],
            ..Default::default()
        };
        (item, p)
    }

    fn fresh_run(base: &Path) -> Run {
        Run {
            loop_id: "t".into(),
            n: 1,
            state: RunState::Running,
            started_at: crate::util::now_ms(),
            branch: "loop/прогон-1".into(),
            worktree: base.to_string_lossy().into_owned(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn branches_work_in_their_own_trees_and_merge_back() {
        let Some((repo, base)) = sandbox("par").await else { return };
        let store = Arc::new(Store::load_at(repo.parent().unwrap().join("state")));
        let (item, p) = parallel_loop(&repo, "stop");
        run_pipeline(store.clone(), item, p, fresh_run(&base), base.clone(), |_| {}).await;

        let run = store.run("t").expect("запуск не сохранён");
        assert_eq!(run.state, RunState::Done, "{}", run.stop_note);
        // Главное: обе ветки работали ПОРОЗНЬ, а результат оказался ВМЕСТЕ.
        assert!(base.join("фронт.txt").exists(), "правка первой ветки не влилась");
        assert!(base.join("бэк.txt").exists(), "правка второй ветки не влилась");
        // И работали они действительно в разных деревьях, а не по очереди в одном.
        let names: Vec<String> = run.iterations.iter().map(|i| i.lane.clone()).collect();
        assert!(names.contains(&"фронт".to_string()) && names.contains(&"бэк".to_string()), "{names:?}");
        // Рабочие места веток убраны, а ветки git — на месте: в них работа.
        assert!(!base.with_file_name("прогон-1-фронт").exists(), "worktree ветки остался висеть");
        let branches = git(&repo, "git branch --list").await;
        assert!(branches.contains("фронт"), "ветку удалили вместе с работой: {branches}");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// Конфликт без правила разбора — инцидент, а не молчаливый выбор стороны.
    /// Дерево при этом обязано остаться рабочим: прогон в незавершённом слиянии
    /// не смог бы больше ничего.
    #[tokio::test]
    async fn a_conflict_stops_the_run_and_leaves_a_working_tree() {
        let Some((repo, base)) = sandbox("conf").await else { return };
        let store = Arc::new(Store::load_at(repo.parent().unwrap().join("state")));
        let (item, mut p) = parallel_loop(&repo, "stop");
        // Обе ветки правят ОДНУ строку одного файла — гарантированный конфликт.
        p.steps[1].kind = StepKind::Shell { command: "echo фронт > README".into() };
        p.steps[2].kind = StepKind::Shell { command: "echo бэк > README".into() };
        run_pipeline(store.clone(), item, p, fresh_run(&base), base.clone(), |_| {}).await;

        let run = store.run("t").expect("запуск не сохранён");
        assert_eq!(run.state, RunState::Stopped);
        assert!(run.stop_note.contains("конфликт"), "{}", run.stop_note);
        assert!(run.stop_note.contains("README"), "надо назвать файл: {}", run.stop_note);
        let status = git(&base, "git status --porcelain").await;
        assert!(!status.contains("UU"), "дерево осталось в слиянии: {status}");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// «Взять чужую» — не «выбрать сторону целиком»: неконфликтующие правки
    /// второй ветки обязаны доехать всё равно.
    #[tokio::test]
    async fn a_conflict_rule_merges_the_rest_of_the_branch_anyway() {
        let Some((repo, base)) = sandbox("theirs").await else { return };
        let store = Arc::new(Store::load_at(repo.parent().unwrap().join("state")));
        let (item, mut p) = parallel_loop(&repo, "theirs");
        p.steps[1].kind = StepKind::Shell { command: "echo фронт > README".into() };
        p.steps[2].kind = StepKind::Shell { command: "echo бэк > README; echo ещё > бэк.txt".into() };
        run_pipeline(store.clone(), item, p, fresh_run(&base), base.clone(), |_| {}).await;

        let run = store.run("t").expect("запуск не сохранён");
        assert_eq!(run.state, RunState::Done, "{}", run.stop_note);
        assert!(base.join("бэк.txt").exists(), "неконфликтующая правка ветки пропала");
        let readme = std::fs::read_to_string(base.join("README")).unwrap();
        assert!(readme.contains("бэк"), "правило «взять чужую» не сработало: {readme}");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// Слияние, до которого дошла только одна ветка, — инцидент. Ждать вечно
    /// значит показывать работающий прогон, который ничего не делает.
    #[tokio::test]
    async fn a_join_that_never_gathers_everyone_becomes_an_incident() {
        let Some((repo, base)) = sandbox("stuck").await else { return };
        let store = Arc::new(Store::load_at(repo.parent().unwrap().join("state")));
        let (item, mut p) = parallel_loop(&repo, "stop");
        // Вторая ветка ведёт в слияние ТОЛЬКО при неудаче, а команда её
        // проходит — и уходит в конец. Слияние по-прежнему ждёт две ветки:
        // столько стрелок в него нарисовано.
        p.steps[2].next = vec![
            Flow::to("свести", super::super::pipeline::Cond::Fail),
            Flow::to("", super::super::pipeline::Cond::Always),
        ];
        run_pipeline(store.clone(), item, p, fresh_run(&base), base.clone(), |_| {}).await;

        let run = store.run("t").expect("запуск не сохранён");
        assert_eq!(run.state, RunState::Stopped);
        assert!(run.stop_note.contains("из 2 веток"), "{}", run.stop_note);
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// Ответ человеку продолжает прогон с того узла, где он встал, — и в
    /// параллельном прогоне соседние ветки не начинаются заново.
    #[tokio::test]
    async fn an_answer_continues_the_run_instead_of_restarting_it() {
        let Some((repo, base)) = sandbox("ask").await else { return };
        let store = Arc::new(Store::load_at(repo.parent().unwrap().join("state")));
        let (item, mut p) = parallel_loop(&repo, "stop");
        // В одной ветке — вопрос человеку. Он замораживает ВЕСЬ прогон.
        p.steps.push(step(
            "спросить",
            StepKind::Human { question: "продолжаем?".into() },
            vec![Flow::to("свести", super::super::pipeline::Cond::Always)],
        ));
        p.steps[1].next = vec![Flow::to("спросить", super::super::pipeline::Cond::Always)];

        run_pipeline(store.clone(), item.clone(), p.clone(), fresh_run(&base), base.clone(), |_| {}).await;
        let asked = store.run("t").expect("запуск не сохранён");
        assert_eq!(asked.state, RunState::Asking, "{}", asked.stop_note);
        assert_eq!(asked.ask.as_ref().unwrap().step, "спросить");
        // Ходы и рабочие места веток обязаны лежать в запуске: без них
        // продолжение начало бы всё сначала.
        assert!(!asked.tokens_at.is_empty(), "живые ходы не сохранены");
        assert!(asked.lanes.len() >= 2, "рабочие места веток не сохранены");
        let done_before = asked.iterations.len();

        // Отвечаем ровно так, как это делает панель.
        let mut resumed = asked.clone();
        resumed.state = RunState::Running;
        resumed.interventions.push("да, продолжай".into());
        run_pipeline(store.clone(), item, p, resumed, base.clone(), |_| {}).await;

        let run = store.run("t").expect("запуск не сохранён");
        assert_eq!(run.state, RunState::Done, "{}", run.stop_note);
        assert!(base.join("бэк.txt").exists(), "работа второй ветки пропала при возобновлении");
        // Журнал продолжился, а не начался заново.
        assert!(run.iterations.len() > done_before, "журнал начался заново");
        let firsts = run.iterations.iter().filter(|i| i.step == "фронт").count();
        assert_eq!(firsts, 1, "шаг ветки выполнился второй раз — прогон начался сначала");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// Главный сценарий «для каждого»: список известен только на ходу, и на
    /// каждый его элемент заводится своя ветка со своим рабочим деревом.
    #[tokio::test]
    async fn each_item_gets_its_own_branch_and_all_of_them_merge_back() {
        let Some((repo, base)) = sandbox("each").await else { return };
        let store = Arc::new(Store::load_at(repo.parent().unwrap().join("state")));
        let (item, _) = parallel_loop(&repo, "stop");
        let p = Pipeline {
            start: "список".into(),
            steps: vec![
                // Список получается на ходу — заранее его знать нельзя.
                step("список", StepKind::Shell { command: "printf 'альфа\\nбета\\nгамма\\n'".into() },
                     vec![Flow::to("править", super::super::pipeline::Cond::Always)]),
                {
                    let mut s = step("править",
                        StepKind::Shell { command: "echo ${элемент} > ${элемент}.txt".into() },
                        vec![Flow::to("", super::super::pipeline::Cond::Always)]);
                    s.over = "${список.вывод}".into();
                    s
                },
            ],
            ..Default::default()
        };
        run_pipeline(store.clone(), item, p, fresh_run(&base), base.clone(), |_| {}).await;

        let run = store.run("t").expect("запуск не сохранён");
        assert_eq!(run.state, RunState::Done, "{}", run.stop_note);
        // Каждый элемент сделал своё — и всё это оказалось в одном дереве.
        for name in ["альфа", "бета", "гамма"] {
            assert!(base.join(format!("{name}.txt")).exists(), "работа по «{name}» не влилась");
        }
        // Работали порознь: у каждого экземпляра своя дорожка в журнале.
        let lanes: std::collections::HashSet<String> = run
            .iterations
            .iter()
            .filter(|i| i.step == "править" && !i.lane.is_empty())
            .map(|i| i.lane.clone())
            .collect();
        assert_eq!(lanes.len(), 3, "экземпляры шли в одной ветке: {lanes:?}");
        // И рабочие места убраны за собой.
        assert!(!base.with_file_name("прогон-1-править-1").exists(), "worktree экземпляра остался");
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// Пустой список — не ошибка: чинить нечего. Но и не молчание: строка в
    /// журнале обязана быть, иначе шаг выглядит пропущенным без причины.
    #[tokio::test]
    async fn an_empty_list_skips_the_step_and_says_so() {
        let Some((repo, base)) = sandbox("empty").await else { return };
        let store = Arc::new(Store::load_at(repo.parent().unwrap().join("state")));
        let (item, _) = parallel_loop(&repo, "stop");
        let p = Pipeline {
            start: "список".into(),
            steps: vec![
                step("список", StepKind::Shell { command: "true".into() },
                     vec![Flow::to("править", super::super::pipeline::Cond::Always)]),
                {
                    let mut s = step("править",
                        StepKind::Shell { command: "echo ${элемент} > нет.txt".into() },
                        vec![Flow::to("", super::super::pipeline::Cond::Always)]);
                    s.over = "${список.вывод}".into();
                    s
                },
            ],
            ..Default::default()
        };
        run_pipeline(store.clone(), item, p, fresh_run(&base), base.clone(), |_| {}).await;

        let run = store.run("t").expect("запуск не сохранён");
        assert_eq!(run.state, RunState::Done, "{}", run.stop_note);
        assert!(!base.join("нет.txt").exists(), "шаг выполнился при пустом списке");
        assert!(
            run.iterations.iter().any(|i| i.summary.contains("список пуст")),
            "о пропуске не сказано ни слова: {:?}",
            run.iterations.iter().map(|i| i.summary.clone()).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    /// Ограничитель — это пауза, а не конец: «Продолжить» обязано продолжить
    /// с того места, где прогон встал, а не начать ночь заново.
    #[tokio::test]
    async fn a_limit_keeps_the_place_the_run_stopped_at() {
        let Some((repo, base)) = sandbox("limit").await else { return };
        let store = Arc::new(Store::load_at(repo.parent().unwrap().join("state")));
        let (mut item, p) = parallel_loop(&repo, "stop");
        item.limits.iterations = 1; // хватит ровно на одну ветку
        run_pipeline(store.clone(), item, p, fresh_run(&base), base.clone(), |_| {}).await;

        let run = store.run("t").expect("запуск не сохранён");
        assert_eq!(run.stop, StopReason::Iterations);
        assert!(
            !run.tokens_at.is_empty() || !run.waiting_at.is_empty(),
            "место остановки потеряно — продолжение начнёт всё сначала"
        );
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn a_lane_lives_next_to_its_parent_tree() {
        let base = Path::new("/tmp/wt/проект-7");
        let a = lane_dir(base, "фронт");
        let b = lane_dir(base, "бэк");
        assert_ne!(a, b, "две ветки в одном каталоге затрут друг друга");
        assert_eq!(a, PathBuf::from("/tmp/wt/проект-7-фронт"));
        // Вложенная параллель читается путём, а не превращается в кашу.
        assert_eq!(lane_dir(&a, "стили"), PathBuf::from("/tmp/wt/проект-7-фронт-стили"));
    }
}
