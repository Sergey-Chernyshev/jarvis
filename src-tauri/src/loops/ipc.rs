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
    snapshot(&Daemon::get(&app))
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
        let q = r.ask.take().map(|a| a.question).unwrap_or_default();
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
