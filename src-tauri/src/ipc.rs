//! IPC-команды панели и тостов — контракт window.jarvis / window.toast.
//!
//! Имена и формы ответов повторяют Electron-каналы один в один (':' → '_'):
//! рендерер не знает, что под мостом сменился рантайм. Формы ошибок — тоже:
//! { ok:false, error } / { ok:false, needsTmux, resumeCmd }.

use serde_json::{json, Value};
use std::sync::Arc;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

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

/// Аккселератор тумблера тихого режима: настройка `quietHotkey`, дефолт ⌘⌥J.
pub fn quiet_accelerator(d: &Arc<Daemon>) -> String {
    let s = d.settings.string("quietHotkey");
    if s.is_empty() { "Command+Alt+J".to_string() } else { s }
}

/// Совпал ли сработавший shortcut с хоткеем тихого режима.
pub fn is_quiet_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> bool {
    quiet_accelerator(d)
        .parse::<Shortcut>()
        .map(|s| &s == shortcut)
        .unwrap_or(false)
}

/// Зарегистрировать хоткей тихого режима на старте (best-effort).
pub fn register_quiet_hotkey(d: &Arc<Daemon>) {
    let accel = quiet_accelerator(d);
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        let _ = gs.register(accel.as_str());
    }
}

/// Аккселератор «Продолжить»: настройка `continueHotkey`, дефолт ⌘⌥C.
pub fn continue_accelerator(d: &Arc<Daemon>) -> String {
    let s = d.settings.string("continueHotkey");
    if s.is_empty() { "Command+Alt+C".to_string() } else { s }
}

pub fn is_continue_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> bool {
    continue_accelerator(d)
        .parse::<Shortcut>()
        .map(|s| &s == shortcut)
        .unwrap_or(false)
}

pub fn register_continue_hotkey(d: &Arc<Daemon>) {
    let accel = continue_accelerator(d);
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        let _ = gs.register(accel.as_str());
    }
}

/// Аккселератор диктовки: из `SttConfig.hotkey`, дефолт "F8".
pub fn dictation_accelerator(d: &Arc<Daemon>) -> String {
    let cfg = crate::stt::config::SttConfig::from_settings(&d.settings.load());
    if cfg.hotkey.is_empty() { "F8".to_string() } else { cfg.hotkey }
}

/// Совпал ли сработавший shortcut с хоткеем диктовки.
pub fn is_dictation_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> bool {
    dictation_accelerator(d)
        .parse::<Shortcut>()
        .map(|s| &s == shortcut)
        .unwrap_or(false)
}

/// Зарегистрировать хоткей диктовки на старте (best-effort).
pub fn register_dictation_hotkey(d: &Arc<Daemon>) {
    let accel = dictation_accelerator(d);
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        if let Err(e) = gs.register(accel.as_str()) {
            crate::log::line(&format!("[dictation] хоткей {accel} не зарегистрировался: {e:?}"));
        }
    }
}

/// Аккселератор «повторить уведомление»: настройка `repeatHotkey`, дефолт ⌘⌥R.
pub fn repeat_accelerator(d: &Arc<Daemon>) -> String {
    let s = d.settings.string("repeatHotkey");
    if s.is_empty() { "Command+Alt+R".to_string() } else { s }
}

pub fn is_repeat_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> bool {
    repeat_accelerator(d).parse::<Shortcut>().map(|s| &s == shortcut).unwrap_or(false)
}

pub fn register_repeat_hotkey(d: &Arc<Daemon>) {
    let accel = repeat_accelerator(d);
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        let _ = gs.register(accel.as_str());
    }
}

/// Аккселератор «без звука» (mute): настройка `muteHotkey`, дефолт ⌘⌥M.
pub fn mute_accelerator(d: &Arc<Daemon>) -> String {
    let s = d.settings.string("muteHotkey");
    if s.is_empty() { "Command+Alt+M".to_string() } else { s }
}

pub fn is_mute_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> bool {
    mute_accelerator(d).parse::<Shortcut>().map(|s| &s == shortcut).unwrap_or(false)
}

pub fn register_mute_hotkey(d: &Arc<Daemon>) {
    let accel = mute_accelerator(d);
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        let _ = gs.register(accel.as_str());
    }
}

/// Выбор варианта вопроса: ⌘⌥1 … ⌘⌥9. Регистрируем ДИНАМИЧЕСКИ — только пока
/// есть активный вопрос (зовётся из do_push), чтобы не перехватывать ⌘⌥-цифры
/// глобально всё время. Идемпотентно: трогаем только при смене состояния.
pub fn set_select_hotkeys(d: &Arc<Daemon>, on: bool) {
    let gs = d.app.global_shortcut();
    let mut touched = 0;
    let mut failed = 0;
    for n in 1..=9 {
        let accel = format!("Command+Alt+{n}");
        let reg = gs.is_registered(accel.as_str());
        if on && !reg {
            touched += 1;
            if gs.register(accel.as_str()).is_err() {
                failed += 1;
            }
        } else if !on && reg {
            touched += 1;
            let _ = gs.unregister(accel.as_str());
        }
    }
    if touched > 0 {
        crate::log::line(&format!(
            "[select] ⌘⌥1-9 {}{}",
            if on { "включены (вопрос активен)" } else { "сняты" },
            if failed > 0 { format!(", провал: {failed}") } else { String::new() },
        ));
    }
}

/// Если shortcut — это ⌘⌥<цифра>, вернуть номер варианта (1..9).
pub fn is_select_hotkey(shortcut: &Shortcut) -> Option<u32> {
    (1..=9).find(|n| {
        format!("Command+Alt+{n}").parse::<Shortcut>().map(|s| &s == shortcut).unwrap_or(false)
    })
}

#[tauri::command]
pub async fn settings_set(app: AppHandle, patch: Value) -> Value {
    let d = Daemon::get(&app);
    let Some(patch) = patch.as_object() else { return err("bad patch") };
    let mut rest = patch.clone();

    if let Some(Value::Bool(open)) = rest.remove("openAtLogin") {
        let autolaunch = app.autolaunch();
        let res = if open { autolaunch.enable() } else { autolaunch.disable() };
        if let Err(e) = res {
            // не глотаем: видно в консоли `npm run start`, а UI перечитает
            // реальное is_enabled() и честно покажет, что не сработало
            eprintln!(
                "[jarvis:autostart] не смог {} автозапуск: {e}",
                if open { "включить" } else { "выключить" }
            );
        }
    }

    if let Some(hotkey) = rest.remove("hotkey") {
        if let Some(hk) = hotkey.as_str().filter(|s| !s.is_empty()) {
            if let Err(e) = register_hotkey(&d, hk) {
                return err(e);
            }
            let _ = via_gate_panel(&d, "settings.set", json!({ "patch": { "hotkey": hk } })).await;
        }
    }

    // прочие глобальные хоткеи (тихий/продолжить/повтор/без звука): перепривязка
    // с откатом на прежний при занятом сочетании — как у главного хоткея.
    for (key, old) in [
        ("quietHotkey", quiet_accelerator(&d)),
        ("continueHotkey", continue_accelerator(&d)),
        ("repeatHotkey", repeat_accelerator(&d)),
        ("muteHotkey", mute_accelerator(&d)),
    ] {
        let removed = rest.remove(key);
        let Some(hk) = removed
            .as_ref()
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
        else {
            continue;
        };
        if hk != old {
            let gs = d.app.global_shortcut();
            let _ = gs.unregister(old.as_str());
            if gs.register(hk.as_str()).is_err() {
                let _ = gs.register(old.as_str());
                return err(format!("Сочетание {hk} занято системой"));
            }
        }
        let _ = via_gate_panel(&d, "settings.set", json!({ "patch": { key: hk } })).await;
    }

    if !rest.is_empty() {
        let _ = via_gate_panel(&d, "settings.set", json!({ "patch": Value::Object(rest) })).await;
    }
    // тумблер «Режим логов» применяем сразу (без перезапуска)
    crate::metrics::set_enabled(d.settings.bool("diagnostics"));
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
pub(crate) async fn set_via_slash(
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
    via_gate_panel(&d, "sessions.control", json!({ "session_id": session_id, "model": model })).await
}

/// Ядро смены модели — общее для IPC и капабилити `sessions.control` (инкр. 8).
pub(crate) async fn set_model_core(d: &Arc<Daemon>, session_id: &str, model: &str) -> Value {
    // Аллоулист в ядре → защищены ВСЕ вызыватели (панель, голос, агент-капабилити):
    // никакой свободный текст не уходит в `/model …` пасту в пану (SEC-3).
    if let Err(e) = crate::convo::skills::validate_model(model) {
        return err(e);
    }
    let friendly = friendly_model(model);
    set_via_slash(d, session_id, format!("/model {model}"), move |s| {
        s.model = Some(friendly); // оптимистично; транскрипт подтвердит
        s.model_at = Some(now_ms());
    })
    .await
}

#[tauri::command]
pub async fn session_set_effort(app: AppHandle, session_id: String, level: String) -> Value {
    let d = Daemon::get(&app);
    via_gate_panel(&d, "sessions.control", json!({ "session_id": session_id, "effort": level })).await
}

/// Ядро смены effort — общее для IPC и капабилити `sessions.control` (инкр. 8).
pub(crate) async fn set_effort_core(d: &Arc<Daemon>, session_id: &str, level: &str) -> Value {
    if let Err(e) = crate::convo::skills::validate_effort(level) {
        return err(e);
    }
    let lv = level.to_string();
    set_via_slash(d, session_id, format!("/effort {level}"), move |s| {
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
            windows::toast_remove(&d, &format!("q-{session_id}")); // снять «липкую» карточку
            ok()
        }
        Err(e) => err(ellipsize(&one_line(&e), 100)),
    }
}

/// Действие с доски задач. ГРАНИЦА: ничего не отправляет и не мутирует доску —
/// возвращает редактируемый текст-инструкцию оркестратору. Панель префилит им
/// composer; реальная отправка — через `session_reply` после правки юзером.
/// Доска не меняется, пока не прилетит следующий настоящий `TodoWrite`.
#[tauri::command]
pub fn task_action(app: AppHandle, session_id: String, task_ref: i64, action: String) -> Value {
    let d = Daemon::get(&app);
    let title = d
        .session(&session_id)
        .and_then(|s| s.board)
        .and_then(|b| b.tasks.into_iter().find(|t| t.n == task_ref))
        .map(|t| t.text);
    match crate::daemon::task_action_text(&action, task_ref, title.as_deref()) {
        Some(text) => json!({ "ok": true, "text": text }),
        None => err("неизвестное действие"),
    }
}

/* ================= голос (инкремент 7) ================= */

/// Состояние голоса для настроек: движок, текущий спикер, список спикеров.
/// НЕ дёргает engine_available (там блокирующий HTTP — нельзя из команды).
#[tauri::command]
pub fn voice_get(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let cfg = crate::voice::config::VoiceConfig::from_settings(&d.settings.load());
    json!({
        "engine": cfg.engine,
        "speaker": d.voice.speaker(),
        "rate": d.voice.rate(),
        "mute": d.voice.is_muted(),
        "duck": d.voice.duck_enabled(),
        // Silero v4_ru — фиксированный набор спикеров
        "speakers": ["aidar", "baya", "kseniya", "xenia", "eugene"],
        // темпы речи (медленнее → быстрее)
        "rates": ["slow", "medium", "fast", "x-fast"],
    })
}

/// Сменить темп речи на лету + сохранить + дать послушать.
#[tauri::command]
pub fn voice_set_rate(app: AppHandle, rate: String) {
    let d = Daemon::get(&app);
    d.voice.set_rate(&rate);
    let mut patch = serde_json::Map::new();
    patch.insert("rate".into(), Value::String(rate));
    d.settings.set_voice(patch);
    d.voice.test_phrase("Так звучит выбранная скорость. Пиксела закончила, изменён один файл.");
}

/// Сменить спикера на лету (без перезапуска) + сохранить + дать послушать.
#[tauri::command]
pub fn voice_set_speaker(app: AppHandle, speaker: String) {
    let d = Daemon::get(&app);
    d.voice.set_speaker(&speaker);
    let mut patch = serde_json::Map::new();
    patch.insert("speaker".into(), Value::String(speaker.clone()));
    d.settings.set_voice(patch);
    d.voice.test_phrase(&format!(
        "Привет, это голос {speaker}. Пиксела закончила, изменён один файл."
    ));
}

/// Проиграть образец текущим голосом (кнопка «Тест» в настройках).
#[tauri::command]
pub fn voice_test(app: AppHandle) {
    Daemon::get(&app)
        .voice
        .test_phrase("Проверка голоса. Пиксела: четыре из шести задач, сейчас docker-compose.");
}

/// Тумблер «без звука» из настроек (мгновенно глушит очередь речи).
#[tauri::command]
pub fn voice_set_mute(app: AppHandle, on: bool) {
    Daemon::get(&app).voice.set_mute(on);
}

/// Пауза чужого медиа на время озвучки — тумблер + сохранить.
#[tauri::command]
pub fn voice_set_duck(app: AppHandle, on: bool) {
    let d = Daemon::get(&app);
    d.voice.set_duck(on);
    let mut patch = serde_json::Map::new();
    patch.insert("duckOthers".into(), Value::Bool(on));
    d.settings.set_voice(patch);
}

/// Прогнать действие панели через гейт (Consumer::panel) и вернуть структурный
/// панельный Value. Панель авто-одобряет (ConfirmPolicy::Never), confirmer не
/// вызывается. На Ok — отдаём value капабилити как есть (сохраняя needsTmux/channel);
/// на Denied/Rejected/Failed/NotFound — панельная ошибка.
pub(crate) async fn via_gate_panel(d: &Arc<Daemon>, id: &str, args: Value) -> Value {
    use crate::capability::{self, confirm::AutoApprove, grant::Consumer, GateError};
    match capability::invoke(
        &d.caps,
        d.clone(),
        &Consumer::panel(),
        id,
        args,
        &AutoApprove,
        &capability::audit::FileAudit,
        capability::GateConfig::default(),
    )
    .await
    {
        Ok(o) => o.value,
        Err(GateError::Failed(m)) => err(&m),
        Err(e) => err(e.to_string()),
    }
}

/// Ответ в сессию: tmux-вставка в пану нашего сервера (-L jarvis).
#[tauri::command]
pub async fn session_reply(app: AppHandle, session_id: String, text: String) -> Value {
    let d = Daemon::get(&app);
    via_gate_panel(&d, "sessions.reply", json!({ "session_id": session_id, "text": text })).await
}

/// Продолжить сессию (кнопка на тосте / хоткей): послать «продолжай» — например
/// после прерывания сном. Под капотом — обычная доставка в пану.
#[tauri::command]
pub async fn session_continue(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    via_gate_panel(&d, "sessions.reply", json!({ "session_id": session_id, "text": "продолжай" })).await
}

/// Ядро отправки в сессию — общее для IPC-команды панели и капабилити
/// `sessions.reply` (инкр. 8). Форма ответа панельная: {ok:true, channel,…} /
/// {ok:false, error} / {ok:false, needsTmux, resumeCmd}.
pub(crate) async fn reply_core(d: &Arc<Daemon>, session_id: String, text: String) -> Value {
    let Some(s) = d.session(&session_id) else { return err("Сессия не найдена") };
    let prompt = text.trim().to_string();
    if prompt.is_empty() {
        return err("Пустой текст");
    }

    if let Some(pane) = s.tmux_pane {
        if tmux::pane_alive(&pane).await {
            // Занята ли сессия в момент отправки. Если да — Claude Code положит
            // наш ввод в СВОЮ очередь, а prompt-хук придёт лишь когда он до него
            // дойдёт (после текущего ответа). Быстрый ack тогда невозможен — это
            // не провал доставки, а «поставлено в очередь». Limit — тоже ждёт.
            let busy = matches!(s.status, Status::Working | Status::Limit);

            // Первая вставка.
            let t0 = now_ms();
            let t_reply = crate::metrics::now();
            if let Err(e) = tmux::reply(&pane, &prompt).await {
                eprintln!("[jarvis] reply tmux fail: {e}");
                return err(format!("tmux: {}", ellipsize(&one_line(&e), 120)));
            }

            // Свободная сессия обработает сразу — ждём короткое подтверждение.
            if d.await_prompt_ack(&session_id, t0, std::time::Duration::from_millis(2500)).await {
                d.mark_prompt_sent(&session_id, &prompt);
                crate::log::line(&format!("[reply] доставлено sid={} pane={pane}", ellipsize(&session_id, 8)));
                crate::metrics::record("reply_ack", t_reply, json!({ "queued": false }));
                return json!({ "ok": true, "channel": "tmux" });
            }
            crate::metrics::record("reply_ack", t_reply, json!({ "queued": busy }));

            if busy {
                // Сессия работала — ввод ушёл в нативную очередь Claude Code.
                // НЕ ретраим вставку (повтор продублировал бы сообщение в очереди).
                // Подтверждаем асинхронно: когда Claude дойдёт до ввода, прилетит
                // prompt-хук — тогда и отметим доставку «из очереди».
                crate::log::line(&format!("[reply] в очереди (сессия занята) sid={} pane={pane}", ellipsize(&session_id, 8)));
                let d2 = d.clone();
                let sid2 = session_id.clone();
                let p2 = prompt.clone();
                tauri::async_runtime::spawn(async move {
                    if d2.await_prompt_ack(&sid2, t0, std::time::Duration::from_secs(300)).await {
                        d2.mark_prompt_sent(&sid2, &p2);
                        crate::log::line(&format!("[reply] доставлено из очереди sid={}", ellipsize(&sid2, 8)));
                    } else {
                        crate::log::line(&format!("[reply] очередь: 5 мин без подтверждения sid={}", ellipsize(&sid2, 8)));
                    }
                });
                return json!({ "ok": true, "channel": "tmux", "queued": true });
            }

            // Свободная сессия, но ack не пришёл — вставка могла не успеть
            // зарегистрироваться. Один ретрай (C-u в reply() чистит строку,
            // повтор не задваивает текст).
            let t1 = now_ms();
            if let Err(e) = tmux::reply(&pane, &prompt).await {
                return err(format!("tmux: {}", ellipsize(&one_line(&e), 120)));
            }
            if d.await_prompt_ack(&session_id, t1, std::time::Duration::from_millis(2500)).await {
                d.mark_prompt_sent(&session_id, &prompt);
                crate::log::line(&format!("[reply] доставлено sid={} pane={pane} (2-я попытка)", ellipsize(&session_id, 8)));
                return json!({ "ok": true, "channel": "tmux", "attempts": 2 });
            }
            return err("Агент не подтвердил получение — проверь терминал");
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

/// Решение пользователя по карточке подтверждения агента (R4). In-process —
/// вызывается ТОЛЬКО из панели (на сокет не выставлено): агент не может сам себя
/// одобрить.
#[tauri::command]
pub fn agent_confirm(app: AppHandle, nonce: String, approved: bool) -> Value {
    let d = Daemon::get(&app);
    let known = d.pending.resolve(&nonce, approved);
    json!({ "ok": known })
}

/// Голосовая маршрутизация: тап по варианту пикера в тосте → доставить выбор
/// ждущему роутеру (`session_id == None` → отмена выбора). In-process (НЕ в
/// MCP-реестре): голосовой агент не может сам себя выбрать.
#[tauri::command]
pub fn voice_pick_resolve(app: AppHandle, nonce: String, session_id: Option<String>) -> Value {
    let d = Daemon::get(&app);
    let known = d.picks.resolve(&nonce, session_id);
    json!({ "ok": known })
}

/// Голосовая маршрутизация: «Отменить» на staged-карточке → снять отложенную
/// отправку ДО tmux-пасты. true — если успели до истечения окна.
#[tauri::command]
pub fn voice_stage_cancel(app: AppHandle, nonce: String) -> Value {
    let d = Daemon::get(&app);
    let cancelled = d.stage.cancel(&nonce);
    if cancelled {
        crate::route::hud::emit(&d, crate::route::hud::Phase::Cancelled);
    }
    json!({ "ok": cancelled })
}

/// Текущее аудио-состояние — тост тянет его на загрузке (audio_state эмитится
/// лишь на изменении: ранний denied/тишина мог уйти до готовности webview; VR-3).
#[tauri::command]
pub fn voice_audio_state(app: AppHandle) -> Value {
    Daemon::get(&app).audio.audio_state_payload()
}

/// Голосовой разговор: «Да/Отмена» на confirm-карточке управления (п/п-2).
/// In-process (НЕ в MCP-реестре): голос-агент не может сам себя подтвердить.
#[tauri::command]
pub fn voice_confirm_resolve(app: AppHandle, nonce: String, approved: bool) -> Value {
    let d = Daemon::get(&app);
    let known = d.vconfirm.resolve(&nonce, approved);
    json!({ "ok": known })
}

/* ================= служебное ================= */

/// Снять ложный лимит-баннер по официальному usage (таймер из main).
pub fn reconcile_limit(d: &Arc<Daemon>) {
    limits::reconcile(d);
}

/* ================= агент-хост (фаза 5) ================= */

/// Отправить сообщение агенту и немедленно вернуть `{ok:true}`.
///
/// Потоковые события поступают через канал `agent:event` (тип `AgentEvent`).
/// `session_id` — необязателен; при наличии используется для возобновления (--resume).
#[tauri::command]
pub async fn agent_send(app: AppHandle, message: String, session_id: Option<String>) -> Value {
    use crate::agent::ClaudeCliHost;
    use crate::capability::{build_registry, grant::Consumer};
    use crate::util::jarvis_dir;

    let mcp_config = jarvis_dir().join("jarvis-mcp.json").to_string_lossy().to_string();

    // Собрать список инструментов из реестра капабилити агента
    let reg = build_registry();
    let agent = Consumer::agent();
    let tools: Vec<String> = reg
        .list_for(&agent.grant)
        .into_iter()
        // Claude называет MCP-инструменты mcp__<server>__<tool>, заменяя точки в
        // id на подчёркивания (проверено живым смоуком: sessions.reply →
        // mcp__jarvis__sessions_reply). Без этого --tools не совпадал бы с реальными.
        .map(|m| format!("mcp__jarvis__{}", m.id.replace('.', "_")))
        .collect();

    let host = ClaudeCliHost { app: app.clone(), mcp_config };
    let resume = session_id.clone();

    tauri::async_runtime::spawn(async move {
        host.run(&message, &tools, resume.as_deref()).await;
    });

    json!({ "ok": true })
}

/// Открыть (или сфокусировать) окно чата с агентом (фаза 7).
#[tauri::command]
pub fn agent_chat_open(app: AppHandle) {
    let _ = windows::create_agent_chat(&app);
}

/* ================= STT — панель настроек (инкремент 9, фаза 9) ================= */

/// Состояние STT для настроек: активный движок, список движков, доступность, хоткей.
/// Не дёргает `available()` напрямую — он блокирует (HTTP). Возвращает мгновенный срез.
#[tauri::command]
pub fn stt_get(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let cfg = crate::stt::config::SttConfig::from_settings(&d.settings.load());
    let engine_name = d.stt.engine_name();
    let whisper_model = crate::util::jarvis_dir()
        .join("stt")
        .join("ggml-large-v3-turbo-q5_0.bin")
        .exists();
    // Qwen3 сайдкар «готов», если движок отвечает на /health (быстро из кэша).
    // Для UI — показываем наличие файла модели через быструю HTTP-проверку.
    let qwen3_sidecar = d.stt.available(); // блокирует не более 3 с (connect timeout)
    // Установлен ли сайдкар на диске (venv + stt-server.py) — отдельно от health:
    // панель предлагает «Установить», если файлов нет, даже когда демон не отвечает.
    let qwen3_installed = crate::install::status().qwen3_sidecar;
    json!({
        "engine": engine_name,
        "engines": ["whisper-turbo", "qwen3-0.6b", "qwen3-1.7b"],
        "whisperReady": whisper_model,
        "qwen3Ready": qwen3_sidecar,
        "qwen3Installed": qwen3_installed,
        "available": qwen3_sidecar || (cfg.engine == "whisper-turbo" && whisper_model),
        "hotkey": if cfg.hotkey.is_empty() { "F8".to_string() } else { cfg.hotkey },
    })
}

/// Сменить движок STT + сохранить в settings.json. Требует перезапуска демона.
#[tauri::command]
pub fn stt_set_engine(app: AppHandle, engine: String) -> Value {
    let allowed = ["whisper-turbo", "qwen3-0.6b", "qwen3-1.7b"];
    if !allowed.contains(&engine.as_str()) {
        return err(format!("Неизвестный STT-движок: {engine}"));
    }
    let d = Daemon::get(&app);
    let mut patch = serde_json::Map::new();
    patch.insert("engine".into(), Value::String(engine));
    d.settings.set_stt(patch);
    json!({ "ok": true, "restart": true })
}

/// Тест диктовки: ~4 с захвата с микрофона → транскрипция активным движком.
/// Всё блокирующее вынесено в spawn_blocking — не блокирует tokio-рантайм.
#[tauri::command]
pub async fn stt_test(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let stt = d.stt.clone();
    let hub = d.audio.clone();
    let opts = stt.options();

    // Весь захват + транскрипция — в блокирующем потоке (cpal + reqwest).
    // Захват идёт через общий AudioHub (единая зона ответственности, инкр. 10).
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let session = hub.open_capture(false);
        std::thread::sleep(std::time::Duration::from_secs(4));
        let pcm = session.finish()
            .map_err(|e| format!("захват: {e}"))?;
        let r = stt.transcribe(&pcm, &opts)
            .map_err(|e| format!("транскрипция: {e}"))?;
        Ok(r.text)
    })
    .await;

    match result {
        Ok(Ok(text)) => json!({ "ok": true, "text": text }),
        Ok(Err(e)) => json!({ "ok": false, "error": e }),
        Err(e) => json!({ "ok": false, "error": format!("задача упала: {e}") }),
    }
}

// ─── Wake-word + общий аудио-вход (инкр. 10) ─────────────────────────────────

/// Статус wake-word + аудио-входа для панели.
#[tauri::command]
pub fn wake_get(app: AppHandle) -> Value {
    Daemon::get(&app).wake.status()
}

/// Вкл/выкл always-on детектор. Поднимает/гасит consumer-поток и аудио-захват.
#[tauri::command]
pub fn wake_set_enabled(app: AppHandle, on: bool) -> Value {
    let d = Daemon::get(&app);
    let mut patch = serde_json::Map::new();
    patch.insert("enabled".into(), json!(on));
    d.settings.set_block("wake", patch);
    d.wake.set_enabled(on);
    json!({ "ok": true, "status": d.wake.status() })
}

/// Установить порог срабатывания (0..1). Переконфигурирует детектор вживую.
#[tauri::command]
pub fn wake_set_threshold(app: AppHandle, threshold: f64) -> Value {
    let d = Daemon::get(&app);
    let mut patch = serde_json::Map::new();
    patch.insert("threshold".into(), json!(threshold.clamp(0.0, 1.0)));
    d.settings.set_block("wake", patch);
    let root = d.settings.load();
    let wcfg = crate::wakeword::config::WakeConfig::from_settings(&root);
    let vcfg = crate::wakeword::config::VerifyConfig::from_settings(&root);
    d.wake.reconfigure(wcfg, vcfg);
    json!({ "ok": true, "status": d.wake.status() })
}

/// Жёсткий mute общего аудио-входа (мгновенно глушит захват у источника).
#[tauri::command]
pub fn audio_set_mute(app: AppHandle, on: bool) -> Value {
    let d = Daemon::get(&app);
    d.audio.set_muted(on);
    let mut patch = serde_json::Map::new();
    patch.insert("mute".into(), json!(on));
    d.settings.set_stt(patch);
    json!({ "ok": true, "muted": on, "state": d.audio.state().as_str() })
}
