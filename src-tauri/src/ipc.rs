//! IPC-команды панели и тостов — контракт window.jarvis / window.toast.
//!
//! Имена и формы ответов повторяют Electron-каналы один в один (':' → '_'):
//! рендерер не знает, что под мостом сменился рантайм. Формы ошибок — тоже:
//! { ok:false, error } / { ok:false, needsTmux, resumeCmd }.

use serde_json::{json, Map, Value};
use std::sync::Arc;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use crate::daemon::Daemon;
use crate::model::Status;
use crate::util::*;
use crate::{limits, tmux, transcript, windows};

fn ok() -> Value {
    json!({ "ok": true })
}

fn err(msg: impl Into<String>) -> Value {
    json!({ "ok": false, "error": msg.into() })
}

/// Вне tmux мы не вставляем текст — сессией нельзя управлять, пока она не в
/// tmux. Подсказываем команду: shim завернёт `claude --resume` в наш сервер.
fn tmux_needed(session_id: &str) -> Value {
    json!({ "ok": false, "needsTmux": true, "resumeCmd": format!("claude --resume {session_id}") })
}

/* ================= состояние и панель ================= */

#[tauri::command]
pub fn state_get(app: AppHandle) -> Value {
    serde_json::to_value(Daemon::get(&app).snapshot()).unwrap_or_else(|_| json!([]))
}

#[tauri::command]
pub fn state_clear(app: AppHandle) {
    let d = Daemon::get(&app);
    d.sessions
        .lock()
        .unwrap()
        .retain(|_, s| !matches!(s.status, Status::Done | Status::Idle));
    d.push();
}

#[tauri::command]
pub fn panel_hide(app: AppHandle) {
    windows::hide_panel(&Daemon::get(&app));
}

/* ================= настройки ================= */

#[tauri::command]
pub fn settings_get(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let mut s = d.settings.load();
    if let Some(obj) = s.as_object_mut() {
        obj.insert(
            "openAtLogin".into(),
            json!(app.autolaunch().is_enabled().unwrap_or(false)),
        );
    }
    s
}

/// Регистрация глобального хоткея с откатом на прежний при провале.
pub fn register_hotkey(d: &Arc<Daemon>, accelerator: &str) -> Result<(), String> {
    let gs = d.app.global_shortcut();
    let current = d.settings.string("hotkey");
    if accelerator == current && gs.is_registered(accelerator) {
        return Ok(());
    }
    if accelerator != current {
        let _ = gs.unregister(current.as_str());
    }
    if gs.register(accelerator).is_err() {
        if accelerator != current {
            let _ = gs.register(current.as_str());
        }
        return Err(format!("Сочетание {accelerator} занято системой"));
    }
    Ok(())
}

#[tauri::command]
pub fn settings_set(app: AppHandle, patch: Value) -> Value {
    let d = Daemon::get(&app);
    let Some(patch) = patch.as_object() else { return err("bad patch") };
    let mut rest = patch.clone();

    if let Some(Value::Bool(open)) = rest.remove("openAtLogin") {
        let autolaunch = app.autolaunch();
        let _ = if open { autolaunch.enable() } else { autolaunch.disable() };
    }

    if let Some(hotkey) = rest.remove("hotkey") {
        if let Some(hk) = hotkey.as_str().filter(|s| !s.is_empty()) {
            if let Err(e) = register_hotkey(&d, hk) {
                return err(e);
            }
            let mut m = Map::new();
            m.insert("hotkey".into(), json!(hk));
            d.settings.save(m);
        }
    }

    if !rest.is_empty() {
        d.settings.save(rest);
    }
    if windows::panel_visible(&d) {
        windows::position_panel(&d); // позиция могла смениться
    }
    ok()
}

/* ================= чат сессии ================= */

#[tauri::command]
pub fn chat_open(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else { return err("Сессия не найдена") };
    let Some(tr) = s.transcript else {
        return err("Нет транскрипта — сессия ещё не слала событий (перезапусти claude)");
    };
    let items: Vec<transcript::ChatItem> = transcript::chain_from_entries(
        transcript::read_recent_entries(std::path::Path::new(&tr), 512 * 1024),
    )
    .iter()
    .flat_map(transcript::to_chat_items)
    .collect();
    let tail_start = items.len().saturating_sub(80);
    let items = &items[tail_start..];
    d.tail.start(app.clone(), session_id.clone(), tr.clone());
    println!(
        "[jarvis] chat:open {} items={} file={}",
        ellipsize(&session_id, 8),
        items.len(),
        short_home(&tr)
    );
    json!({ "ok": true, "items": items, "project": s.project })
}

#[tauri::command]
pub fn chat_close(app: AppHandle) {
    Daemon::get(&app).tail.stop();
}

#[tauri::command]
pub fn commands_get(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    let cwd = d.session(&session_id).and_then(|s| s.cwd);
    serde_json::to_value(d.commands.get_for_cwd(cwd.as_deref())).unwrap_or_else(|_| json!([]))
}

#[tauri::command]
pub fn app_meta(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    json!({ "effortLevels": *d.effort_levels.lock().unwrap() })
}

/* ================= плагины, usage, история ================= */

#[tauri::command]
pub fn plugins_status(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    d.power.statuses(&d)
}

#[tauri::command]
pub async fn plugins_cmd(app: AppHandle, id: String, cmd: String, args: Option<Value>) -> Value {
    let d = Daemon::get(&app);
    crate::power::Power::cmd(&d, &id, &cmd, &args.unwrap_or(json!({}))).await
}

#[tauri::command]
pub fn usage_summary(app: AppHandle, period: Option<String>) -> Value {
    Daemon::get(&app).usage.stats(period.as_deref().unwrap_or("today"))
}

#[tauri::command]
pub fn limit_get(app: AppHandle) -> Value {
    serde_json::to_value(Daemon::get(&app).limits.state()).unwrap_or(Value::Null)
}

#[tauri::command]
pub fn history_get(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    d.history.projects(&d.usage)
}

#[tauri::command]
pub fn usage_session(app: AppHandle, id: String) -> Value {
    Daemon::get(&app).usage.for_session(&id).unwrap_or(Value::Null)
}

/* ================= управление сессией ================= */

#[tauri::command]
pub fn session_set_pin(app: AppHandle, session_id: String, pinned: bool) -> Value {
    let d = Daemon::get(&app);
    let found = d.with_session(&session_id, |s| s.pinned = pinned);
    if found {
        d.push();
    }
    json!({ "ok": found })
}

/// Пульт: слэш-команда с аргументом в живую пану + оптимистичное состояние.
async fn set_via_slash(
    d: &Arc<Daemon>,
    session_id: &str,
    slash: String,
    apply: impl FnOnce(&mut crate::model::Session),
) -> Value {
    let Some(s) = d.session(session_id) else { return err("Сессия не найдена") };
    let Some(pane) = s.tmux_pane else { return tmux_needed(session_id) };
    if !tmux::pane_alive(&pane).await {
        return tmux_needed(session_id);
    }
    match tmux::paste_slash(&pane, &slash).await {
        Ok(()) => {
            d.with_session(session_id, apply);
            d.push();
            ok()
        }
        Err(e) => err(ellipsize(&one_line(&e), 100)),
    }
}

#[tauri::command]
pub async fn session_set_model(app: AppHandle, session_id: String, model: String) -> Value {
    let d = Daemon::get(&app);
    let friendly = friendly_model(&model);
    set_via_slash(&d, &session_id, format!("/model {model}"), move |s| {
        s.model = Some(friendly); // оптимистично; транскрипт подтвердит
        s.model_at = Some(now_ms());
    })
    .await
}

#[tauri::command]
pub async fn session_set_effort(app: AppHandle, session_id: String, level: String) -> Value {
    let d = Daemon::get(&app);
    let lv = level.clone();
    set_via_slash(&d, &session_id, format!("/effort {level}"), move |s| {
        s.effort = Some(lv); // effort снаружи не читается — ведём оптимистично
    })
    .await
}

/// «Где это?» — секундный оверлей прямо в терминале сессии, фокус не воруем.
#[tauri::command]
pub async fn terminal_ping(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else { return err("Сессия не найдена") };
    let Some(pane) = s.tmux_pane else { return err("Сессия не в tmux — пингануть нечем") };
    match tmux::ping(&pane).await {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

/// Ответ на AskUserQuestion клавишами в пану.
#[tauri::command]
pub async fn question_answer(app: AppHandle, session_id: String, choice: Value) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else { return err("Вопрос уже неактуален") };
    let Some(q) = s.question.clone() else { return err("Вопрос уже неактуален") };
    let Some(pane) = s.tmux_pane else { return err("Сессия вне tmux — ответь в терминале") };
    if !tmux::pane_alive(&pane).await {
        return err("Пана сессии не отвечает");
    }
    let indices: Vec<u32> = choice
        .get("indices")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_u64)
                .filter(|&n| (1..=9).contains(&n))
                .map(|n| n as u32)
                .collect()
        })
        .unwrap_or_default();
    if indices.is_empty() {
        return err("Пустой выбор");
    }
    let multi = choice.get("multiSelect").and_then(Value::as_bool).unwrap_or(false);
    match tmux::answer_question(&pane, &indices, multi).await {
        Ok(()) => {
            // у хук-вопроса карточку закроет post-tool; у экранного — событий
            // нет, снимаем сами (детектор подтвердит по idle-экрану)
            if q.from_screen {
                d.with_session(&session_id, |s| {
                    s.question = None;
                    s.status = Status::Working;
                    s.updated_at = now_ms();
                });
                d.push();
            }
            ok()
        }
        Err(e) => err(ellipsize(&one_line(&e), 100)),
    }
}

/// Ответ в сессию: tmux-вставка в пану нашего сервера (-L jarvis).
#[tauri::command]
pub async fn session_reply(app: AppHandle, session_id: String, text: String) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else { return err("Сессия не найдена") };
    let prompt = text.trim().to_string();
    if prompt.is_empty() {
        return err("Пустой текст");
    }

    if let Some(pane) = s.tmux_pane {
        if tmux::pane_alive(&pane).await {
            return match tmux::reply(&pane, &prompt).await {
                Ok(()) => {
                    d.mark_prompt_sent(&session_id, &prompt);
                    println!("[jarvis] reply→tmux {pane} ({})", s.project.as_deref().unwrap_or("?"));
                    json!({ "ok": true, "channel": "tmux" })
                }
                Err(e) => {
                    eprintln!("[jarvis] reply tmux fail: {e}");
                    err(format!("tmux: {}", ellipsize(&one_line(&e), 120)))
                }
            };
        }
        d.with_session(&session_id, |s| s.tmux_pane = None); // пана умерла
        d.push();
    }
    tmux_needed(&session_id)
}

/// Лесенка «показать терминал»: tmux → вкладка по tty (Terminal/iTerm2) →
/// GUI-приложение-владелец. Нижняя ступень — не тост, а чат сессии в панели:
/// renderer открывает его сам при ok:false + fallbackChat.
#[tauri::command]
pub async fn terminal_focus(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else { return err("Сессия не найдена") };

    // 1) tmux — точнее некуда
    if let Some(pane) = &s.tmux_pane {
        if tmux::focus(pane).await {
            return ok();
        }
    }
    // 2) скриптуемые терминалы: точный фокус вкладки по tty
    if let Some(tty) = &s.tty {
        if crate::terminal::focus_terminal_by_tty(&format!("/dev/{tty}")).await {
            return ok();
        }
    }
    // 3) GUI-приложение, в котором живёт терминал (JediTerm и прочие без API)
    if let Some(name) = &s.app {
        if crate::terminal::activate_app_by_name(name).await {
            return json!({ "ok": true, "app": name });
        }
    }
    if let Some(pid) = s.pid {
        if let Some(gui) = crate::terminal::gui_ancestor_app(pid).await {
            if crate::terminal::activate_app_by_pid(gui.pid).await {
                return json!({ "ok": true, "app": gui.name });
            }
        }
    }
    json!({ "ok": false, "error": "Терминал не нашёлся — открываю чат", "fallbackChat": true })
}

/* ================= тосты ================= */

#[tauri::command]
pub fn toast_resize(app: AppHandle, h: f64) {
    windows::toast_resize(&Daemon::get(&app), h);
}

/// Мост окна тостов загрузился — можно доливать буфер ранних уведомлений.
#[tauri::command]
pub fn toast_ready(app: AppHandle) {
    windows::toast_flush(&Daemon::get(&app));
}

/// Клик по тосту: панель с фокусом + открыть чат сессии.
#[tauri::command]
pub fn toast_click(app: AppHandle, session_id: Option<String>) {
    let d = Daemon::get(&app);
    windows::show_panel_focused(&d);
    if let Some(sid) = session_id {
        windows::emit_to_panel(&d.app, "open-session", &sid);
    }
}

/* ================= служебное ================= */

/// Снять ложный лимит-баннер по официальному usage (таймер из main).
pub fn reconcile_limit(d: &Arc<Daemon>) {
    limits::reconcile(d);
}
