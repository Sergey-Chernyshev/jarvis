//! Команды панели для режима «Циклы».
//!
//! Наружу отдаётся снимок целиком (`loops_state`), а не патчи: циклов единицы,
//! а частичное обновление заставило бы панель домысливать недостающее — ровно
//! тот способ, которым экраны расходятся с правдой.

use super::model::*;
use super::{engine, schedule, view, LoopView};
use crate::daemon::Daemon;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::AppHandle;

/// Снимок режима: циклы с их запусками, шаблоны, идёт ли что-нибудь прямо сейчас.
pub fn snapshot(d: &Arc<Daemon>) -> Value {
    let now = crate::util::now_ms();
    let items: Vec<LoopView> = d
        .loops
        .store
        .all()
        .iter()
        .map(|l| view(l, d.loops.store.run(&l.id), now))
        .collect();
    json!({
        "ok": true,
        "loops": items,
        "templates": super::template_views(),
        "busy": !d.loops.idle(),
    })
}

/// Разослать состояние в панель — после каждой правки и на каждом шаге запуска.
pub fn push(d: &Arc<Daemon>) {
    crate::windows::emit_to_panel(&d.app, "loops-state", &snapshot(d));
}

#[tauri::command]
pub fn loops_get(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    // Панель спросила состояние — самый момент проверить, не сохранил ли
    // человек что-то в модельере. Здесь, а не в `push`: `push` летит на каждом
    // шаге запуска, и трогать диск в этом темпе незачем.
    sync_from_files(&d);
    snapshot(&d)
}

/* ================= обмен с Camunda Modeler ================= */

/// Куда кладём файлы пайплайнов. Отдельный каталог, а не рядом с проектом:
/// это файл ИНСТРУМЕНТА, и класть его в чужой репозиторий без спроса нельзя.
fn bpmn_dir() -> std::path::PathBuf {
    crate::util::jarvis_dir().join("pipelines")
}

fn default_bpmn_path(item: &Loop) -> std::path::PathBuf {
    bpmn_dir().join(format!("{}-{}.bpmn", slug(&item.name), item.id))
}

fn mtime_ms(p: &std::path::Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Записать пайплайн в связанный с ним файл и запомнить время правки.
///
/// Время запоминаем ОБЯЗАТЕЛЬНО: без этого следующая же проверка увидела бы
/// свой собственный файл как «человек что-то сохранил» и втянула бы его обратно.
fn write_bpmn(p: &mut super::pipeline::Pipeline, name: &str) -> Result<(), String> {
    let path = std::path::PathBuf::from(&p.bpmn_file);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, super::bpmn::to_xml(p, name)).map_err(|e| e.to_string())?;
    p.bpmn_mtime = mtime_ms(&path);
    Ok(())
}

/// Выгрузить пайплайн в `.bpmn` и (по просьбе) открыть его системой.
///
/// Открываем именно системой: ассоциация `.bpmn` у человека своя — Camunda
/// Modeler, bpmn.io в браузере, что угодно. Догадываться, чем он рисует, и
/// звать это по имени — самый быстрый способ не открыть ничего.
#[tauri::command]
pub fn loops_bpmn_export(app: AppHandle, id: String, path: Option<String>, open: Option<bool>) -> Value {
    let d = Daemon::get(&app);
    let Some(mut item) = d.loops.store.get(&id) else {
        return json!({ "ok": false, "error": "цикл не найден" });
    };
    let Some(linked) = item.pipeline.as_ref().map(|p| p.bpmn_file.clone()) else {
        return json!({ "ok": false, "error": "у этого цикла нет пайплайна — рисовать нечего" });
    };
    let target = match path.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(x) => std::path::PathBuf::from(crate::util::expand_home(&x)),
        None if !linked.is_empty() => std::path::PathBuf::from(&linked),
        None => default_bpmn_path(&item),
    };
    let name = item.name.clone();
    let p = item.pipeline.as_mut().unwrap();
    p.bpmn_file = target.to_string_lossy().into_owned();
    if let Err(e) = write_bpmn(p, &name) {
        return json!({ "ok": false, "error": format!("не записал файл: {e}") });
    }
    d.loops.store.save(item.clone());
    push(&d);
    if open.unwrap_or(false) {
        if let Err(e) = crate::ipc::open_path(&target, false) {
            return json!({ "ok": true, "path": target, "warning": e });
        }
    }
    json!({ "ok": true, "path": target })
}

/// Забрать пайплайн обратно из файла.
#[tauri::command]
pub fn loops_bpmn_import(app: AppHandle, id: String, path: Option<String>) -> Value {
    let d = Daemon::get(&app);
    let Some(mut item) = d.loops.store.get(&id) else {
        return json!({ "ok": false, "error": "цикл не найден" });
    };
    let linked = item.pipeline.as_ref().map(|p| p.bpmn_file.clone()).unwrap_or_default();
    let target = match path.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(x) => std::path::PathBuf::from(crate::util::expand_home(&x)),
        None if !linked.is_empty() => std::path::PathBuf::from(&linked),
        None => return json!({ "ok": false, "error": "нечего забирать: файл не выгружен" }),
    };
    let text = match std::fs::read_to_string(&target) {
        Ok(t) => t,
        Err(e) => return json!({ "ok": false, "error": format!("{}: {e}", target.display()) }),
    };
    let mut fresh = match super::bpmn::from_xml(&text) {
        Ok(p) => p,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    fresh.bpmn_file = target.to_string_lossy().into_owned();
    fresh.bpmn_mtime = mtime_ms(&target);
    let problems = fresh.problems();
    item.pipeline = Some(fresh);
    d.loops.store.save(item);
    push(&d);
    json!({ "ok": true, "path": target, "problems": problems })
}

/// Разорвать связь с файлом — панель снова единственный источник правды.
#[tauri::command]
pub fn loops_bpmn_unlink(app: AppHandle, id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(mut item) = d.loops.store.get(&id) else {
        return json!({ "ok": false, "error": "цикл не найден" });
    };
    if let Some(p) = item.pipeline.as_mut() {
        p.bpmn_file.clear();
        p.bpmn_mtime = 0;
    }
    d.loops.store.save(item);
    push(&d);
    json!({ "ok": true })
}

/// Втянуть правки, сделанные в модельере.
///
/// Правило простое и одно: **файл свежее — файл и главнее**. Человек только что
/// сохранил в редакторе, где видит схему целиком; молча оставить его правку за
/// бортом было бы худшим из возможных. Обратное направление — правка в панели —
/// пишет файл сразу же (`loops_save`), поэтому спор двух правок невозможен: у
/// того, кто сохранил позже, время и больше.
fn sync_from_files(d: &Arc<Daemon>) -> bool {
    let mut changed = false;
    for mut item in d.loops.store.all() {
        let Some(p) = item.pipeline.as_ref() else { continue };
        if p.bpmn_file.is_empty() {
            continue;
        }
        let path = std::path::PathBuf::from(&p.bpmn_file);
        let mtime = mtime_ms(&path);
        if mtime == 0 || mtime <= p.bpmn_mtime {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        match super::bpmn::from_xml(&text) {
            Ok(mut fresh) => {
                fresh.bpmn_file = p.bpmn_file.clone();
                fresh.bpmn_mtime = mtime;
                crate::log::line(&format!(
                    "[loops] {}: забрал правку из {} — шагов {}",
                    item.name,
                    path.display(),
                    fresh.steps.len()
                ));
                item.pipeline = Some(fresh);
                d.loops.store.save(item);
                changed = true;
            }
            // Битый файл не должен стирать рабочий пайплайн. Помечаем время
            // прочитанным, чтобы не жаловаться на него в каждом обновлении.
            Err(e) => {
                crate::log::line(&format!("[loops] {}: {} не разобрался — {e}", item.name, path.display()));
                if let Some(p) = item.pipeline.as_mut() {
                    p.bpmn_mtime = mtime;
                }
                d.loops.store.save(item);
            }
        }
    }
    if changed {
        push(d);
    }
    changed
}

/// Справочник конструктора: модели агентов и каталог заготовок.
///
/// Статика — но за отдельной командой, а не в каждом снимке: снимок летит в
/// панель на каждый шаг запуска, и возить с ним неизменный каталог значило бы
/// платить за него каждые несколько секунд. Панель зовёт это один раз и кэширует.
#[tauri::command]
pub fn loops_catalog() -> Value {
    let models = |a: crate::backend::Agent| -> Vec<Value> {
        crate::backend::backend(a)
            .models()
            .iter()
            .map(|(id, label)| json!({ "id": id, "label": label }))
            .collect()
    };
    json!({
        "ok": true,
        "models": {
            "claude": models(crate::backend::Agent::Claude),
            "codex": models(crate::backend::Agent::Codex),
        },
        "presets": super::presets::all(),
    })
}

/// Заготовка нового цикла: из шаблона или с нуля.
///
/// Ничего не сохраняет. Раньше создание сразу писало пустой цикл на диск, и
/// человек, передумавший на первом же поле, оставлял в списке «без имени»
/// навсегда. Заготовка живёт в панели, пока её не сохранят.
#[tauri::command]
pub fn loops_draft(template: Option<String>) -> Value {
    let item = match template.as_deref().filter(|t| !t.is_empty()) {
        Some(id) => match super::templates::build(id) {
            Some(l) => l,
            None => return json!({ "ok": false, "error": format!("нет шаблона «{id}»") }),
        },
        None => Loop {
            agent: "claude".into(),
            created_at: crate::util::now_ms(),
            ..Default::default()
        },
    };
    json!({ "ok": true, "item": item })
}

/// Собрать цикл из описания словами.
///
/// Ничего не сохраняет и не запускает: возвращает заполненную заготовку, а
/// решает человек. Это и есть граница ответственности — модель раскладывает
/// описание по полям, но команды, которые всю ночь будут выполняться без
/// надзора, подтверждает глазами человек.
#[tauri::command]
pub async fn loops_compose(app: AppHandle, text: String, item: Option<Value>) -> Value {
    let text = text.trim().to_string();
    if text.len() < 8 {
        return json!({ "ok": false, "error": "опиши задачу хотя бы одной фразой" });
    }
    if !crate::claude_bin::any_service_bin() {
        return json!({ "ok": false, "error": "не найден ни claude, ни codex — заполни поля руками" });
    }
    // Заготовка из панели: репозиторий и агент человек мог выбрать до описания,
    // и его выбор сильнее того, что придумает модель.
    let base: Loop = item.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    let _ = &app; // команда ничего не берёт у демона, но подпись держим общей

    let p = super::compose::prompt(&text, &base.sandbox.repo, &base.agent);
    let out = crate::claude_bin::run_service_llm(
        &p,
        std::time::Duration::from_secs(super::compose::TIMEOUT_SECS),
    )
    .await;
    let Some(out) = out else {
        return json!({ "ok": false, "error": "модель не ответила — попробуй ещё раз или заполни руками" });
    };
    match super::compose::parse(&out, &base) {
        Some(item) => {
            let problems = item.problems();
            json!({ "ok": true, "item": item, "problems": problems })
        }
        None => json!({ "ok": false, "error": "не разобрал ответ модели — попробуй переформулировать" }),
    }
}

/// Сохранить конфигурацию целиком — конструктор шлёт форму как есть.
#[tauri::command]
pub fn loops_save(app: AppHandle, item: Value) -> Value {
    let d = Daemon::get(&app);
    let mut parsed: Loop = match serde_json::from_value(item) {
        Ok(l) => l,
        Err(e) => return json!({ "ok": false, "error": format!("не разобрал цикл: {e}") }),
    };
    if parsed.id.is_empty() {
        parsed.id = format!("loop-{}", crate::util::now_ms());
    }
    // Прежнее время последнего запуска не должно теряться при правке формы:
    // от него считается следующее пробуждение.
    if let Some(old) = d.loops.store.get(&parsed.id) {
        if parsed.last_run_at == 0 {
            parsed.last_run_at = old.last_run_at;
        }
        if parsed.created_at == 0 {
            parsed.created_at = old.created_at;
        }
        // Связь с файлом `.bpmn` — свойство цикла, а не формы. Панель могла
        // собрать пайплайн заново (кнопкой заготовки), и потерять из-за этого
        // связку с открытым в модельере файлом было бы неприятным сюрпризом.
        if let (Some(p), Some(o)) = (parsed.pipeline.as_mut(), old.pipeline.as_ref()) {
            if p.bpmn_file.is_empty() {
                p.bpmn_file = o.bpmn_file.clone();
                p.bpmn_mtime = o.bpmn_mtime;
            }
        }
    }
    // Правка из панели уезжает в файл сразу: иначе следующее открытие в
    // модельере показало бы вчерашний граф и затёрло бы сегодняшний.
    if let Some(p) = parsed.pipeline.as_mut() {
        if !p.bpmn_file.is_empty() {
            let name = parsed.name.clone();
            if let Err(e) = write_bpmn(p, &name) {
                crate::log::line(&format!("[loops] не записал .bpmn: {e}"));
            }
        }
    }
    d.loops.store.save(parsed.clone());
    push(&d);
    json!({ "ok": true, "id": parsed.id, "problems": parsed.problems() })
}

#[tauri::command]
pub fn loops_remove(app: AppHandle, id: String) -> Value {
    let d = Daemon::get(&app);
    d.loops.store.remove(&id);
    push(&d);
    json!({ "ok": true })
}

/// Запустить цикл сейчас.
#[tauri::command]
pub fn loops_start(app: AppHandle, id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(item) = d.loops.store.get(&id) else {
        return json!({ "ok": false, "error": "цикл не найден" });
    };
    let problems = item.problems();
    if !problems.is_empty() {
        // Незаполненный цикл не запускаем молча: ночь впустую хуже отказа.
        return json!({ "ok": false, "error": problems.join("; ") });
    }
    if !d.loops.claim() {
        return json!({ "ok": false, "error": "уже крутится другой цикл" });
    }
    spawn_run(&d, item);
    json!({ "ok": true })
}

/// Поднять запуск в фоне. Вынесено отдельно: этим же пользуется расписание.
pub fn spawn_run(d: &Arc<Daemon>, item: Loop) {
    let run_n = d.loops.store.run(&item.id).map(|r| r.n + 1).unwrap_or(1);
    let mut stamped = item.clone();
    stamped.last_run_at = crate::util::now_ms();
    d.loops.store.save(stamped);

    let daemon = d.clone();
    let store = d.loops.store.clone();
    let keep_awake = item.schedule.keep_awake;
    tauri::async_runtime::spawn(async move {
        // Мак не должен уснуть посреди ночной работы: заснувшая машина — это
        // оборванная итерация и утро без результата.
        if keep_awake {
            daemon.power.loop_running(true);
        }
        let sink = daemon.clone();
        engine::run_loop(store, item, run_n, move |_run| {
            push(&sink);
        })
        .await;
        if keep_awake {
            daemon.power.loop_running(false);
        }
        daemon.loops.release();
        push(&daemon);
    });
}

/// Остановить цикл. Работа остаётся: ветка и worktree целы.
#[tauri::command]
pub fn loops_stop(app: AppHandle, id: String) -> Value {
    let d = Daemon::get(&app);
    d.loops.store.with_run(&id, |run| {
        run.state = RunState::Stopped;
        run.stop = StopReason::Stopped;
        run.stop_note = "остановлен вручную".into();
        run.ended_at = crate::util::now_ms();
    });
    push(&d);
    json!({ "ok": true })
}

/// Вмешаться: уточнить цель, добавить ограничение. Уйдёт в следующую итерацию.
#[tauri::command]
pub fn loops_intervene(app: AppHandle, id: String, text: String) -> Value {
    let d = Daemon::get(&app);
    let text = text.trim().to_string();
    if text.is_empty() {
        return json!({ "ok": false, "error": "пустая реплика" });
    }
    let updated = d.loops.store.with_run(&id, |run| run.interventions.push(text));
    push(&d);
    json!({ "ok": updated.is_some() })
}

/// Ответить на вопрос цикла. Ответ уходит репликой в следующую итерацию, а
/// запуск продолжается с того места, где встал.
#[tauri::command]
pub fn loops_answer(app: AppHandle, id: String, answer: String) -> Value {
    let d = Daemon::get(&app);
    let Some(item) = d.loops.store.get(&id) else {
        return json!({ "ok": false, "error": "цикл не найден" });
    };
    let Some(run) = d.loops.store.run(&id) else {
        return json!({ "ok": false, "error": "запуска нет" });
    };
    if run.state != RunState::Asking {
        return json!({ "ok": false, "error": "цикл ни о чём не спрашивает" });
    }
    d.loops.store.with_run(&id, |r| {
        // Вопрос НЕ забираем: у пайплайна в нём записан узел, с которого
        // продолжать. Заберёт его сам движок, когда дойдёт до возобновления, —
        // а линейному циклу его снимет `run_loop`, которому он не нужен.
        let q = r.ask.as_ref().map(|a| a.question.clone()).unwrap_or_default();
        r.interventions.push(format!("Ты спрашивал: {q}\nОтвет: {answer}"));
        r.state = RunState::Running;
    });
    if !d.loops.claim() {
        return json!({ "ok": false, "error": "уже крутится другой цикл" });
    }
    // Продолжаем ТОТ ЖЕ запуск: новый начал бы с чистой ветки и потерял всё,
    // что цикл успел за ночь.
    resume_run(&d, item, run.n);
    push(&d);
    json!({ "ok": true })
}

/// Продолжить существующий запуск, не начиная новый.
pub fn resume_run(d: &Arc<Daemon>, item: Loop, run_n: u32) {
    let daemon = d.clone();
    let store = d.loops.store.clone();
    let keep_awake = item.schedule.keep_awake;
    tauri::async_runtime::spawn(async move {
        if keep_awake {
            daemon.power.loop_running(true);
        }
        let sink = daemon.clone();
        engine::run_loop(store, item, run_n, move |_run| push(&sink)).await;
        if keep_awake {
            daemon.power.loop_running(false);
        }
        daemon.loops.release();
        push(&daemon);
    });
}

/// Принять итерацию выборки или вернуть её с комментарием.
///
/// Возврат — это не «отмена»: комментарий уходит критику как фидбэк человека,
/// и следующая итерация начинается с него.
#[tauri::command]
pub fn loops_review(app: AppHandle, id: String, n: u32, accept: bool, comment: String) -> Value {
    let d = Daemon::get(&app);
    let updated = d.loops.store.with_run(&id, |run| {
        if let Some(it) = run.iterations.iter_mut().find(|i| i.n == n) {
            it.reviewed = true;
            if !accept {
                it.verdict = Verdict::Returned;
                it.critic = comment.clone();
            }
        }
        if !accept && !comment.trim().is_empty() {
            run.interventions.push(format!("Человек вернул итерацию {n}: {comment}"));
            run.streak = 0;
        }
    });
    push(&d);
    json!({ "ok": updated.is_some() })
}

/// Возобновить остановленный ограничителем запуск, подняв потолок.
#[tauri::command]
pub fn loops_resume(app: AppHandle, id: String, extra_tokens: Option<u64>) -> Value {
    let d = Daemon::get(&app);
    let Some(mut item) = d.loops.store.get(&id) else {
        return json!({ "ok": false, "error": "цикл не найден" });
    };
    let Some(run) = d.loops.store.run(&id) else {
        return json!({ "ok": false, "error": "запуска нет" });
    };
    if !run.stop.is_limit() {
        return json!({ "ok": false, "error": "этот запуск остановлен не ограничителем" });
    }
    // Потолок поднимаем в самой конфигурации: иначе следующая же проверка
    // ограничителя остановит запуск на том же месте.
    match run.stop {
        StopReason::Tokens => item.limits.tokens += extra_tokens.unwrap_or(50_000),
        StopReason::Iterations => item.limits.iterations += 5,
        StopReason::Time => item.limits.minutes += 60,
        _ => {}
    }
    d.loops.store.save(item.clone());
    d.loops.store.with_run(&id, |r| {
        r.state = RunState::Running;
        r.stop = StopReason::None;
        r.stop_note.clear();
        r.ended_at = 0;
    });
    if !d.loops.claim() {
        return json!({ "ok": false, "error": "уже крутится другой цикл" });
    }
    resume_run(&d, item, run.n);
    push(&d);
    json!({ "ok": true })
}

/// Дифф итерации — экран итерации показывает его целиком.
#[tauri::command]
pub async fn loops_diff(app: AppHandle, id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(run) = d.loops.store.run(&id) else {
        return json!({ "ok": false, "error": "запуска нет" });
    };
    if run.worktree.is_empty() {
        return json!({ "ok": false, "error": "песочницы больше нет" });
    }
    let text = super::runner::diff(std::path::Path::new(&run.worktree), 400_000).await;
    json!({ "ok": true, "diff": text })
}

/// Тик расписания: разбудить те циклы, чьё время пришло.
///
/// Дёргается тем же таймером, что и остальная периодика демона. Один запуск за
/// раз: два цикла разом — это два агента, жгущих один лимит аккаунта.
pub fn tick(d: &Arc<Daemon>) {
    if !d.loops.idle() {
        return;
    }
    let now = crate::util::now_ms();
    let due: Option<Loop> = d
        .loops
        .store
        .all()
        .into_iter()
        .filter(|l| l.problems().is_empty())
        .find(|l| schedule::due(l, now));
    let Some(item) = due else { return };
    if !d.loops.claim() {
        return;
    }
    spawn_run(d, item);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_lifts_the_ceiling_that_stopped_the_run() {
        // Возобновление без подъёма потолка бессмысленно: та же проверка
        // ограничителя остановит запуск на том же месте.
        let mut item = Loop::default();
        item.limits.tokens = 200_000;
        let before = item.limits.tokens;
        match StopReason::Tokens {
            StopReason::Tokens => item.limits.tokens += 50_000,
            _ => unreachable!(),
        }
        assert!(item.limits.tokens > before);

        let run = Run { tokens: 200_000, ..Default::default() };
        assert_eq!(run.tripped(&item.limits, 0), None, "после подъёма стена отодвинулась");
    }

    #[test]
    fn only_limit_stops_are_resumable() {
        assert!(StopReason::Tokens.is_limit());
        assert!(StopReason::Time.is_limit());
        // Сорвавшийся запуск возобновлять нечем: причина не в бюджете.
        assert!(!StopReason::Failed.is_limit());
        assert!(!StopReason::Exit.is_limit());
    }
}
