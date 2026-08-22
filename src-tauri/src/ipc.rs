//! IPC-команды панели и тостов — контракт window.jarvis / window.toast.
//!
//! Имена и формы ответов повторяют Electron-каналы один в один (':' → '_'):
//! рендерер не знает, что под мостом сменился рантайм. Формы ошибок — тоже:
//! { ok:false, error } / { ok:false, needsTmux, resumeCmd }.

use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use crate::bundle::host::Host;
use crate::daemon::Daemon;
use crate::model::Status;
use crate::util::*;
use crate::{claude_bin, limits, tmux, windows};

fn ok() -> Value {
    json!({ "ok": true })
}

fn err(msg: impl Into<String>) -> Value {
    json!({ "ok": false, "error": msg.into() })
}

/// Вне tmux мы не вставляем текст — сессией нельзя управлять, пока она не в
/// tmux. Подсказываем команду resume по агенту: shim завернёт её в наш сервер
/// (`claude --resume …` либо `codex resume …`).
/// «Сессия вне tmux» + команда, которой её можно поднять заново уже в tmux.
///
/// Команда собирается по `agent_id`, а не по ключу реестра: у сессии с узла
/// ключ выглядит как `<узел>:<id>`, и `claude --resume` с ним на той машине не
/// найдёт ничего. Для удалённой сессии добавляем, ГДЕ её выполнять — иначе
/// человек честно выполнит её у себя и не поймёт, почему не помогло.
fn tmux_needed(s: &crate::model::Session) -> Value {
    let agent = crate::backend::Agent::from_opt(s.agent.as_deref());
    let cmd = crate::backend::backend(agent).resume_cmd(s.agent_id());
    match &s.remote {
        Some(node) => json!({
            "ok": false, "needsTmux": true, "resumeCmd": cmd, "onNode": node,
        }),
        None => json!({ "ok": false, "needsTmux": true, "resumeCmd": cmd }),
    }
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
/// Раздаёт ли клавиатуру композитор, а не мы.
///
/// На Wayland глобальных сочетаний у приложения нет по устройству протокола:
/// перехват клавиш — привилегия композитора. Плагин при этом «регистрирует»
/// комбинацию в XWayland и молча не срабатывает — худший вид поломки: настройка
/// показывает клавишу, а клавиша не работает. Поэтому здесь мы честно
/// отказываемся и объясняем, куда её вешать.
pub fn compositor_owns_keys() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

pub fn register_hotkey(d: &Arc<Daemon>, accelerator: &str) -> Result<(), String> {
    if compositor_owns_keys() {
        return Err(
            "На Wayland глобальные клавиши раздаёт композитор. Повесь их в его конфиге: \
             bindsym $mod+j exec jarvis --toggle"
                .into(),
        );
    }
    let gs = d.app.global_shortcut();
    let current = d.settings.string("hotkey");
    if accelerator.is_empty() {
        // «не назначен»: снять текущий, ничего не регистрировать
        if !current.is_empty() && current != HK_NONE {
            let _ = gs.unregister(current.as_str());
        }
        return Ok(());
    }
    if accelerator == current && gs.is_registered(accelerator) {
        return Ok(());
    }
    if accelerator != current && !current.is_empty() && current != HK_NONE {
        let _ = gs.unregister(current.as_str());
    }
    if gs.register(accelerator).is_err() {
        if accelerator != current && !current.is_empty() && current != HK_NONE {
            let _ = gs.register(current.as_str());
        }
        return Err(format!("Сочетание {accelerator} занято системой"));
    }
    Ok(())
}

/* ================= реестр хоткей-действий ================= */

/// Сентинел «хоткей не назначен» в настройке (пустая строка значит «дефолт»,
/// поэтому нужен отдельный маркер — появляется после перехвата сочетания).
pub const HK_NONE: &str = "none";

/// Действие с глобальным хоткеем — единый реестр для назначения, детекта
/// конфликтов и приостановки на время записи сочетания.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HkAction {
    Panel,
    Continue,
    Repeat,
    Mute,
    Quiet,
    Select,
    Dictation,
}

impl HkAction {
    pub const ALL: [HkAction; 7] = [
        HkAction::Panel,
        HkAction::Continue,
        HkAction::Repeat,
        HkAction::Mute,
        HkAction::Quiet,
        HkAction::Select,
        HkAction::Dictation,
    ];

    /// Строковый id в IPC-контракте (bridge.js шлёт его в hotkey_assign).
    pub fn id(self) -> &'static str {
        match self {
            HkAction::Panel => "panel",
            HkAction::Continue => "continue",
            HkAction::Repeat => "repeat",
            HkAction::Mute => "mute",
            HkAction::Quiet => "quiet",
            HkAction::Select => "select",
            HkAction::Dictation => "dictation",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        HkAction::ALL.into_iter().find(|a| a.id() == s)
    }

    /// Подпись для сообщений о конфликте и списка привязок в UI.
    pub fn label(self) -> &'static str {
        match self {
            HkAction::Panel => "Открыть панель",
            HkAction::Continue => "Продолжить сессию",
            HkAction::Repeat => "Повторить",
            HkAction::Mute => "Без звука",
            HkAction::Quiet => "Тихий режим",
            HkAction::Select => "Варианты ответа",
            HkAction::Dictation => "Диктовка",
        }
    }

    pub fn default_accel(self) -> &'static str {
        match self {
            HkAction::Panel => "Command+J",
            HkAction::Continue => "Command+Alt+C",
            HkAction::Repeat => "Command+Alt+R",
            HkAction::Mute => "Command+Alt+M",
            HkAction::Quiet => "Command+Alt+J",
            HkAction::Select => SELECT_TEMPLATE_DEFAULT,
            HkAction::Dictation => "F8",
        }
    }

    /// Ключ в настройках. None — диктовка: живёт в settings.stt.hotkey,
    /// читается/пишется отдельным путём (SttConfig / set_stt).
    pub fn settings_key(self) -> Option<&'static str> {
        match self {
            HkAction::Panel => Some("hotkey"),
            HkAction::Continue => Some("continueHotkey"),
            HkAction::Repeat => Some("repeatHotkey"),
            HkAction::Mute => Some("muteHotkey"),
            HkAction::Quiet => Some("quietHotkey"),
            HkAction::Select => Some("selectHotkeyTemplate"),
            HkAction::Dictation => None,
        }
    }
}

/// Сырое значение настройки → акселератор действия.
/// "" → дефолт; HK_NONE → None («не назначен»); select нормализуется.
pub fn accel_from_raw(raw: &str, a: HkAction) -> Option<String> {
    if raw == HK_NONE {
        return None;
    }
    if raw.is_empty() {
        return Some(a.default_accel().to_string());
    }
    if a == HkAction::Select {
        return Some(normalize_select_template(raw));
    }
    Some(raw.to_string())
}

/// Текущий акселератор действия из настроек; None = «не назначен».
pub fn action_accel(d: &Arc<Daemon>, a: HkAction) -> Option<String> {
    let raw = match a {
        HkAction::Dictation => {
            crate::stt::config::SttConfig::from_settings(&d.settings.load()).hotkey
        }
        _ => d.settings.string(a.settings_key().expect("не-dictation имеет ключ")),
    };
    accel_from_raw(&raw, a)
}

/// Акселератор действия → конкретные шорткаты (select → до 9 экземпляров).
/// Битые части молча выпадают — битое не конфликтует.
pub fn action_shortcuts(a: HkAction, accel: &str) -> Vec<Shortcut> {
    if a == HkAction::Select {
        (1..=9)
            .filter_map(|n| select_accel(accel, n).parse::<Shortcut>().ok())
            .collect()
    } else {
        accel.parse::<Shortcut>().ok().into_iter().collect()
    }
}

/// Конфликт нового сочетания действия `a` с текущими привязками ОСТАЛЬНЫХ
/// действий. bindings — (действие, акселератор), «не назначенные» не передавать.
/// Чистая функция — покрыта юнитами без Daemon.
pub fn find_conflict(
    bindings: &[(HkAction, String)],
    a: HkAction,
    accel: &str,
) -> Option<HkAction> {
    let new = action_shortcuts(a, accel);
    bindings.iter().find_map(|(other, cur)| {
        if *other == a {
            return None;
        }
        let cur_sc = action_shortcuts(*other, cur);
        new.iter().any(|n| cur_sc.contains(n)).then_some(*other)
    })
}

/// Снять регистрацию текущего сочетания действия (select — весь набор).
fn unregister_action(d: &Arc<Daemon>, a: HkAction) {
    let Some(accel) = action_accel(d, a) else { return };
    let gs = d.app.global_shortcut();
    match a {
        HkAction::Select => {
            for n in 1..=9 {
                let _ = gs.unregister(select_accel(&accel, n).as_str());
            }
        }
        _ => {
            let _ = gs.unregister(accel.as_str());
        }
    }
}

/// Зарегистрировать сочетание действия. select регистрируется ТОЛЬКО при
/// активном вопросе (набор динамический — см. set_select_hotkeys), поэтому
/// принимает флаг. Err = сочетание занято системой.
fn register_action_accel(
    d: &Arc<Daemon>,
    a: HkAction,
    accel: &str,
    select_active: bool,
) -> Result<(), ()> {
    let gs = d.app.global_shortcut();
    match a {
        HkAction::Select => {
            if !select_active {
                return Ok(());
            }
            for n in 1..=9 {
                if gs.register(select_accel(accel, n).as_str()).is_err() {
                    for k in 1..n {
                        let _ = gs.unregister(select_accel(accel, k).as_str());
                    }
                    return Err(());
                }
            }
            Ok(())
        }
        _ => gs.register(accel).map_err(|_| ()),
    }
}

/// Сохранить сырое значение акселератора действия (HK_NONE = «не назначен»).
/// `Err` — на диск не легло: сочетание работает до перезапуска и об этом
/// обязан узнать человек, а не следующий запуск.
async fn persist_accel(d: &Arc<Daemon>, a: HkAction, raw: &str) -> Result<(), String> {
    match a.settings_key() {
        Some(key) => save_via_gate(d, one_key(key, Value::String(raw.to_string()))).await,
        None => {
            // диктовка: settings.stt.hotkey
            let mut patch = serde_json::Map::new();
            patch.insert("hotkey".into(), Value::String(raw.to_string()));
            d.settings.try_set_block("stt", patch)
        }
    }
}

/// Патч из одного ключа — самая частая форма правки настроек.
fn one_key(key: &str, value: Value) -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([(key.to_string(), value)])
}

/// Ответ панели по итогу записи настроек: «сохранено» — только если правда
/// легло на диск. Настройка, живущая до перезапуска, — та же тихая потеря.
fn saved(res: Result<(), String>) -> Value {
    match res {
        Ok(()) => ok(),
        Err(e) => err(format!(
            "Настройка работает, но не сохранилась: {e}. После перезапуска вернётся прежняя"
        )),
    }
}

/// Записать настройки через гейт и убедиться, что патч ПРАВДА лёг в файл.
///
/// Гейт отдаёт настройки как есть даже когда `Store` не смог их записать (диск
/// полон, права слетели после запуска под sudo) — а панель по такому ответу
/// говорит человеку «сохранено». Читаем после записи, как `save_chat_book`:
/// иначе отказ всплывает только следующим запуском, когда объяснять уже нечем.
async fn save_via_gate(d: &Arc<Daemon>, patch: serde_json::Map<String, Value>) -> Result<(), String> {
    let out = via_gate_panel(d, "settings.set", json!({ "patch": Value::Object(patch.clone()) })).await;
    if out.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(out
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("настройки не записаны")
            .to_string());
    }
    let disk = d.settings.load();
    match patch.iter().find(|(k, v)| disk.get(k.as_str()) != Some(v)) {
        Some((key, _)) => Err(format!("«{key}» не сохранился на диск — подробности в логе")),
        None => Ok(()),
    }
}

/// Привязки всех действий для UI настроек: id, подпись, текущее сочетание
/// (null = не назначен), дефолт.
#[tauri::command]
pub fn hotkey_bindings(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let list: Vec<Value> = HkAction::ALL
        .iter()
        .map(|a| {
            json!({
                "action": a.id(),
                "label": a.label(),
                "accel": action_accel(&d, *a),
                "default": a.default_accel(),
            })
        })
        .collect();
    json!({ "ok": true, "bindings": list })
}

/// Назначить хоткей действию. Валидация → конфликт со своими (steal=false →
/// { ok:false, conflict } и ничего не меняется; steal=true → у конфликтующего
/// действия хоткей снимается в «не назначен») → перерегистрация с откатом
/// («занято системой» — как раньше).
#[tauri::command]
pub async fn hotkey_assign(
    app: AppHandle,
    action: String,
    accel: String,
    steal: Option<bool>,
) -> Value {
    let d = Daemon::get(&app);
    let Some(a) = HkAction::parse(&action) else {
        return err(format!("Неизвестное действие: {action}"));
    };
    let accel = accel.trim().to_string();
    if accel.is_empty() {
        return err("Пустое сочетание");
    }
    if a == HkAction::Select {
        if normalize_select_template(&accel) != accel {
            return err(format!("Битый шаблон «{accel}» — нужен вид Command+Alt+{{n}}"));
        }
    } else if accel.parse::<Shortcut>().is_err() {
        return err(format!("Не разобрал сочетание: {accel}"));
    }

    let old = action_accel(&d, a);
    if old.as_deref() == Some(accel.as_str()) {
        return json!({ "ok": true, "accel": accel });
    }

    // конфликты со своими хоткеями; перехват может каскадом задеть несколько
    // действий (напр. новый шаблон {n} бьётся с двумя) — снимаем в цикле
    let steal = steal.unwrap_or(false);
    loop {
        let bindings: Vec<(HkAction, String)> = HkAction::ALL
            .iter()
            .filter_map(|o| action_accel(&d, *o).map(|acc| (*o, acc)))
            .collect();
        let Some(other) = find_conflict(&bindings, a, &accel) else { break };
        if !steal {
            return json!({ "ok": false, "conflict": { "action": other.id(), "label": other.label() } });
        }
        unregister_action(&d, other);
        if let Err(e) = persist_accel(&d, other, HK_NONE).await {
            return err(format!(
                "«{}» освобождено только в этом запуске — настройки не записались: {e}",
                other.label()
            ));
        }
        crate::log::line(&format!(
            "[hotkeys] перехват: «{}» остался без сочетания",
            other.label()
        ));
    }

    // активность набора 1..9 фиксируем ДО снятия старого
    let select_active = a == HkAction::Select
        && old
            .as_ref()
            .map(|o| {
                d.app
                    .global_shortcut()
                    .is_registered(select_accel(o, 1).as_str())
            })
            .unwrap_or(false);
    unregister_action(&d, a);
    if register_action_accel(&d, a, &accel, select_active).is_err() {
        if let Some(oldacc) = &old {
            let _ = register_action_accel(&d, a, oldacc, select_active);
        }
        return err(format!("Сочетание {accel} занято системой"));
    }
    if let Err(e) = persist_accel(&d, a, &accel).await {
        return err(format!(
            "Сочетание работает, но не сохранилось: {e}. После перезапуска вернётся прежнее"
        ));
    }
    json!({ "ok": true, "accel": accel })
}

/// Приостановить/вернуть ВСЕ глобальные хоткеи Jarvis — режим записи
/// сочетания в настройках: пока пользователь жмёт комбо, команды не должны
/// срабатывать (и наши же шорткаты не должны съедать keydown у webview).
/// Идемпотентно. Страховки от «умершего» UI: авто-ресюм через 15 с
/// (повторный suspend продлевает) и ресюм при скрытии панели.
pub fn hotkeys_set_suspended(d: &Arc<Daemon>, on: bool) {
    use std::sync::atomic::Ordering;
    let was = d.hk_suspend_gen.load(Ordering::SeqCst) != 0;
    if on {
        if !was {
            // активность набора 1..9 запоминаем ДО снятия
            let select_on = action_accel(d, HkAction::Select)
                .map(|t| {
                    d.app
                        .global_shortcut()
                        .is_registered(select_accel(&t, 1).as_str())
                })
                .unwrap_or(false);
            d.hk_select_was_on.store(select_on, Ordering::SeqCst);
            for a in HkAction::ALL {
                unregister_action(d, a);
            }
            crate::log::line("[hotkeys] приостановлены (запись сочетания)");
        }
        let gen = d.hk_suspend_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let d2 = d.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(15));
            if d2.hk_suspend_gen.load(Ordering::SeqCst) == gen {
                crate::log::line("[hotkeys] авто-ресюм по таймауту — UI не вернул хоткеи");
                hotkeys_set_suspended(&d2, false);
            }
        });
    } else {
        if !was {
            return;
        }
        d.hk_suspend_gen.store(0, Ordering::SeqCst);
        if let Err(e) = register_hotkey(d, &action_accel(d, HkAction::Panel).unwrap_or_default()) {
            crate::log::line(&format!("[hotkeys] ресюм панели: {e}"));
        }
        register_quiet_hotkey(d);
        register_continue_hotkey(d);
        register_dictation_hotkey(d);
        register_repeat_hotkey(d);
        register_mute_hotkey(d);
        if d.hk_select_was_on.load(Ordering::SeqCst) {
            set_select_hotkeys(d, true);
        }
        crate::log::line("[hotkeys] возвращены");
    }
}

#[tauri::command]
pub fn hotkeys_suspend(app: AppHandle, on: bool) -> Value {
    hotkeys_set_suspended(&Daemon::get(&app), on);
    ok()
}

/// Аккселератор тумблера тихого режима ("" = не назначен), дефолт ⌘⌥J.
pub fn quiet_accelerator(d: &Arc<Daemon>) -> String {
    action_accel(d, HkAction::Quiet).unwrap_or_default()
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
    if compositor_owns_keys() {
        return; // клавиши раздаёт композитор — см. register_hotkey
    }
    let accel = quiet_accelerator(d);
    if accel.is_empty() {
        return; // «не назначен»
    }
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        let _ = gs.register(accel.as_str());
    }
}

/// Аккселератор «Продолжить» ("" = не назначен), дефолт ⌘⌥C.
pub fn continue_accelerator(d: &Arc<Daemon>) -> String {
    action_accel(d, HkAction::Continue).unwrap_or_default()
}

pub fn is_continue_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> bool {
    continue_accelerator(d)
        .parse::<Shortcut>()
        .map(|s| &s == shortcut)
        .unwrap_or(false)
}

pub fn register_continue_hotkey(d: &Arc<Daemon>) {
    if compositor_owns_keys() {
        return; // клавиши раздаёт композитор — см. register_hotkey
    }
    let accel = continue_accelerator(d);
    if accel.is_empty() {
        return; // «не назначен»
    }
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        let _ = gs.register(accel.as_str());
    }
}

/// Аккселератор диктовки: из `SttConfig.hotkey` ("" = не назначен), дефолт "F8".
pub fn dictation_accelerator(d: &Arc<Daemon>) -> String {
    action_accel(d, HkAction::Dictation).unwrap_or_default()
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
    if compositor_owns_keys() {
        return; // клавиши раздаёт композитор — см. register_hotkey
    }
    let accel = dictation_accelerator(d);
    if accel.is_empty() {
        return; // «не назначен»
    }
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        if let Err(e) = gs.register(accel.as_str()) {
            crate::log::line(&format!(
                "[dictation] хоткей {accel} не зарегистрировался: {e:?}"
            ));
        }
    }
}

/// Аккселератор «повторить уведомление» ("" = не назначен), дефолт ⌘⌥R.
pub fn repeat_accelerator(d: &Arc<Daemon>) -> String {
    action_accel(d, HkAction::Repeat).unwrap_or_default()
}

pub fn is_repeat_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> bool {
    repeat_accelerator(d)
        .parse::<Shortcut>()
        .map(|s| &s == shortcut)
        .unwrap_or(false)
}

pub fn register_repeat_hotkey(d: &Arc<Daemon>) {
    if compositor_owns_keys() {
        return; // клавиши раздаёт композитор — см. register_hotkey
    }
    let accel = repeat_accelerator(d);
    if accel.is_empty() {
        return; // «не назначен»
    }
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        let _ = gs.register(accel.as_str());
    }
}

/// Аккселератор «без звука» (mute) ("" = не назначен), дефолт ⌘⌥M.
pub fn mute_accelerator(d: &Arc<Daemon>) -> String {
    action_accel(d, HkAction::Mute).unwrap_or_default()
}

pub fn is_mute_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> bool {
    mute_accelerator(d)
        .parse::<Shortcut>()
        .map(|s| &s == shortcut)
        .unwrap_or(false)
}

pub fn register_mute_hotkey(d: &Arc<Daemon>) {
    if compositor_owns_keys() {
        return; // клавиши раздаёт композитор — см. register_hotkey
    }
    let accel = mute_accelerator(d);
    if accel.is_empty() {
        return; // «не назначен»
    }
    let gs = d.app.global_shortcut();
    if !gs.is_registered(accel.as_str()) {
        let _ = gs.register(accel.as_str());
    }
}

/// Дефолт шаблона хоткеев выбора варианта: ⌘⌥<цифра>.
pub const SELECT_TEMPLATE_DEFAULT: &str = "Command+Alt+{n}";

/// Подставить номер варианта в шаблон ("Command+Alt+{n}", 3 → "Command+Alt+3").
pub fn select_accel(template: &str, n: u32) -> String {
    template.replace("{n}", &n.to_string())
}

/// Нормализовать шаблон из настроек: без «{n}» или с непарсибельным
/// экземпляром → дефолт (мягкая деградация вместо мёртвых хоткеев).
pub fn normalize_select_template(raw: &str) -> String {
    let valid = raw.contains("{n}") && select_accel(raw, 1).parse::<Shortcut>().is_ok();
    if valid {
        raw.to_string()
    } else {
        SELECT_TEMPLATE_DEFAULT.to_string()
    }
}


/// Если shortcut — экземпляр шаблона с цифрой, вернуть номер варианта (1..9).
pub fn match_select_template(template: &str, shortcut: &Shortcut) -> Option<u32> {
    (1..=9).find(|n| {
        select_accel(template, *n)
            .parse::<Shortcut>()
            .map(|s| &s == shortcut)
            .unwrap_or(false)
    })
}

/// Выбор варианта вопроса: <шаблон>+1 … +9 (дефолт ⌘⌥1-9). Регистрируем
/// ДИНАМИЧЕСКИ — только пока есть активный вопрос (зовётся из do_push), чтобы
/// не перехватывать цифровые комбо глобально всё время. Идемпотентно: трогаем
/// только при смене состояния.
pub fn set_select_hotkeys(d: &Arc<Daemon>, on: bool) {
    // «не назначен» → снимать нечего и ставить нечего
    let Some(tpl) = action_accel(d, HkAction::Select) else {
        return;
    };
    set_select_hotkeys_tpl(d, on, &tpl);
}

/// То же с явным шаблоном — при смене selectHotkeyTemplate старый набор
/// снимается по прежнему шаблону, новый ставится по новому.
pub fn set_select_hotkeys_tpl(d: &Arc<Daemon>, on: bool, template: &str) {
    if compositor_owns_keys() {
        return; // клавиши раздаёт композитор — см. register_hotkey
    }
    let gs = d.app.global_shortcut();
    let mut touched = 0;
    let mut failed = 0;
    for n in 1..=9 {
        let accel = select_accel(template, n);
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
            "[select] {} {}{}",
            select_accel(template, 1).replace('1', "1-9"),
            if on {
                "включены (вопрос активен)"
            } else {
                "сняты"
            },
            if failed > 0 {
                format!(", провал: {failed}")
            } else {
                String::new()
            },
        ));
    }
}

/// Если shortcut — это <шаблон>+<цифра>, вернуть номер варианта (1..9).
pub fn is_select_hotkey(d: &Arc<Daemon>, shortcut: &Shortcut) -> Option<u32> {
    match_select_template(&action_accel(d, HkAction::Select)?, shortcut)
}

#[tauri::command]
pub async fn settings_set(app: AppHandle, patch: Value) -> Value {
    let d = Daemon::get(&app);
    let Some(patch) = patch.as_object() else {
        return err("bad patch");
    };
    let mut rest = patch.clone();

    if let Some(Value::Bool(open)) = rest.remove("openAtLogin") {
        let autolaunch = app.autolaunch();
        let res = if open {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        if let Err(e) = res {
            // не глотаем: видно в консоли `npm run start`, а UI перечитает
            // реальное is_enabled() и честно покажет, что не сработало
            eprintln!(
                "[jarvis:autostart] не смог {} автозапуск: {e}",
                if open {
                    "включить"
                } else {
                    "выключить"
                }
            );
        }
    }

    if let Some(hotkey) = rest.remove("hotkey") {
        if let Some(hk) = hotkey.as_str().filter(|s| !s.is_empty()) {
            if let Err(e) = register_hotkey(&d, hk) {
                return err(e);
            }
            if let Err(e) = save_via_gate(&d, one_key("hotkey", Value::from(hk))).await {
                return err(format!(
                    "Сочетание работает, но не сохранилось: {e}. После перезапуска вернётся прежнее"
                ));
            }
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
        if let Err(e) = save_via_gate(&d, one_key(key, Value::from(hk.as_str()))).await {
            return err(format!(
                "Сочетание работает, но не сохранилось: {e}. После перезапуска вернётся прежнее"
            ));
        }
    }

    // шаблон хоткеев выбора варианта (⌘⌥1-9 по умолчанию): валидация + если
    // вопрос сейчас активен (набор зарегистрирован) — перерегистрация на лету.
    if let Some(v) = rest.remove("selectHotkeyTemplate") {
        if let Some(tpl) = v.as_str().filter(|s| !s.is_empty()).map(String::from) {
            if normalize_select_template(&tpl) != tpl {
                return err(format!(
                    "Битый шаблон «{tpl}» — нужен вид Command+Alt+{{n}}"
                ));
            }
            let old = action_accel(&d, HkAction::Select)
                .unwrap_or_else(|| SELECT_TEMPLATE_DEFAULT.to_string());
            let gs = d.app.global_shortcut();
            let active = gs.is_registered(select_accel(&old, 1).as_str());
            if active && tpl != old {
                set_select_hotkeys_tpl(&d, false, &old);
            }
            let saved =
                save_via_gate(&d, one_key("selectHotkeyTemplate", Value::from(tpl.as_str()))).await;
            if active && tpl != old {
                set_select_hotkeys_tpl(&d, true, &tpl);
            }
            if let Err(e) = saved {
                return err(format!(
                    "Шаблон работает, но не сохранился: {e}. После перезапуска вернётся прежний"
                ));
            }
        }
    }

    // «grants» — security-ключ: капабилити settings.set не пишет его НИКОМУ,
    // включая панель (гейт, §7). Законный путь один — эта команда, то есть руки
    // пользователя в настройках. Форму нормализуем, чтобы и через UI нельзя было
    // записать ничего, кроме поимённого авто-одобрения.
    if let Some(g) = rest.remove("grants") {
        if let Err(e) = d.settings.try_save(one_key("grants", normalize_grants(&g))) {
            return err(format!("Права не сохранены: {e}"));
        }
    }

    const APPEARANCE_KEYS: [&str; 7] =
        ["theme", "paint", "mode", "accent", "density", "radius", "scale"];
    let appearance_changed = APPEARANCE_KEYS.iter().any(|k| rest.contains_key(*k));
    let mode_changed = rest.contains_key("mode");
    let remotes_changed = rest.contains_key("remotes");
    // Отказ записи не отменяет применённого: тема уже перекрашена, узлы уже
    // подняты. Применяем, но в конце говорим правду — «сохранено» про то, чего
    // не будет после перезапуска, хуже любой ошибки.
    let saved = if rest.is_empty() {
        Ok(())
    } else {
        save_via_gate(&d, rest).await
    };
    // внешность сменили в одном окне — перекрашиваем все сразу (дизайн 14f «вид»)
    if appearance_changed {
        windows::broadcast_appearance(&d);
    }
    // накладка ⇄ окно: свойства самого окна меняем здесь, раскладку — CSS по data-mode
    if mode_changed {
        windows::apply_mode(&d);
    }
    // список узлов правят не только вкладкой «Удалённые» (ещё settings.json и
    // `jarvis-setup remote add`) — применяем сразу, как и всё в этом обработчике
    if remotes_changed {
        d.start_remotes();
    }
    // тумблер «Режим логов» применяем сразу (без перезапуска)
    crate::metrics::set_enabled(d.settings.bool("diagnostics"));
    crate::log::set_enabled(d.settings.bool("diagnostics"));
    if windows::panel_visible(&d) {
        windows::position_panel(&d); // позиция могла смениться
    }
    match saved {
        Ok(()) => ok(),
        Err(e) => err(format!(
            "Изменения работают, но не сохранились: {e}. После перезапуска вернутся прежние"
        )),
    }
}

/// Свести патч грантов к единственной поддерживаемой форме:
/// `{"<потребитель>": {"autoApprove": ["<id капабилити>", …]}}`. Всё прочее
/// отбрасываем — иначе панель стала бы каналом для произвольных прав.
fn normalize_grants(v: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for (consumer, body) in v.as_object().into_iter().flatten() {
        let ids: Vec<Value> = body
            .get("autoApprove")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter(|x| x.is_string()).cloned().collect())
            .unwrap_or_default();
        out.insert(consumer.clone(), json!({ "autoApprove": ids }));
    }
    Value::Object(out)
}

/* ================= чат сессии ================= */

/// Узел + хвост его транскрипта + смещение, с которого продолжать. Узел
/// возвращаем сюда же: живой хвост пойдёт в ТОТ ЖЕ узел, а не в найденный
/// заново — между двумя поисками список мог смениться.
/// Ошибки — человеческим текстом: это то, что увидит юзер вместо чата.
async fn remote_transcript(
    d: &std::sync::Arc<Daemon>,
    name: &str,
    path: &str,
) -> Result<(std::sync::Arc<crate::remote::Node>, String, u64), String> {
    let node = d
        .remotes
        .node(name)
        .ok_or_else(|| format!("Узел «{name}» не подключён"))?;
    let client = node.client().map_err(|e| format!("Узел «{name}»: {e}"))?;
    match client.tail_text(path, 512 * 1024).await {
        Ok(Some((text, next))) => Ok((node, text, next)),
        Ok(None) => Err("Транскрипта ещё нет на узле — сессия не слала событий".into()),
        Err(e) => Err(format!("Узел «{name}»: {e}")),
    }
}

/// Асинхронна из-за удалённых сессий: их транскрипт приезжает по HTTP с узла.
/// Локальная ветка осталась прежним синхронным чтением файла.
#[tauri::command]
pub async fn chat_open(app: AppHandle, session_id: String) -> Value {
    let _t = crate::log::Step::new("chat_open");
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else {
        return err("Сессия не найдена");
    };
    let Some(tr) = s.transcript else {
        return err("Нет транскрипта — сессия ещё не слала событий (перезапусти claude)");
    };
    // Парсер транскрипта — по бэкенду сессии (Claude JSONL vs Codex rollout).
    let agent = crate::backend::Agent::from_opt(s.agent.as_deref());
    let be = crate::backend::backend(agent);
    // Байты берём с той машины, где живёт сессия, и там же заводим живой хвост.
    // Ниже по коду уже всё равно, откуда они приехали.
    let entries = match &s.remote {
        None => {
            let e = be.read_entries(std::path::Path::new(&tr), 512 * 1024);
            d.tail.start(app.clone(), agent, session_id.clone(), tr.clone());
            e
        }
        Some(name) => match remote_transcript(&d, name, &tr).await {
            Ok((node, text, next)) => {
                d.tail
                    .start_remote(app.clone(), agent, session_id.clone(), node, tr.clone(), next);
                be.entries_from_text(&text)
            }
            Err(e) => return err(e),
        },
    };
    let (all_items, turns) = crate::turns::segment(be, &entries);
    let tail_start = all_items.len().saturating_sub(80);
    let items = &all_items[tail_start..];
    // разметка ходов в координатах видимого хвоста; факты — для дет-карточек
    let spans: Vec<Value> = turns
        .iter()
        .filter(|t| t.span.end > tail_start)
        .map(|t| {
            json!({
                "key": t.span.key,
                "start": t.span.start.saturating_sub(tail_start),
                "end": t.span.end - tail_start,
                // ход, чья юзер-реплика отрезана хвостом, не суммаризируем из UI
                "complete": t.span.complete && t.span.start >= tail_start,
                "files": t.facts.files,
                "commands": t.facts.commands,
            })
        })
        .collect();
    let cards = crate::turnsum::load_cards(&session_id);
    let llm = claude_bin::any_service_bin();
    if llm {
        d.turn_backfill(session_id.clone(), 5);
    }
    println!(
        "[jarvis] chat:open {} items={} turns={} cards={} file={}",
        ellipsize(&session_id, 8),
        items.len(),
        spans.len(),
        cards.len(),
        short_home(&tr)
    );
    json!({ "ok": true, "items": items, "spans": spans, "cards": cards, "llm": llm, "project": s.project })
}

#[tauri::command]
pub fn chat_close(app: AppHandle) {
    Daemon::get(&app).tail.stop();
}

/// Сводка конкретного хода по кнопке. Fire-and-forget: карточку принесёт
/// событие chat:summary (кэш — turnsum), UI показывает спиннер сам.
#[tauri::command]
pub fn chat_summarize(app: AppHandle, session_id: String, turn_key: String) -> Value {
    let d = Daemon::get(&app);
    if d.session(&session_id).is_none() {
        return err("Сессия не найдена");
    }
    tauri::async_runtime::spawn(async move {
        let Some((be, entries)) = d.turn_entries(&session_id).await else { return };
        let (_items, turns) = crate::turns::segment(be, &entries);
        if let Some(t) = turns.iter().find(|t| t.span.key == turn_key) {
            d.turn_generate(&session_id, t).await;
        }
    });
    ok()
}

/// Открыть файл из карточки сводки. path из транскрипта (запись агента),
/// не свободный ввод; резолв от cwd сессии + канонизация + только обычные файлы.
#[tauri::command]
pub fn file_open(app: AppHandle, session_id: String, path: String, reveal: bool) -> Value {
    let d = Daemon::get(&app);
    let cwd = d.session(&session_id).and_then(|s| s.cwd);
    let p = match resolve_user_file(cwd.as_deref(), &path) {
        Ok(p) => p,
        Err(e) => return err(&e),
    };
    // Путь из транскрипта — недоверенный: `open evil.command` ЗАПУСТИЛ бы
    // скрипт. Исполняемые документы не открываем — только показываем в папке.
    let reveal = reveal || force_reveal(&p);
    match open_path(&p, reveal) {
        Ok(()) => ok(),
        Err(e) => err(&e),
    }
}

/// Открыть файл системным способом либо показать его в файловом менеджере.
#[cfg(target_os = "macos")]
fn open_path(p: &std::path::Path, reveal: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new("open");
    if reveal {
        cmd.arg("-R"); // показать в Finder
    }
    cmd.arg(p).spawn().map(|_| ()).map_err(|e| format!("open: {e}"))
}

/// Linux: `xdg-open` открывает файл ассоциированной программой. Для «показать
/// в папке» единого способа нет — сперва пробуем D-Bus-интерфейс
/// `org.freedesktop.FileManager1` (его понимают Nautilus, Dolphin, Nemo,
/// Thunar), иначе просто открываем родительскую папку.
#[cfg(not(target_os = "macos"))]
fn open_path(p: &std::path::Path, reveal: bool) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let quiet = |c: &mut Command| {
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    };

    if reveal {
        let uri = format!("file://{}", p.display());
        let mut dbus = Command::new("dbus-send");
        quiet(&mut dbus);
        let ok = dbus
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{uri}"),
                "string:",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Ok(());
        }
        // не вышло — открываем папку целиком, файл юзер найдёт глазами
        let dir = p.parent().unwrap_or(p);
        let mut c = Command::new("xdg-open");
        quiet(&mut c);
        return c.arg(dir).spawn().map(|_| ()).map_err(|e| format!("xdg-open: {e}"));
    }

    let mut c = Command::new("xdg-open");
    quiet(&mut c);
    c.arg(p).spawn().map(|_| ()).map_err(|e| format!("xdg-open: {e}"))
}

/// Типы, которые системный «открыть» ВЫПОЛНЯЕТ, а не показывает (Terminal/
/// Automator/AppleScript и т.п.) — такие принудительно уводим в reveal.
/// Список маковский, но на Linux он тоже не мешает: .desktop и так не в нём,
/// а лишняя осторожность с недоверенным путём из транскрипта не повредит.
fn force_reveal(p: &std::path::Path) -> bool {
    const EXECUTABLE_DOCS: [&str; 8] = [
        "command", "terminal", "workflow", "webloc", "tool", "applescript", "scpt", "app",
    ];
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXECUTABLE_DOCS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Путь юзер-файла: абсолютный как есть, относительный — от cwd сессии;
/// канонизация резолвит симлинки и отсекает несуществующее, берём только
/// обычные файлы. Пути НЕ ограничены cwd — агент легитимно трогает файлы вне
/// проекта (напр. ~/.claude/...), а клик по чипу — явное действие пользователя.
fn resolve_user_file(cwd: Option<&str>, path: &str) -> Result<std::path::PathBuf, String> {
    let raw = if std::path::Path::new(path).is_absolute() {
        std::path::PathBuf::from(path)
    } else {
        std::path::PathBuf::from(cwd.ok_or("нет рабочего каталога сессии")?).join(path)
    };
    let p = raw
        .canonicalize()
        .map_err(|_| format!("файл не найден: {path}"))?;
    if !p.is_file() {
        return Err(format!("не файл: {path}"));
    }
    Ok(p)
}

/// Прочитать файл для вьюера документов (спека 2026-07-18 §3.1). Путь обязан
/// входить в множество файлов из фактов ходов сессии — сверка по
/// канонизированным путям, см. file_read_impl.
#[tauri::command]
pub async fn file_read(app: AppHandle, session_id: String, path: String) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else {
        return file_read_dispatch(None, &path);
    };
    // Файлы удалённой сессии лежат на её машине. Открыть путь здесь — значит
    // показать одноимённый файл ЭТОГО компьютера под видом того: узел отдаёт
    // только транскрипты, и это сознательная граница (см. docs/remote.md).
    if let Some(name) = &s.remote {
        return err(format!("Файлы сессии — на узле «{name}», отсюда их не открыть"));
    }
    let entries = d.turn_entries(&session_id).await;
    file_read_dispatch(entries.map(|(be, e)| (s.cwd, be, e)), &path)
}

/// Диспетчер file_read, отделён от команды ради тестов: None — сессии нет
/// (или у неё нет транскрипта, т.е. фактов, — для вьюера это одно и то же).
fn file_read_dispatch(
    sess: Option<(Option<String>, &dyn crate::backend::Backend, Vec<Value>)>,
    path: &str,
) -> Value {
    let Some((cwd, be, entries)) = sess else {
        return err("Сессия не найдена или без транскрипта");
    };
    file_read_impl(cwd.as_deref(), be, &entries, path)
}

/// Лимит содержимого file_read; больше — голова+хвост с truncated:true
/// (середина наименее информативна — как head_tail в turns.rs).
const FILE_READ_LIMIT: u64 = 512 * 1024;
const FILE_READ_HEAD: usize = 448 * 1024;
const FILE_READ_TAIL: usize = 64 * 1024;

/// Ядро file_read. Безопасность (§4 спеки): содержимое транскрипта
/// недоверенное, поэтому мало резолва файла — путь ОБЯЗАН входить в множество
/// файлов из фактов ходов этой сессии (пересчёт turns::segment по транскрипту),
/// сравнение канонизированных путей. Инъекция пути в транскрипт не даст чипу
/// открыть произвольный файл: путь вне фактов tool_use не пройдёт.
fn file_read_impl(
    cwd: Option<&str>,
    be: &dyn crate::backend::Backend,
    entries: &[Value],
    path: &str,
) -> Value {
    let p = match resolve_user_file(cwd, path) {
        Ok(p) => p,
        Err(e) => return err(&e),
    };
    let (_items, turns) = crate::turns::segment(be, entries);
    let allowed = turns
        .iter()
        .flat_map(|t| t.facts.files.iter())
        .filter_map(|f| resolve_user_file(cwd, &f.path).ok())
        .any(|q| q == p);
    if !allowed {
        return err("Файл не из фактов этой сессии");
    }
    match read_head_tail(&p) {
        Ok((content, truncated)) => json!({
            "ok": true,
            "name": p.file_name().and_then(|n| n.to_str()).unwrap_or(path),
            "content": content,
            "truncated": truncated,
        }),
        Err(e) => err(&format!("чтение: {e}")),
    }
}

/// Файл целиком до FILE_READ_LIMIT; больше — голова+хвост (UTF-8 lossy на
/// каждом куске: разрез посреди символа даёт replacement char на стыке — ок).
fn read_head_tail(p: &std::path::Path) -> std::io::Result<(String, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(p)?;
    let len = f.metadata()?.len();
    if len <= FILE_READ_LIMIT {
        let mut buf = Vec::with_capacity(len as usize);
        f.read_to_end(&mut buf)?;
        return Ok((String::from_utf8_lossy(&buf).into_owned(), false));
    }
    let mut head = vec![0u8; FILE_READ_HEAD];
    f.read_exact(&mut head)?;
    f.seek(SeekFrom::End(-(FILE_READ_TAIL as i64)))?;
    let mut tail = vec![0u8; FILE_READ_TAIL];
    f.read_exact(&mut tail)?;
    let content = format!(
        "{}\n\n[… обрезано: файл {} КБ …]\n\n{}",
        String::from_utf8_lossy(&head),
        len / 1024,
        String::from_utf8_lossy(&tail)
    );
    Ok((content, true))
}

/// Дифф файла для таба «Изменения» вьюера (спека 2026-07-18 §3.2). Тот же
/// гейт по фактам, что file_read; сам дифф считает git (gitdiff.rs) от cwd
/// сессии. Не в git / бинарь / нет cwd → mode "none" (таб просто не покажется).
#[tauri::command]
pub async fn file_diff(app: AppHandle, session_id: String, path: String) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else {
        return file_diff_dispatch(None, &path);
    };
    // git-дифф считается от cwd сессии — у удалённой он на её машине (как и в
    // file_read: чужой одноимённый репозиторий показал бы неправду)
    if s.remote.is_some() {
        return json!({ "ok": true, "mode": "none", "label": "", "hunks": [] });
    }
    let entries = d.turn_entries(&session_id).await;
    file_diff_dispatch(entries.map(|(be, e)| (s.cwd, be, e)), &path)
}

/// Диспетчер file_diff, отделён от команды ради тестов (как file_read_dispatch).
fn file_diff_dispatch(
    sess: Option<(Option<String>, &dyn crate::backend::Backend, Vec<Value>)>,
    path: &str,
) -> Value {
    let Some((cwd, be, entries)) = sess else {
        return err("Сессия не найдена или без транскрипта");
    };
    file_diff_impl(cwd.as_deref(), be, &entries, path)
}

/// Ядро file_diff. Гейт §4 идентичен file_read_impl: путь обязан входить в
/// множество файлов из фактов ходов сессии (сверка канонизированных путей),
/// иначе инъекция пути в транскрипт дала бы дифф произвольного файла.
fn file_diff_impl(
    cwd: Option<&str>,
    be: &dyn crate::backend::Backend,
    entries: &[Value],
    path: &str,
) -> Value {
    let p = match resolve_user_file(cwd, path) {
        Ok(p) => p,
        Err(e) => return err(&e),
    };
    let (_items, turns) = crate::turns::segment(be, entries);
    let allowed = turns
        .iter()
        .flat_map(|t| t.facts.files.iter())
        .filter_map(|f| resolve_user_file(cwd, &f.path).ok())
        .any(|q| q == p);
    if !allowed {
        return err("Файл не из фактов этой сессии");
    }
    // git ищет репозиторий от cwd сессии — без него дифф не построить
    let Some(cwd) = cwd else {
        return json!({ "ok": true, "mode": "none", "label": "", "hunks": [] });
    };
    let diff = crate::gitdiff::diff_for_file(cwd, &p);
    json!({
        "ok": true,
        "mode": diff.mode,
        "label": diff.label,
        "hunks": diff.hunks,
    })
}

/// Открыть внешнюю ссылку из отрендеренного документа в браузере по клику.
/// markdown.js уже режет не-http(s) схемы, но UI-слою не доверяем — схема
/// валидируется и здесь; url уходит одним аргументом (без шелла).
/// Ошибка в панели → в общий лог.
///
/// Белый экран — это почти всегда исключение в JS, оборвавшее отрисовку. Без
/// этого канала оно видно только в девтулзах, то есть на практике не видно
/// никому: человек сообщает «белый экран», и дальше начинается гадание.
#[tauri::command]
pub fn ui_error(place: String, message: String) -> Value {
    crate::log::line(&format!(
        "[ui] {} — {}",
        one_line(&place),
        ellipsize(&one_line(&message), 600)
    ));
    json!({ "ok": true })
}

#[tauri::command]
pub fn url_open(url: String) -> Value {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return err("не-http ссылка");
    }
    // macOS — `open`, Linux — `xdg-open`; оба принимают url одним аргументом.
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    match std::process::Command::new(opener).arg(&url).spawn() {
        Ok(_) => ok(),
        Err(e) => err(&format!("{opener}: {e}")),
    }
}

#[tauri::command]
pub fn commands_get(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else {
        return json!([]);
    };
    // Свои слэш-команды есть у каждого агента; проектные из `.claude/commands`
    // читает только Claude — у остальных такой механики нет (у Kimi это skills,
    // отдельная история).
    match crate::backend::Agent::from_opt(s.agent.as_deref()) {
        crate::backend::Agent::Codex => {
            return serde_json::to_value(crate::commands_catalog::codex_commands())
                .unwrap_or_else(|_| json!([]))
        }
        crate::backend::Agent::Kimi => {
            return serde_json::to_value(crate::commands_catalog::kimi_commands())
                .unwrap_or_else(|_| json!([]))
        }
        crate::backend::Agent::Claude => {}
    }
    // Проектные команды каталог собирает из .claude/commands по cwd — на ЭТОЙ
    // машине. У сессии с узла её проект на той стороне, поэтому отдаём только
    // встроенные: чужой список команд хуже пустого.
    let cwd = s.cwd.as_deref().filter(|_| s.remote.is_none());
    serde_json::to_value(d.commands.get_for_cwd(cwd)).unwrap_or_else(|_| json!([]))
}

#[tauri::command]
pub fn app_meta(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    // Способности агентов — из бэкендов, а не литералами во фронте: раньше UI
    // сам решал «codex → скрыть effort, запретить свой ответ», и с третьим
    // агентом это молча разъехалось бы с Rust-стороной.
    let agents: Vec<Value> = crate::backend::Agent::all()
        .iter()
        .map(|a| {
            let be = crate::backend::backend(*a);
            json!({
                "id": a.label(),
                "title": a.title(),
                "models": be.models().iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
                "effortLevels": be.effort_levels(),
                "hasSeparateEffort": be.has_separate_effort(),
                "supportsCustomAnswer": be.supports_custom_answer(),
                "present": be.cli_found(),
            })
        })
        .collect();
    json!({
        "agents": agents,
        "effortLevels": *d.effort_levels.lock().unwrap(),
        "version": env!("CARGO_PKG_VERSION"),
        // Wayland отдаём в UI не ради красоты: там глобальные клавиши
        // приложению недоступны — их раздаёт композитор, — и настройка обязана
        // сказать это вслух, а не молча не работать.
        "wayland": std::env::var_os("WAYLAND_DISPLAY").is_some(),
    })
}

/// Проверить обновление и, если есть, скачать+установить (применится при
/// следующем запуске). Возвращает статус для UI «О программе».
#[tauri::command]
pub async fn update_check_install(app: AppHandle) -> Value {
    use tauri_plugin_updater::UpdaterExt;
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => return json!({ "ok": false, "error": format!("апдейтер недоступен: {e}") }),
    };
    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            match update.download_and_install(|_, _| {}, || {}).await {
                Ok(()) => {
                    crate::log::line(&format!("[updater] {version} установлен по кнопке"));
                    json!({ "ok": true, "updated": true, "version": version })
                }
                Err(e) => {
                    json!({ "ok": false, "error": ellipsize(&one_line(&e.to_string()), 120) })
                }
            }
        }
        Ok(None) => json!({ "ok": true, "updated": false }),
        Err(e) => json!({ "ok": false, "error": ellipsize(&one_line(&e.to_string()), 120) }),
    }
}

/// Перезапустить приложение (после установки обновления).
///
/// Именно `request_restart`: синхронный `restart()`, вызванный с главного
/// потока (а команда без async идёт ровно там), делает `cleanup_before_exit` +
/// `process::restart` БЕЗ `RunEvent::Exit`. А в этом событии у нас снимок
/// реестра, остановка ssh-туннелей, гашение сайдкаров и удаление run.sock —
/// без него перезапуск теряет состояние и оставляет висеть чужие процессы.
#[tauri::command]
pub fn app_relaunch(app: AppHandle) {
    app.request_restart();
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
    Daemon::get(&app)
        .usage
        .stats(period.as_deref().unwrap_or("today"))
}

/// Панель получает РОВНО то же, что агент в `limits.get`: пока сюда ехало одно
/// состояние баннера, человек в интерфейсе бюджета не видел вовсе.
#[tauri::command]
pub fn limit_get(app: AppHandle) -> Value {
    limits::state_json(&Daemon::get(&app))
}

/* ================= бюджет перед дорогой работой ================= */

/// Насколько свежими обязаны быть числа перед дорогой работой. Минута — это
/// «только что»: длинный ход и лишняя сессия стоят дороже одного GET.
pub const BUDGET_FRESH_MS: i64 = 60_000;

/// Провайдер бюджета за ярлыком агента. У codex подписки в бюджете нет — про
/// него гейт молчит, а не отказывает наугад по чужим числам.
pub fn budget_provider(agent: &str) -> Option<&'static str> {
    match agent.trim().to_lowercase().as_str() {
        "claude" => Some(crate::budget::CLAUDE),
        "kimi" => Some(crate::budget::KIMI),
        _ => None,
    }
}

/// Отказ по ступени бюджета — числами и временем сброса, а не «сейчас нельзя».
///
/// `bg` — работа фоновая (заход авто-цепочки): для неё отказ начинается уже с
/// `queue`, потому что фон и есть то, что откладывают первым. На глазах у
/// человека «фон в очередь» ещё не стена — там отказ только на `stop`.
///
/// `unknown` отказом НЕ считается: молчание добытчика неотличимо от «всё
/// хорошо» только на словах, а вставать по нему нельзя — `stop` и так стоит по
/// факту расхода, а не по прогнозу.
pub fn budget_refusal(provider: &str, rep: &Value, bg: bool) -> Option<String> {
    let p = rep.pointer(&format!("/providers/{provider}"))?;
    let rung = p.get("rung").and_then(Value::as_str)?;
    if !(rung == "stop" || (bg && rung == "queue")) {
        return None;
    }
    let num = |k: &str| p.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let reset = p.get("weekResetAt").and_then(Value::as_i64).unwrap_or(0);
    // Списанное вперёд называем числом: иначе «осталось 20%» и отказ выглядят
    // враньём, а человек не понимает, что стену сделал его же залп запусков.
    let held = if num("reservedPct") > 0.0 {
        format!(
            " (из них {:.1}% придержано под уже разрешённую работу, броней: {})",
            num("reservedPct"),
            p.get("reservedCount").and_then(Value::as_i64).unwrap_or(0)
        )
    } else {
        String::new()
    };
    let mut out = format!(
        "бюджет {provider}: {}. Осталось {:.1}% недели{held} при резерве {:.1}%, сброс через {} — {}",
        p.get("reason").and_then(Value::as_str).unwrap_or("ступень без причины"),
        num("weekLeftPct"),
        num("reservePct"),
        if reset > 0 { fmt_reset_in(reset) } else { "неизвестно сколько".into() },
        if bg {
            "фоновый заход в очередь: дождись сброса или веди эту работу руками"
        } else {
            "дождись сброса, возьми другого агента или закрой лишние через sessions.close"
        }
    );
    // Ночью буфер не спасает: он тратится только по явному разрешению человека,
    // а ночью человека нет. Это надо сказать, иначе отказ выглядит запасом.
    if rep.pointer("/night/active").and_then(Value::as_bool) == Some(true)
        && p.get("bufferAvailable").and_then(Value::as_bool) != Some(true)
    {
        out.push_str(&format!(
            ". Ночью буфер {:.0}% недоступен: {}",
            num("bufferPct"),
            p.get("bufferReason")
                .and_then(Value::as_str)
                .unwrap_or(crate::budget::BUFFER_REASON)
        ));
    }
    Some(out)
}

/// Обязательный свежий запрос перед дорогой работой, бронь ожидаемого расхода и
/// отказ словами, если ступень говорит «стоп». Молча упереться в бюджет нельзя:
/// числа и время сброса обязаны дойти и до агента, и до человека.
///
/// Ожидаемая стоимость списывается ДО проверки, а не после: между «посмотрел
/// остаток» и «потратил» помещается сколько угодно других запусков — ровно
/// поэтому залп и проходил целиком. Списав сначала, каждый вызывающий видит в
/// остатке хотя бы себя, а брони копятся, а не теряются.
///
/// Бронь надо ВЕРНУТЬ, если работа так и не началась (`Reservation::release`) —
/// иначе бюджет протечёт вниз и начнёт врать в другую сторону. Отказ здесь
/// возвращает её сам.
pub async fn budget_reserve(
    d: &Arc<Daemon>,
    agent: &str,
    model: Option<&str>,
    bg: bool,
    why: &str,
) -> Result<crate::budget::Reservation, String> {
    let Some(provider) = budget_provider(agent) else {
        // У codex подписки в бюджете нет — держать нечего и отказывать не за что.
        return Ok(crate::budget::Reservation::none());
    };
    crate::budget::ensure_fresh(d, BUDGET_FRESH_MS, why).await;
    let hold = crate::budget::reserve(
        provider,
        crate::budget::expected_pct(provider, model),
        why,
        now_ms(),
    );
    match budget_refusal(provider, &crate::budget::report(d), bg) {
        Some(text) => {
            hold.release(); // отказали — работа не началась, держать нечего
            Err(text)
        }
        None => Ok(hold),
    }
}

/// Тот же гейт для вызывающих, которым нечего возвращать: ход уходит сразу и
/// «не началось» у них не бывает. Сигнатуру знают чужие файлы — не менять.
pub async fn budget_gate(d: &Arc<Daemon>, agent: &str, bg: bool, why: &str) -> Result<(), String> {
    budget_reserve(d, agent, None, bg, why)
        .await
        .map(crate::budget::Reservation::in_flight)
}

/// Машины, на которых можно работать: эта плюс настроенные узлы.
///
/// Список нужен вкладке «Проекты» первым шагом — до выбора проекта. Локальная
/// всегда первая и всегда «на связи»: она никуда не денется, и отсутствие
/// узлов не должно выглядеть как «работать негде».
/// Реестр своих агентов: список из настроек и готовые карточки.
#[tauri::command]
pub async fn agents_list(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    json!({
        "ok": true,
        "agents": crate::agents::parse(&d.settings.load()),
        "presets": crate::agents::presets(),
    })
}

/// Сохранить реестр целиком и привести шимы к нему.
///
/// Валидация — до записи и вся разом: человек правит форму целиком и вправе
/// увидеть все дыры, а не по одной за подход.
#[tauri::command]
pub async fn agents_save(app: AppHandle, agents: Value) -> Value {
    let d = Daemon::get(&app);
    let list: Vec<crate::agents::CustomAgent> = match serde_json::from_value(agents) {
        Ok(l) => l,
        Err(e) => return err(format!("не разобрал список агентов: {e}")),
    };
    let mut bad = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for a in &list {
        for p in crate::agents::problems(a) {
            bad.push(format!("{}: {p}", if a.id.is_empty() { "агент" } else { &a.id }));
        }
        if !seen.insert(a.id.clone()) {
            bad.push(format!("{}: имя повторяется", a.id));
        }
    }
    if !bad.is_empty() {
        return json!({ "ok": false, "error": bad.join("\n") });
    }
    d.settings.set_top("customAgents", serde_json::to_value(&list).unwrap_or(Value::Null));
    // Шимы приводим сразу: агент должен быть запускаем в ту же секунду, а не
    // после перезапуска приложения.
    crate::install::sync_custom_shims(&crate::agents::shim_specs(&list));
    // Бинарь проверяем ПОСЛЕ сохранения и только предупреждением: человек
    // вправе вписать агента до того, как установил его на машину.
    let missing: Vec<String> = list
        .iter()
        .filter(|a| resolve_agent_bin(&a.bin).is_none())
        .map(|a| a.id.clone())
        .collect();
    json!({ "ok": true, "missing": missing })
}

/// Найдётся ли бинарь: абсолютный путь — проверкой файла, имя — поиском в PATH.
fn resolve_agent_bin(bin: &str) -> Option<std::path::PathBuf> {
    let bin = bin.trim();
    if bin.is_empty() {
        return None;
    }
    if bin.contains('/') {
        let p = std::path::PathBuf::from(bin);
        return p.is_file().then_some(p);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let p = std::path::Path::new(dir).join(bin);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[tauri::command]
pub async fn machines_list(app: AppHandle) -> Value {
    let _t = crate::log::Step::new("machines_list");
    // Паника внутри асинхронной команды убивает задачу, и вызов из панели не
    // завершается НИКОГДА — ни успехом, ни отказом. Раздел висит белым, и
    // отличить это от «пусто» нечем. Отказ честнее вечного ожидания.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| machines(&app)));
    match caught {
        Ok(v) => v,
        Err(_) => json!([{ "id": "local", "name": "Эта машина", "kind": "local", "online": true }]),
    }
}

fn machines(app: &AppHandle) -> Value {
    let d = Daemon::get(app);
    let mut out = vec![json!({
        "id": "local", "name": "Эта машина", "kind": "local", "online": true,
    })];
    for st in d.remotes.list() {
        out.push(json!({
            "id": st.name,
            "name": st.name,
            "kind": "remote",
            "sshHost": st.ssh_host,
            "online": st.connected,
            "error": st.error,
        }));
    }
    Value::Array(out)
}

/// История проектов выбранной машины. `machine` = `None`/`"local"` — эта.
///
/// У локальной история богатая (заголовки, модели, расход) — её собирает
/// сканер транскриптов. У удалённой берём оглавление с узла: каталоги, время
/// и идентификаторы сессий. Заголовков там нет и взяться им неоткуда без
/// вычитывания каждого транскрипта по ssh — а это уже не «показать список».
#[tauri::command]
pub async fn history_get(app: AppHandle, machine: Option<String>) -> Value {
    let _t = crate::log::Step::new("history_get");
    let d = Daemon::get(&app);
    let machine = machine.unwrap_or_default();
    if machine.is_empty() || machine == "local" {
        // См. `machines_list`: паника здесь оставила бы вкладку «Проекты»
        // белой навсегда, потому что обещание в панели не завершится.
        let d2 = d.clone();
        let mut projects = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            d2.history.projects(&d2.usage)
        }))
        .unwrap_or_else(|_| json!({ "error": "история не собралась — подробности в логе" }));
        apply_chat_names(&d, &mut projects);
        return projects;
    }
    let Some(node) = d.remotes.node(&machine) else {
        return json!([]);
    };
    let client = match node.client() {
        Ok(c) => c,
        Err(e) => return json!({ "error": format!("{e}: {}", node.why()) }),
    };
    match client.projects().await {
        Ok(list) => {
            let mut out = remote_projects_to_history(&machine, list);
            apply_chat_names(&d, &mut out);
            out
        }
        Err(e) => json!({ "error": ellipsize(&one_line(&e), 160) }),
    }
}

/// Имя, данное человеком, поверх заголовков истории. История собирает их сама
/// из транскриптов и про переименование не знает — а чат в «Проектах» тот же
/// самый, и называться в двух списках по-разному он не должен.
fn apply_chat_names(d: &Arc<Daemon>, projects: &mut Value) {
    overlay_names(projects, |id| d.chat_name(id));
}

/// Чистая часть наложения имён — источник имён отдельно, чтобы проверялось
/// без демона.
fn overlay_names(projects: &mut Value, name_of: impl Fn(&str) -> Option<String>) {
    let Some(arr) = projects.as_array_mut() else { return };
    for p in arr {
        let Some(sessions) = p.get_mut("sessions").and_then(Value::as_array_mut) else {
            continue;
        };
        for s in sessions {
            let Some(name) = s.get("id").and_then(Value::as_str).and_then(&name_of) else {
                continue;
            };
            s["name"] = json!(name);
            s["title"] = json!(name);
        }
    }
}

/// Оглавление узла → та же форма, что отдаёт локальная история, чтобы панель
/// рисовала оба списка одним кодом. Чего нет — того нет: заголовок сессии
/// заменяем её временем, а не выдумываем.
fn remote_projects_to_history(machine: &str, list: Value) -> Value {
    let Some(arr) = list.as_array() else { return json!([]) };
    let out: Vec<Value> = arr
        .iter()
        .map(|p| {
            let cwd = p.get("cwd").and_then(Value::as_str).unwrap_or_default();
            let project = cwd.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("другое");
            let sessions: Vec<Value> = p
                .get("sessions")
                .and_then(Value::as_array)
                .map(|s| {
                    s.iter()
                        .map(|x| {
                            let id = x.get("id").and_then(Value::as_str).unwrap_or_default();
                            json!({
                                // ключ реестра — с префиксом узла, как у событий:
                                // по нему панель узнает уже известную ей сессию
                                "id": format!("{machine}:{id}"),
                                "agentId": id,
                                "at": x.get("at").cloned().unwrap_or(Value::Null),
                                "title": "",
                                "remote": machine,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "project": project,
                "cwd": cwd,
                "count": p.get("count").cloned().unwrap_or(json!(sessions.len())),
                "lastAt": p.get("lastAt").cloned().unwrap_or(Value::Null),
                "remote": machine,
                "sessions": sessions,
            })
        })
        .collect();
    Value::Array(out)
}

#[tauri::command]
pub fn usage_session(app: AppHandle, id: String) -> Value {
    Daemon::get(&app)
        .usage
        .for_session(&id)
        .unwrap_or(Value::Null)
}

/* ================= управление сессией ================= */

/// Дать чату своё имя (или снять его пустой строкой) — общее ядро для панели и
/// капабилити `sessions.rename`. Отказ всегда с причиной: молча не переименовать
/// хуже, чем не переименовать вслух.
pub(crate) fn rename_core(d: &Arc<Daemon>, session_id: &str, title: &str) -> Value {
    match d.rename_chat(session_id, title) {
        Ok((name, shown)) => {
            crate::log::line(&format!(
                "[rename] чат {} → {}",
                ellipsize(session_id, 8),
                name.as_deref().unwrap_or("автозаголовок")
            ));
            json!({ "ok": true, "name": name, "title": shown })
        }
        Err(e) => err(e),
    }
}

#[tauri::command]
pub async fn session_rename(app: AppHandle, session_id: String, title: String) -> Value {
    let d = Daemon::get(&app);
    via_gate_panel(
        &d,
        "sessions.rename",
        json!({ "session_id": session_id, "title": title }),
    )
    .await
}

#[tauri::command]
pub fn session_set_pin(app: AppHandle, session_id: String, pinned: bool) -> Value {
    let d = Daemon::get(&app);
    let found = d.with_session(&session_id, |s| s.pinned = pinned);
    if found {
        d.push();
    }
    json!({ "ok": found })
}

/// Завершить сессию: закрыть пану, если она ещё жива, и убрать сессию из
/// списка в любом случае.
///
/// Два случая — один жест. Живая сессия: агент работает, человек решил, что
/// хватит; паны не станет вместе с ним. Зомби: агент давно умер — терминал
/// закрыли, машина ушла в сон, узел отвалился, — `session-end` не пришёл, и
/// сессия висит «в работе» навсегда. Сверка живости её не снимет: у сессии с
/// недоступного узла судить не по чему, а у сессии без паны и без pid — нечем.
/// До сих пор такую сессию нельзя было убрать вообще ничем.
///
/// Порядок важен: сперва пана, потом реестр. Если пана жива, но закрыть её не
/// вышло, — сессию НЕ забываем: список без строки и живой агент за спиной хуже
/// висящей строки.
#[tauri::command]
pub async fn session_kill(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    kill_core(&d, &session_id).await
}

/// Попросить процесс завершиться (SIGTERM). `false` — процесса уже нет.
///
/// Только для местных сессий: pid с чужой машины здесь не значит ничего и
/// вполне может совпасть с чужим живым процессом.
fn signal_term(pid: i64) -> bool {
    // SAFETY: обычный вызов kill(2); опасен он не памятью, а последствиями —
    // поэтому и зовётся только по явной команде человека.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) == 0 }
}

pub(crate) async fn kill_core(d: &Arc<Daemon>, session_id: &str) -> Value {
    let Some(s) = d.session(session_id) else {
        return err("Сессия не найдена");
    };
    let mut killed = false;
    let mut note = String::new();
    if let Some(pane) = s.tmux_pane.clone() {
        // «Не смогли спросить» — не «пана мертва». Туннель моргнул, tmux не
        // отозвался: агент на той стороне жив, работает и жжёт токены. Строку
        // убираем (за этим и звали), но говорим вслух — вернётся он сам, первым
        // же своим событием.
        match d.pane_target(&s) {
            Ok(target) => match target.pane_state(&pane).await {
                Ok(true) => {
                    if let Err(e) = target.kill(&pane).await {
                        return err(format!(
                            "Не удалось закрыть терминал сессии: {}",
                            ellipsize(&one_line(&e), 120)
                        ));
                    }
                    killed = true;
                }
                Ok(false) => {}
                Err(e) => note = format!("{e} — агент мог остаться работать"),
            },
            Err(e) => note = format!("{e} — агент мог остаться работать"),
        }
    } else if s.remote.is_none() {
        // Сессия не в tmux (терминал IDE): закрывать нечего, но агент жив и
        // после «завершить» обязан завершиться — иначе кнопка просто прячет
        // строку, а работа продолжается за спиной. Просим по-хорошему:
        // SIGTERM, а не SIGKILL — claude успеет закрыть транскрипт.
        if let Some(pid) = s.pid.filter(|p| *p > 0) {
            killed = signal_term(pid);
        }
    }
    d.sessions.lock().unwrap().remove(session_id);
    d.push();
    crate::log::line(&format!(
        "[kill] сессия {} — {}",
        ellipsize(session_id, 8),
        if killed { "агент остановлен" } else { "убрана из списка" }
    ));
    json!({ "ok": true, "killed": killed, "note": note })
}

/// Пульт: слэш-команда с аргументом в живую пану + оптимистичное состояние.
pub(crate) async fn set_via_slash(
    d: &Arc<Daemon>,
    session_id: &str,
    slash: String,
    apply: impl FnOnce(&mut crate::model::Session),
) -> Value {
    let Some(s) = d.session(session_id) else {
        return err("Сессия не найдена");
    };
    let target = match d.pane_target(&s) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let Some(pane) = s.tmux_pane.clone() else {
        return tmux_needed(&s);
    };
    match target.pane_state(&pane).await {
        Ok(true) => {}
        Ok(false) => return tmux_needed(&s),
        // Спросить не вышло — не выдаём это за «сессия вне tmux»: подсказка
        // «подними её заново» увела бы человека чинить не то.
        Err(e) => return err(e),
    }
    match target.paste_slash(&pane, &slash).await {
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
    via_gate_panel(
        &d,
        "sessions.control",
        json!({ "session_id": session_id, "model": model }),
    )
    .await
}

/// Ядро смены модели — общее для IPC и капабилити `sessions.control` (инкр. 8).
/// Claude: слэш `/model <id>` (+ confirm). Codex: `/model` открывает объединённый
/// пикер модель+reasoning (отдельного `/effort` нет) — слэш с аргументом best-effort.
pub(crate) async fn set_model_core(d: &Arc<Daemon>, session_id: &str, model: &str) -> Value {
    let agent = d
        .session(session_id)
        .map(|s| crate::backend::Agent::from_opt(s.agent.as_deref()))
        .unwrap_or_default();
    // Валидация модели — политика бэкенда (SEC-3: недоверенный голос не должен
    // пастить свободный текст в `/model …`). Claude ограничен аллоулистом, Codex —
    // только «чистотой» строки, потому что его набор моделей дрейфует.
    let be = crate::backend::backend(agent);
    if let Err(e) = be.validate_model(model) {
        return err(e);
    }
    let friendly = be.friendly_model(model);
    set_via_slash(d, session_id, format!("/model {model}"), move |s| {
        s.model = Some(friendly); // оптимистично; транскрипт подтвердит
        s.model_at = Some(now_ms());
    })
    .await
}

#[tauri::command]
pub async fn session_set_effort(app: AppHandle, session_id: String, level: String) -> Value {
    let d = Daemon::get(&app);
    via_gate_panel(
        &d,
        "sessions.control",
        json!({ "session_id": session_id, "effort": level }),
    )
    .await
}

/// Ядро смены effort — общее для IPC и капабилити `sessions.control` (инкр. 8).
/// У Codex отдельного `/effort` НЕТ (reasoning меняется внутри `/model`-пикера),
/// поэтому для codex-сессии это не-операция с понятным сообщением; UI и так
/// прячет effort-пикер (has_separate_effort=false).
pub(crate) async fn set_effort_core(d: &Arc<Daemon>, session_id: &str, level: &str) -> Value {
    let agent = d
        .session(session_id)
        .map(|s| crate::backend::Agent::from_opt(s.agent.as_deref()))
        .unwrap_or_default();
    let be = crate::backend::backend(agent);
    if !be.has_separate_effort() {
        return err("Codex: reasoning effort меняется через /model-пикер (отдельной команды нет)");
    }
    if let Err(e) = be.validate_effort(level) {
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
    let Some(s) = d.session(&session_id) else {
        return err("Сессия не найдена");
    };
    // popup рисуется в подключённом клиенте tmux — у удалённой сессии он на
    // той машине, и увидит его тот, кто сидит за ней, а не мы
    if let Some(name) = &s.remote {
        return err(format!("Сессия идёт на узле «{name}» — показывать оверлей некому"));
    }
    let Some(pane) = s.tmux_pane else {
        return err("Сессия не в tmux — показать её терминал нечем");
    };
    match tmux::ping(&pane).await {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

/// Разбор payload'а ответа на вопрос(ы): выборы по вопросам + свои тексты.
/// Новый контракт `{ answers: number[][], texts?: (string|null)[] }`; обратная
/// совместимость: `{ indices }` → `answers = [indices]`, без texts. Пустые и
/// пробельные тексты приводятся к None — «нет кастома».
fn parse_question_choice(choice: &Value) -> (Vec<Vec<u32>>, Vec<Option<String>>) {
    // парсинг массива выборов вопроса в Vec<u32> (1-based, >0)
    let parse_row = |v: &Value| -> Vec<u32> {
        v.as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_u64)
                    .filter(|&n| n >= 1)
                    .map(|n| n as u32)
                    .collect()
            })
            .unwrap_or_default()
    };

    // новый контракт answers[][] либо старый indices[] → [indices]
    let answers: Vec<Vec<u32>> = if let Some(rows) = choice.get("answers").and_then(Value::as_array)
    {
        rows.iter().map(parse_row).collect()
    } else if let Some(idx) = choice.get("indices") {
        vec![parse_row(idx)]
    } else {
        Vec::new()
    };

    let texts: Vec<Option<String>> = choice
        .get("texts")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|v| {
                    v.as_str()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    (answers, texts)
}

/// Ответ на AskUserQuestion/пикер клавишами в пану.
/// `choice` = `{ answers: number[][], texts?: (string|null)[] }` (answers[i] —
/// опции 1-based вопроса i, texts[i] — свой ответ строкой «Other», только Claude).
/// Обратная совместимость: `{ indices, multiSelect }` → `answers = [indices]`.
#[tauri::command]
pub async fn question_answer(app: AppHandle, session_id: String, choice: Value) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else {
        return err("Вопрос уже неактуален");
    };
    let Some(q) = s.question.clone() else {
        return err("Вопрос уже неактуален");
    };
    let Some(pane) = s.tmux_pane.clone() else {
        return err("Сессия вне tmux — ответь в терминале");
    };
    let target = match d.pane_target(&s) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    match target.pane_state(&pane).await {
        Ok(true) => {}
        Ok(false) => return err("Терминал сессии закрыт — ответить некуда"),
        Err(e) => return err(e),
    }

    let (answers, texts) = parse_question_choice(&choice);

    if answers.iter().all(Vec::is_empty) && texts.iter().all(Option::is_none) {
        return err("Пустой выбор");
    }
    // валидация: на каждый вопрос — выбор в пределах его опций либо свой текст
    for (i, item) in q.questions.iter().enumerate() {
        let row = answers.get(i).map(Vec::as_slice).unwrap_or(&[]);
        if row.is_empty() && texts.get(i).map_or(true, Option::is_none) {
            return err("Не на все вопросы выбран ответ");
        }
        let max = item.options.len() as u32;
        if row.iter().any(|&n| n > max) {
            return err("Выбран несуществующий вариант");
        }
    }

    let agent = crate::backend::Agent::from_opt(s.agent.as_deref());
    // если в пикере агента нет строки «Other» — свой текст доставить некуда
    if !crate::backend::backend(agent).supports_custom_answer() && texts.iter().any(Option::is_some)
    {
        return err(format!(
            "Свой ответ недоступен в {}-сессии — выбери вариант",
            agent.label()
        ));
    }
    match target.answer_question(&pane, agent, &q, &answers, &texts).await {
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
        None => err("Неизвестное действие"),
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
        "bluetoothOnly": cfg.bluetooth_only,
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
    d.voice
        .test_phrase("Так звучит выбранная скорость. Пиксела закончила, изменён один файл.");
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

/// Тумблер «озвучивать только при Bluetooth-гарнитуре» — сохранить в voice.
#[tauri::command]
pub fn voice_set_bluetooth_only(app: AppHandle, on: bool) {
    let mut patch = serde_json::Map::new();
    patch.insert("bluetoothOnly".into(), Value::Bool(on));
    Daemon::get(&app).settings.set_voice(patch);
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
    via_gate_panel(
        &d,
        "sessions.reply",
        json!({ "session_id": session_id, "text": text }),
    )
    .await
}

/// Сохранить вставленную в поле ответа картинку во временный файл и вернуть
/// абсолютный путь. Доставка картинок агенту (Claude/Codex) — ссылкой на файл:
/// путь уходит в промпт обычным текстом, TUI его читает и подгружает картинку.
/// `data_base64` — содержимое без префикса `data:…;base64,`; `ext` — расширение.
#[tauri::command]
pub async fn session_save_image(data_base64: String, ext: String) -> Value {
    use base64::Engine as _;
    let bytes = match base64::engine::general_purpose::STANDARD.decode(data_base64.trim()) {
        Ok(b) => b,
        Err(e) => return err(format!("base64: {e}")),
    };
    if bytes.is_empty() {
        return err("Пустая картинка");
    }
    // Защита от мусора в буфере обмена: не пишем гигантские блобы на диск.
    if bytes.len() > 25 * 1024 * 1024 {
        return err("Картинка больше 25 МБ");
    }
    // Разрешаем только безопасное короткое расширение из белого списка.
    let ext = match ext.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
        "png" => "png",
        "jpg" | "jpeg" => "jpg",
        "gif" => "gif",
        "webp" => "webp",
        "bmp" => "bmp",
        "heic" => "heic",
        "tiff" | "tif" => "tiff",
        _ => "png",
    };
    let dir = std::env::temp_dir().join("jarvis-paste");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("temp: {e}"));
    }
    // Каталог никто больше не чистит — подметаем старьё сами (агент читает файл
    // вскоре после отправки; трое суток — с большим запасом).
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 24 * 3600);
        for e in entries.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).map(|t| t < cutoff).unwrap_or(false);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    // Уникальное имя без коллизий в пределах одной мс — счётчик процесса.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!("img-{}-{}.{}", now_ms(), seq, ext));
    if let Err(e) = std::fs::write(&path, &bytes) {
        return err(format!("write: {e}"));
    }
    json!({ "ok": true, "path": path.to_string_lossy() })
}

/// Продолжить сессию (кнопка на тосте / хоткей): послать «продолжай» — например
/// после прерывания сном. Под капотом — обычная доставка в пану.
#[tauri::command]
pub async fn session_continue(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    via_gate_panel(
        &d,
        "sessions.reply",
        json!({ "session_id": session_id, "text": "продолжай" }),
    )
    .await
}

/// Ядро отправки в сессию — общее для IPC-команды панели и капабилити
/// `sessions.reply` (инкр. 8). Форма ответа панельная: {ok:true, channel,…} /
/// {ok:false, error} / {ok:false, needsTmux, resumeCmd}.
pub(crate) async fn reply_core(d: &Arc<Daemon>, session_id: String, text: String) -> Value {
    let Some(s) = d.session(&session_id) else {
        return err("Сессия не найдена");
    };
    let prompt = text.trim().to_string();
    if prompt.is_empty() {
        return err("Пустой текст");
    }
    // Сессия с узла — вставка уезжает туда же по ssh; дальше логика доставки
    // (ack, очередь, ретрай) одна и та же.
    let target = match d.pane_target(&s) {
        Ok(t) => t,
        Err(e) => return err(e),
    };

    if let Some(pane) = s.tmux_pane {
        // «Не смог спросить» и «паны нет» — разные вещи. Если tmux вообще не
        // запускается или узел не отвечает, опрос живости провалится на ЛЮБОЙ
        // пане, и стереть её — значит своей же ошибкой сделать живую сессию
        // неуправляемой навсегда (до перезапуска агента). Лучше честная ошибка.
        let alive = match target.pane_state(&pane).await {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        if alive {
            // Занята ли сессия в момент отправки. Если да — Claude Code положит
            // наш ввод в СВОЮ очередь, а prompt-хук придёт лишь когда он до него
            // дойдёт (после текущего ответа). Быстрый ack тогда невозможен — это
            // не провал доставки, а «поставлено в очередь». Limit — тоже ждёт.
            let busy = matches!(s.status, Status::Working | Status::Limit);

            // Первая вставка.
            let t0 = now_ms();
            let t_reply = crate::metrics::now();
            if let Err(e) = target.reply(&pane, &prompt).await {
                eprintln!("[jarvis] reply tmux fail: {e}");
                return err(format!("tmux: {}", ellipsize(&one_line(&e), 120)));
            }

            // Свободная сессия обработает сразу — ждём короткое подтверждение.
            if d.await_prompt_ack(&session_id, t0, std::time::Duration::from_millis(2500))
                .await
            {
                d.mark_prompt_sent(&session_id, &prompt);
                crate::log::line(&format!(
                    "[reply] доставлено sid={} pane={pane}",
                    ellipsize(&session_id, 8)
                ));
                crate::metrics::record("reply_ack", t_reply, json!({ "queued": false }));
                return json!({ "ok": true, "channel": "tmux" });
            }
            crate::metrics::record("reply_ack", t_reply, json!({ "queued": busy }));

            if busy {
                // Сессия работала — ввод ушёл в нативную очередь Claude Code.
                // НЕ ретраим вставку (повтор продублировал бы сообщение в очереди).
                // Подтверждаем асинхронно: когда Claude дойдёт до ввода, прилетит
                // prompt-хук — тогда и отметим доставку «из очереди».
                crate::log::line(&format!(
                    "[reply] в очереди (сессия занята) sid={} pane={pane}",
                    ellipsize(&session_id, 8)
                ));
                let d2 = d.clone();
                let sid2 = session_id.clone();
                let p2 = prompt.clone();
                tauri::async_runtime::spawn(async move {
                    if d2
                        .await_prompt_ack(&sid2, t0, std::time::Duration::from_secs(300))
                        .await
                    {
                        d2.mark_prompt_sent(&sid2, &p2);
                        crate::log::line(&format!(
                            "[reply] доставлено из очереди sid={}",
                            ellipsize(&sid2, 8)
                        ));
                    } else {
                        crate::log::line(&format!(
                            "[reply] очередь: 5 мин без подтверждения sid={}",
                            ellipsize(&sid2, 8)
                        ));
                    }
                });
                return json!({ "ok": true, "channel": "tmux", "queued": true });
            }

            // Свободная сессия, но ack не пришёл — вставка могла не успеть
            // зарегистрироваться. Один ретрай (C-u в reply() чистит строку,
            // повтор не задваивает текст).
            let t1 = now_ms();
            if let Err(e) = target.reply(&pane, &prompt).await {
                return err(format!("tmux: {}", ellipsize(&one_line(&e), 120)));
            }
            if d.await_prompt_ack(&session_id, t1, std::time::Duration::from_millis(2500))
                .await
            {
                d.mark_prompt_sent(&session_id, &prompt);
                crate::log::line(&format!(
                    "[reply] доставлено sid={} pane={pane} (2-я попытка)",
                    ellipsize(&session_id, 8)
                ));
                return json!({ "ok": true, "channel": "tmux", "attempts": 2 });
            }
            return err("Агент не подтвердил получение — проверь терминал");
        }
        d.with_session(&session_id, |s| s.tmux_pane = None); // пана умерла
        d.push();
    }
    match d.session(&session_id) {
        Some(s) => tmux_needed(&s),
        None => err("Сессия не найдена"),
    }
}

/// Лесенка «показать терминал»: tmux → вкладка по tty (Terminal/iTerm2) →
/// GUI-приложение-владелец. Нижняя ступень — не тост, а чат сессии в панели:
/// renderer открывает его сам при ok:false + fallbackChat.
#[tauri::command]
pub async fn terminal_focus(app: AppHandle, session_id: String) -> Value {
    let d = Daemon::get(&app);
    let Some(s) = d.session(&session_id) else {
        return err("Сессия не найдена");
    };
    // Терминал удалённой сессии — на другой машине. Вся лесенка ниже (tmux,
    // tty, GUI-владелец) искала бы его здесь и в лучшем случае не нашла бы
    // ничего, а в худшем подняла бы чужое окно с совпавшим id паны.
    if let Some(name) = &s.remote {
        return err(format!("Сессия идёт на узле «{name}» — её терминал не на этой машине"));
    }

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

/// Запуск сессии прямо из вкладки «Проекты»: открыть терминал из настроек,
/// (опц.) выполнить прокси-команду, затем `claude`/`codex` в директории `cwd`.
/// `session_id == None` → новая сессия; иначе `--resume`/`resume`. Параметры
/// запуска (терминал, прокси-команда, «опасный режим») берутся из настроек.
///
/// `container` — запуск агента в докере (вторая изоляция из Air: worktree
/// разводит файлы, контейнер — инструменты и зависимости).
///
/// `isolate` и `mode` — свойства ЗАДАЧИ, а не настройки на все разом: поднять
/// ли её в отдельном worktree-песочнице и с каким доверием («ask» | «plan» |
/// «yolo»). Разведать чужой код и переписать свой требуют разного.
///
/// `task` — текст, который уедет агенту, как только он встанет. Без него
/// «поставить задачу» — это два шага (подними, потом найди чат и напиши), и
/// именно на втором работа откладывается «на потом».
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_launch(
    app: AppHandle,
    cwd: Option<String>,
    agent: String,
    session_id: Option<String>,
    machine: Option<String>,
    isolate: Option<bool>,
    mode: Option<String>,
    task: Option<String>,
    container: Option<bool>,
) -> Value {
    let d = Daemon::get(&app);
    launch_core(
        &d,
        LaunchReq { cwd, agent, session_id, machine, isolate, mode, task, container, bind: None },
    )
    .await
}

/// Что просят поднять. Одна структура на оба пути запуска — панель
/// (`session_launch`) и капабилити `sessions.spawn`: второй реализации запуска
/// в проекте нет, различие ровно одно — `bind`.
#[derive(Default)]
pub(crate) struct LaunchReq {
    pub cwd: Option<String>,
    pub agent: String,
    pub session_id: Option<String>,
    pub machine: Option<String>,
    pub isolate: Option<bool>,
    pub mode: Option<String>,
    pub task: Option<String>,
    pub container: Option<bool>,
    /// Учёт родителя (`sessions.spawn`): имя, модель, кто поднял и зачем.
    /// `None` — ручной запуск человеком, учитывать нечего.
    pub bind: Option<crate::capability::native::spawn::Bind>,
}

/// Общее ядро запуска. Возвращает управление, как только терминал открыт: имя,
/// модель и первый промпт доезжают фоном, когда сессия появится в реестре.
pub(crate) async fn launch_core(d: &Arc<Daemon>, req: LaunchReq) -> Value {
    let LaunchReq { cwd, agent, session_id, machine, isolate, mode, task, container, bind } = req;
    // cwd бывает null: история группирует сессии без директории в «другое».
    // Resume без cwd допустим (как прежнее «скопировать команду» без cd),
    // а вот новая сессия без директории бессмысленна.
    let cwd = cwd.unwrap_or_default();
    if cwd.trim().is_empty() && session_id.is_none() {
        return err("Не указана директория проекта");
    }
    let machine = machine.unwrap_or_default();
    let mode = crate::launch::Mode::parse(mode.as_deref().unwrap_or(""));
    // Песочница — только для НОВОЙ задачи: продолжение живёт там, где начиналось,
    // и переносить его в свежий worktree значило бы оторвать от своей работы.
    let cwd = if isolate.unwrap_or(false) && session_id.is_none() {
        let host = if machine.is_empty() || machine == "local" {
            Host::Local
        } else {
            match d.remotes.node(&machine) {
                Some(node) => Host::Ssh {
                    machine: machine.clone(),
                    host: node.cfg.ssh_host.clone(),
                },
                None => return err(format!("Узел «{machine}» не подключён")),
            }
        };
        match sandbox_for(&host, &cwd).await {
            Ok(dir) => dir,
            Err(e) => return err(e),
        }
    } else {
        cwd
    };
    if !machine.is_empty() && machine != "local" {
        let res = launch_on_node(d, &machine, &cwd, &agent, session_id.as_deref(), mode).await;
        if res.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            deliver_task(d, &machine, &cwd, &agent, task, bind);
        }
        return res;
    }
    // Новый проект на этой машине: каталога может ещё не быть, и требовать
    // сходить создать его руками — значит не сделать работу.
    if !cwd.trim().is_empty() {
        if let Err(e) = std::fs::create_dir_all(cwd.trim()) {
            return err(format!("не создал {}: {e}", cwd.trim()));
        }
    }
    // Контейнер — вторая изоляция из Air: worktree разводит файлы, docker —
    // инструменты. Образ задаётся в настройках: своего мы не собираем, а
    // угадывать чужой нельзя.
    let in_docker = container.unwrap_or(false);
    let image = d.settings.string("launchDockerImage");
    if in_docker && image.trim().is_empty() {
        return err("для запуска в контейнере укажи образ в настройках («Запуск» → образ контейнера)");
    }
    if in_docker && !crate::launch::has_docker().await {
        return err("docker не найден на этой машине");
    }
    let terminal = d.settings.string("launchTerminal");
    let custom = d.settings.string("launchCustomCmd");
    let proxy = d.settings.string("launchProxyCmd");
    // Режим задачи сильнее общей настройки: человек выбрал его для ЭТОЙ работы.
    let dangerous = mode == crate::launch::Mode::Yolo || d.settings.bool("launchDangerous");

    // Свой агент из реестра — раньше зашитой пары: команду для него собирает
    // реестр (шим + шаблон возобновления человека), а не наши догадки.
    let customs = crate::agents::parse(&d.settings.load());
    let agent_cmd = match crate::agents::find(&customs, &agent) {
        Some(a) => crate::agents::command(a, session_id.as_deref(), dangerous),
        None => crate::launch::agent_command_mode(
            &agent,
            session_id.as_deref(),
            if dangerous {
                crate::launch::Mode::Yolo
            } else {
                mode
            },
        ),
    };
    // PATH запускаемой команды достраиваем сами: терминал выполняет её в
    // неинтерактивном шелле, где PATH-блока Jarvis (и шима) ещё нет.
    let path_dirs = crate::launch::launch_path_dirs();
    let agent_cmd = if in_docker {
        crate::launch::docker_command(
            image.trim(),
            &cwd,
            &crate::util::home_dir().to_string_lossy(),
            &agent_cmd,
        )
    } else {
        agent_cmd
    };
    // Kimi и claude в незнакомом каталоге спрашивают про доверие и ЖДУТ клавишу —
    // сессия, поднятая агентом, вставала на этом молча. Каталог тут уже
    // окончательный: worktree песочницы создан выше.
    crate::launch::prepare_workspace(&agent, &cwd);
    let inner = crate::launch::inner_command(&cwd, &proxy, &agent_cmd, &path_dirs);
    match crate::launch::spawn(&terminal, &custom, &inner).await {
        Ok(()) => {
            deliver_task(d, "", &cwd, &agent, task, bind);
            ok()
        }
        Err(e) => err(e),
    }
}

/// Отдать задачу агенту, как только он встанет.
///
/// Здесь остался только вызов: механика — в `launch::ready`. Причина не в
/// размере, а в том, что порядков запуска ДВА и они несовместимы. Claude и
/// Codex заводят сессию сами, и ждать её в реестре — правильно. Kimi сессию до
/// первой реплики не заводит вовсе («No session yet — one will be created on
/// your first message»), и прежнее ожидание было взаимной блокировкой: мы ждали
/// сессию, чтобы отдать реплику, а сессии не было, пока не было реплики. Четыре
/// подъёма подряд не встали ни разу, три задачи человека не были сделаны.
///
/// `agent` добавлен сюда именно за этим: порядок запуска — свойство агента, а
/// не наша догадка.
fn deliver_task(
    d: &Arc<Daemon>,
    machine: &str,
    cwd: &str,
    agent: &str,
    task: Option<String>,
    bind: Option<crate::capability::native::spawn::Bind>,
) {
    crate::launch::ready::deliver(d, machine, cwd, agent, task, bind);
}

/// Запуск на удалённой машине. Терминала там нет и открывать нечего: сессия
/// поднимается в `tmux -L jarvis` отсоединённой, и дальше живёт как любая
/// другая удалённая — статусы и чат приезжают хуками через узел.
///
/// Идентификатор сессии для `--resume` отдаём БЕЗ префикса узла: префикс —
/// ключ нашего реестра, агент на той машине про него не знает.
async fn launch_on_node(
    d: &Arc<Daemon>,
    machine: &str,
    cwd: &str,
    agent: &str,
    session_id: Option<&str>,
    mode: crate::launch::Mode,
) -> Value {
    let Some(node) = d.remotes.node(machine) else {
        return err(format!("Узел «{machine}» не подключён"));
    };
    let client = match node.client() {
        Ok(c) => c,
        Err(e) => return err(format!("{e}: {}", node.why())),
    };
    let bare = session_id.map(|s| s.strip_prefix(&format!("{machine}:")).unwrap_or(s));
    let dangerous = d.settings.bool("launchDangerous");
    // На узле шима своего агента нет — без него не будет ни tmux, ни хуков
    // жизненного цикла, и сессия молча не появилась бы. Честный отказ лучше.
    if crate::agents::find(&crate::agents::parse(&d.settings.load()), agent).is_some() {
        return err("свои агенты пока запускаются только на этой машине — на узле нет их шима");
    }
    let cmd = crate::launch::agent_command_mode(
        agent,
        bare,
        if dangerous {
            crate::launch::Mode::Yolo
        } else {
            mode
        },
    );
    let name = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("project");
    match client.launch(cwd, &cmd, name).await {
        Ok(()) => json!({ "ok": true, "channel": "node", "machine": machine }),
        Err(e) => err(format!(
            "{}\nЕсли не хватает tmux или агента — поставь их на той машине.",
            ellipsize(&one_line(&e), 200)
        )),
    }
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
///
/// `armed` — признаки того, что нажимал человек, а не подброшенный клик
/// (карточка пожила на экране, курсор к ней ехал, окно не подняли только что;
/// считает `ui/agent-chat.js`). Проверяющий CLI слал синтетический ввод в живое
/// окно, а такой клик способен нажать «Разрешить» и согласиться за человека —
/// обойти ровно тот гейт, через который агент и спрашивает разрешение.
///
/// Асимметрия намеренная: без признаков не проходит только СОГЛАСИЕ. Отказ
/// принимается всегда — заблокировать «Отклонить» значит запереть человека
/// наедине с карточкой, а подброшенный отказ в худшем случае стоит одного хода.
///
/// Это второй рубеж, а не замок: тот, кто синтезирует ещё и движение курсора,
/// подделает и признаки. Настоящий запрет стоит у источника — в шимах, через
/// которые запускаются CLI.
#[tauri::command]
pub fn agent_confirm(app: AppHandle, nonce: String, approved: bool, armed: Option<bool>) -> Value {
    let d = Daemon::get(&app);
    if !crate::capability::gate::decision_allowed(approved, armed) {
        // Громко: в журнал и в ответ. Тихо отклонённое согласие выглядело бы
        // для человека как «нажал и ничего», а для агента — как молчание.
        crate::capability::gate::note_unarmed(&crate::capability::audit::FileAudit, &nonce);
        return json!({
            "ok": false,
            "code": "not-armed",
            "error": "согласие не принято: нет признаков, что нажимал человек"
        });
    }
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

/// Голосовой разговор: крестик в HUD = «стоп всё» — оборвать текущую озвучку И
/// завершить разговор (цикл выйдет, listen прервётся, мик закроется). Плюс
/// снимаем висящие confirm/stage, чтобы ничего не сработало после.
#[tauri::command]
pub fn voice_abort(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    d.convo_abort
        .store(true, std::sync::atomic::Ordering::SeqCst);
    d.voice.stop(); // оборвать речь + очистить очередь TTS
                    // HUD убираем ТИХО (Phase::Dismiss), БЕЗ тоста «Отменено»: × — это «закрой/
                    // останови», а не «отмена действия»; «Отменено» на каждый крестик раздражает.
    crate::route::hud::emit(&d, crate::route::hud::Phase::Dismiss);
    json!({ "ok": true })
}

/* ================= служебное ================= */

/// Снять ложный лимит-баннер по официальному usage (таймер из main).
pub fn reconcile_limit(d: &Arc<Daemon>) {
    limits::reconcile(d);
}

/* ================= агент-хост (фаза 5) ================= */

/// Отправить сообщение агенту и немедленно вернуть `{ok:true, chatId}`.
///
/// Потоковые события поступают через канал `agent:event` (тип `AgentEvent` плюс
/// метка `chatId`, см. `agent::TaggedEvent`). Канал один на все чаты, поэтому
/// окно раскладывает поток по метке — и переключаться во время хода можно.
/// `chatId` в ответе — тот чат, которым помечен ход: адресата выбирает ядро
/// (окно могло прислать только id разговора), и знать его окно должно сразу.
/// `session_id` — необязателен; при наличии используется для возобновления (--resume).
#[tauri::command]
pub async fn agent_send(
    app: AppHandle,
    message: String,
    chat_id: Option<String>,
    session_id: Option<String>,
) -> Value {
    use crate::agent::ClaudeCliHost;
    use crate::capability::{build_registry, grant::Consumer};
    use crate::util::jarvis_dir;

    let mcp_config = jarvis_dir()
        .join("jarvis-mcp.json")
        .to_string_lossy()
        .to_string();

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

    // Чат выбираем ЗДЕСЬ и один раз: ход длится минуты, за это время человек
    // уходит в другой проект — «текущий на момент ответа» записал бы нить не туда.
    // Нить берём из чата, а не из окна: окно живёт до закрытия, разговор дольше.
    let book = crate::agent::chat_book(&app);
    let chat = match crate::agent::chat_for_send(&book, chat_id.as_deref(), session_id.as_deref()) {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let chat_id = chat.id.clone();
    let resume = chat.session_id.clone();

    // Выбор хоста по доступности («auto»): Claude (жёсткий INV-TOOLS на init) если
    // есть, иначе Codex (чистый CODEX_HOME + обязательный per-item kill).
    if crate::claude_bin::resolve_claude_bin().is_some() {
        let host = ClaudeCliHost {
            app: app.clone(),
            mcp_config,
            chat_id: chat_id.clone(),
        };
        tauri::async_runtime::spawn(async move {
            host.run(&message, &tools, resume.as_deref()).await;
        });
    } else if crate::backend::codex::resolve_codex_bin().is_some() {
        let Some((mcp_bin, token)) = read_mcp_bin_token(&mcp_config) else {
            return err(
                "Не прочитал ~/.jarvis/jarvis-mcp.json — Codex-агенту нечем говорить \
                 с Jarvis. Нажми «Переустановить» в настройках, карточка «Интеграция»",
            );
        };
        let host = crate::backend::codex_agent::CodexCliHost {
            app: app.clone(),
            mcp_bin,
            token,
            chat_id: chat_id.clone(),
        };
        tauri::async_runtime::spawn(async move {
            host.run(&message, &tools, resume.as_deref()).await;
        });
    } else {
        return err("Не нашёл ни claude, ни codex — поставь один из них, и агент заработает");
    }

    json!({ "ok": true, "chatId": chat_id })
}

/// Достать (путь к jarvis-mcp, токен агента) из jarvis-mcp.json — для Codex-хоста,
/// который инжектит MCP-сервер через `-c`, а не файлом.
pub(crate) fn read_mcp_bin_token(mcp_config: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(mcp_config).ok()?).ok()?;
    let j = v.get("mcpServers")?.get("jarvis")?;
    let bin = j.get("command")?.as_str()?.to_string();
    let token = j.get("env")?.get("JARVIS_TOKEN")?.as_str()?.to_string();
    Some((bin, token))
}

/// Открыть (или сфокусировать) окно чата с агентом (фаза 7).
#[tauri::command]
pub fn agent_chat_window(app: AppHandle) {
    let _ = windows::create_agent_chat(&app);
}

/// Каталог транскриптов главного агента.
///
/// Хост работает из временной папки (`agent/mod.rs`, `current_dir(temp_dir())`),
/// поэтому каталог проекта считаем от неё — тем же механизмом, что у обычных
/// сессий. Своего резолва пути не городим: `project_dir_for` уже разбирается с
/// симлинком `/var/folders` → `/private/var/folders`.
fn agent_transcript_dir() -> Option<std::path::PathBuf> {
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    crate::backend::backend(crate::backend::Agent::Claude).transcript_dir_for(&cwd)
}

/// Разговоры, найденные на диске. Их отсутствие — не ошибка: каталога может не
/// быть вовсе (агента ещё ни разу не запускали).
fn agent_threads() -> Vec<crate::agent::history::Thread> {
    agent_transcript_dir()
        .map(|d| crate::agent::history::scan(&d))
        .unwrap_or_default()
}

/// Состояние открытого чата: id разговора, который продолжится (null — начнём
/// новый), плюс сам чат. Окно рисует по нему пометку о продолжении.
#[tauri::command]
pub fn agent_chat_state(app: AppHandle) -> Value {
    let book = crate::agent::chat_book(&app);
    let c = book.current();
    // Имя — то же, что в списке: чип и строка списка не должны звать один чат
    // по-разному. Безымянному подставится первая реплика разговора.
    let preview = c
        .session_id
        .as_deref()
        .and_then(|sid| agent_threads().into_iter().find(|t| t.session_id == sid))
        .map(|t| t.preview)
        .unwrap_or_default();
    json!({
        "sessionId": c.session_id,
        "chatId": c.id,
        "name": crate::agent::history::display_name(c.human_name(), &preview),
        "named": c.human_name().is_some(),
    })
}

/* ----- список разговоров: по чату на проект ----- */

/// Ответ всех команд списка: сразу весь список с пометкой открытого. Отдавать
/// «ok» и ждать, что окно само сходит за списком, — лишний круг и рассинхрон.
///
/// Список сшивается с диском: настройки знают имена и порядок, а какие разговоры
/// вообще были — знают только транскрипты.
fn chat_book_json(book: &crate::agent::ChatBook) -> Value {
    let threads = agent_threads();
    json!({
        "ok": true,
        "current": book.current().id,
        "chats": crate::agent::history::chats_json(book, &threads),
        // Скрытых в списке нет — окну нужно чем-то нарисовать «скрыто N · вернуть».
        "hidden": crate::agent::history::hidden_count(book, &threads),
    })
}

/// Изменить список и сохранить. Отказ на любом шаге — с причиной наружу.
fn edit_chat_book(
    app: &AppHandle,
    edit: impl FnOnce(&mut crate::agent::ChatBook) -> Result<(), String>,
) -> Value {
    let mut book = crate::agent::chat_book(app);
    match edit(&mut book).and_then(|()| crate::agent::save_chat_book(app, &book)) {
        Ok(()) => chat_book_json(&book),
        Err(e) => err(e),
    }
}

#[tauri::command]
pub fn agent_chats_list(app: AppHandle) -> Value {
    chat_book_json(&crate::agent::chat_book(&app))
}

#[tauri::command]
pub fn agent_chat_switch(app: AppHandle, chat_id: String) -> Value {
    edit_chat_book(&app, |b| b.switch(&chat_id))
}

/// Новый чат под новый проект. Имя необязательно — будет порядковый номер.
#[tauri::command]
pub fn agent_chat_create(app: AppHandle, name: Option<String>) -> Value {
    edit_chat_book(&app, |b| b.create(name.as_deref()).map(|_| ()))
}

#[tauri::command]
pub fn agent_chat_rename(app: AppHandle, chat_id: String, name: String) -> Value {
    edit_chat_book(&app, |b| b.rename(&chat_id, &name).map(|_| ()))
}

/// Убрать чат из списка. Сам разговор остаётся на диске И в истории: список
/// сшивается с транскриптами, поэтому удаление теряет имя и место, а не беседу.
#[tauri::command]
pub fn agent_chat_delete(app: AppHandle, chat_id: String) -> Value {
    edit_chat_book(&app, |b| b.delete(&chat_id))
}

/// Переставить чат на позицию `to_index` (0 — первый). Порядок задаёт человек и
/// меняет только перетаскиванием: активность строку не двигает, иначе теряется
/// смысл постоянного места и постоянного ⌘-сочетания под ним.
#[tauri::command]
pub fn agent_chat_reorder(app: AppHandle, chat_id: String, to_index: usize) -> Value {
    edit_chat_book(&app, |b| b.reorder(&chat_id, to_index))
}

/// Открыть разговор, найденный на диске: привязать его к новому чату и сделать
/// текущим. Отказ, если такого транскрипта нет, — молча завести пустой чат
/// значит повторить ровно тот тихий отказ, из-за которого разговор и терялся.
#[tauri::command]
pub fn agent_chat_open(app: AppHandle, session_id: String) -> Value {
    let sid = session_id.trim().to_string();
    let Some(dir) = agent_transcript_dir() else {
        return err("не нашёл каталог транскриптов агента — открывать нечего");
    };
    // id уходит в имя файла: пускаем только то, из чего пути не собрать.
    match crate::agent::history::transcript_path(&dir, &sid) {
        Some(p) if p.is_file() => edit_chat_book(&app, |b| b.adopt(&sid)),
        _ => err(format!("разговора {sid} нет на диске — открыть его не получится")),
    }
}

/* ----- недописанные реплики: у каждого чата свой черновик ----- */

/// Все черновики разом. Окно рисует по ним пометки в списке и подставляет текст
/// в поле — обе задачи нужны сразу, а данных тут на десяток килобайт.
#[tauri::command]
pub fn agent_drafts_get() -> Value {
    crate::agent::drafts::to_json(&crate::agent::drafts::all())
}

/// Сохранить черновик чата — или стереть его пустым текстом.
///
/// Хранилище своё (`agent/drafts.rs`), не settings.json: сюда пишут через
/// полсекунды после каждой клавиши, а настройки переписываются целиком.
#[tauri::command]
pub fn agent_draft_set(chat_id: String, text: String, caret: Option<usize>) -> Value {
    match crate::agent::drafts::set(&chat_id, &text, caret.unwrap_or(0)) {
        Ok(()) => json!({ "ok": true }),
        Err(e) => err(e),
    }
}

/// Убрать разговор из списка, оставив файл на диске: обратимое «с глаз долой».
/// Прятать можно только НЕпривязанный разговор — за привязанным стоит чат, и
/// убирается он через `agent_chat_delete`.
#[tauri::command]
pub fn agent_history_hide(app: AppHandle, session_id: String) -> Value {
    edit_chat_book(&app, |b| b.hide(&session_id))
}

/// Вернуть в список все скрытые разговоры — та самая обратимость, ради которой
/// скрытие и отделено от забвения.
#[tauri::command]
pub fn agent_history_unhide_all(app: AppHandle) -> Value {
    edit_chat_book(&app, |b| {
        b.unhide_all();
        Ok(())
    })
}

/// Забыть разговор насовсем: удалить транскрипт с диска.
///
/// Единственное необратимое действие приложения. Подтверждение спрашивает окно,
/// ядро его не дублирует — но и не смягчает отказы: привязанный к чату разговор
/// и разговор под идущим ходом не удаляются, а id проверяется как путь (он им и
/// становится). В лог — строкой: у необратимого обязан оставаться след.
#[tauri::command]
pub fn agent_history_forget(app: AppHandle, session_id: String) -> Value {
    let sid = session_id.trim().to_string();
    let Some(dir) = agent_transcript_dir() else {
        return err("не нашёл каталог транскриптов агента — удалять нечего");
    };
    let book = crate::agent::chat_book(&app);
    let busy = crate::agent::turn_in_flight(&sid);
    let path = match crate::agent::history::forget_decision(&dir, &book, &sid, busy) {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let turns = match crate::agent::history::forget_file(&path) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    crate::log::line(&format!(
        "[agent] разговор {sid} забыт насовсем: транскрипт удалён, записей было {turns}"
    ));
    // Пометка «скрыт» пережила бы файл и висела в настройках мусором.
    edit_chat_book(&app, |b| {
        b.unhide(&sid);
        Ok(())
    })
}

/// Прошлая переписка главного агента — чтобы окно рисовало ленту, а не пустоту.
///
/// Контекст агент помнит и без этого (`--resume`), но человеку нужно ВИДЕТЬ, о
/// чём шла речь: пустое окно с пометкой «продолжение» выглядит как потеря.
///
/// Транскрипт лежит там же, где у обычных сессий, только рабочая папка агента —
/// временная (`agent/mod.rs`, `current_dir(temp_dir())`), поэтому и каталог
/// проекта считаем от неё.
///
/// Отсутствие файла — не ошибка: разговора могло не быть, или транскрипт
/// подчищен системой. Отвечаем пустой лентой и говорим об этом честно, чтобы
/// окно не молчало о причине.
#[tauri::command]
pub fn agent_chat_history(app: AppHandle, chat_id: Option<String>) -> Value {
    let book = crate::agent::chat_book(&app);
    // Явный чат — чтобы окно рисовало ленту сразу после переключения, не гадая,
    // доехало ли переключение до настроек.
    let sid = match chat_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => match book.chats.iter().find(|c| c.id == id) {
            Some(c) => c.session_id.clone(),
            None => return err(format!("чата «{id}» нет в списке — обнови список")),
        },
        None => book.current().session_id.clone(),
    };
    let Some(sid) = sid else {
        return json!({ "ok": true, "items": [], "reason": "нет сохранённого разговора" });
    };
    let path = agent_transcript_dir().and_then(|d| crate::agent::history::transcript_path(&d, &sid));
    let Some(path) = path.filter(|p| p.exists()) else {
        return json!({
            "ok": true, "items": [], "sessionId": sid,
            "reason": "транскрипт не найден — история недоступна, контекст у агента остался",
        });
    };
    let be = crate::backend::backend(crate::backend::Agent::Claude);
    let entries = be.read_entries(&path, 512 * 1024);
    let items: Vec<Value> = entries
        .iter()
        .flat_map(|e| be.to_chat_items(e))
        .map(|i| json!({ "role": i.role, "kind": i.kind, "text": i.text, "ts": i.ts }))
        .collect();
    // хвост: длинную переписку целиком в окно не тащим
    let start = items.len().saturating_sub(120);
    json!({ "ok": true, "sessionId": sid, "items": &items[start..], "total": items.len() })
}

/* ----- авто-цепочка: режим, шапка, стоп ----- */

/// Чат, к которому относится команда цепочки. Тот же резолв, что у отправки:
/// окно могло не передать id (после перезапуска) — тогда открытый чат.
fn chain_chat(app: &AppHandle, chat_id: Option<String>) -> Result<String, String> {
    let book = crate::agent::chat_book(app);
    crate::agent::chat_for_send(&book, chat_id.as_deref(), None).map(|c| c.id.clone())
}

fn chain_ok(app: &AppHandle, chat_id: &str) -> Value {
    json!({ "ok": true, "state": crate::agent::chain::state(app, chat_id) })
}

/// Ответ команде + рассылка среза остальным окнам: чат бывает открыт не в одном.
fn chain_changed(app: &AppHandle, chat_id: &str) -> Value {
    crate::agent::chain::push_state(app, chat_id);
    chain_ok(app, chat_id)
}

/// Состояние цепочки для шапки чата: номер захода, что в работе, режим.
#[tauri::command]
pub fn agent_chain_state(app: AppHandle, chat_id: Option<String>) -> Value {
    match chain_chat(&app, chat_id) {
        Ok(id) => chain_ok(&app, &id),
        Err(e) => err(e),
    }
}

/// Включить/выключить «продолжать самому». Режим ложится в настройки чата —
/// он обязан пережить перезапуск, иначе Джарвис молча перестанет продолжать.
#[tauri::command]
pub fn agent_chain_mode(app: AppHandle, chat_id: Option<String>, auto: bool) -> Value {
    use crate::agent::chain::Mode;
    let id = match chain_chat(&app, chat_id) {
        Ok(id) => id,
        Err(e) => return err(e),
    };
    let mode = if auto { Mode::Auto } else { Mode::Ask };
    let mut book = crate::agent::chat_book(&app);
    if let Err(e) = book
        .set_mode(&id, mode)
        .and_then(|()| crate::agent::save_chat_book(&app, &book))
    {
        return err(e);
    }
    crate::agent::chain::chains().set_mode(&id, mode);
    chain_changed(&app, &id)
}

/// Стоп рвёт ЦЕПОЧКУ, а не текущий ход: завершения сессии больше никого не
/// разбудят, пока человек не включит режим снова. Режим тоже гасим — иначе
/// первый же следующий заход агента тихо перезапустил бы цепочку.
/// Вернуть цепочку в работу после паузы, которую поставила задача от человека.
///
/// Отдельная команда, а не «включить режим заново»: режим человек не менял, и
/// трогать его тут значило бы чинить не то. Пауза — это состояние живой цепочки,
/// и выходит она из него ровно одним решением.
#[tauri::command]
pub fn agent_chain_resume(app: AppHandle, chat_id: Option<String>) -> Value {
    let id = match chain_chat(&app, chat_id) {
        Ok(id) => id,
        Err(e) => return err(e),
    };
    if !crate::agent::chain::chains().resume(&id) {
        // Честный отказ: цепочки уже нет или она не на паузе. Молчаливое «ок»
        // здесь означало бы кнопку, после которой ничего не происходит.
        return err("продолжать нечего: цепочка не на паузе");
    }
    chain_changed(&app, &id)
}

#[tauri::command]
pub fn agent_chain_stop(app: AppHandle, chat_id: Option<String>) -> Value {
    use crate::agent::chain::Mode;
    let id = match chain_chat(&app, chat_id) {
        Ok(id) => id,
        Err(e) => return err(e),
    };
    crate::agent::chain::chains().stop(&id);
    let mut book = crate::agent::chat_book(&app);
    if let Err(e) = book
        .set_mode(&id, Mode::Ask)
        .and_then(|()| crate::agent::save_chat_book(&app, &book))
    {
        return err(e);
    }
    chain_changed(&app, &id)
}

/// Esc: остановить работу Джарвиса в ЭТОМ чате.
///
/// Останавливаем разом три вещи, иначе остановка получается на вид: сам ход
/// (сигнал доходит до процесса CLI, и тот умирает вместе с детьми), цепочку
/// авто-продолжения и режим «продолжать самому» — без последних двух
/// остановленный ход через минуту сменился бы следующим, и карусель нечем было
/// бы прервать. Соседние чаты не задеваются: реестр ходов разведён по `chatId`.
///
/// Дочерние сессии, поднятые через `sessions.spawn`, НЕ закрываем — там идёт
/// работа, за которую заплачено; вместо этого называем их словами.
///
/// `stopped: false` — хода не было. Это не ошибка: Esc нажали вхолостую, и окну
/// по этому полю понятно, что показывать нечего.
#[tauri::command]
pub fn agent_stop(app: AppHandle, chat_id: Option<String>) -> Value {
    use crate::agent::stop;
    let id = match chain_chat(&app, chat_id) {
        Ok(id) => id,
        Err(e) => return err(e),
    };
    let outcome = stop::request(&id);
    // Цепочку рвём в любом случае: она крутится и без идущего хода. Логику не
    // дублируем — зовём ту же команду, что и кнопка «стоп цепочки».
    let chain = agent_chain_stop(app.clone(), Some(id.clone()));
    if chain.get("ok").and_then(Value::as_bool) != Some(true) {
        crate::log::line(&format!("[agent] стоп {id}: цепочка не оборвалась — {chain}"));
    }

    let d = Daemon::get(&app);
    let chats: Vec<String> = crate::agent::chat_book(&app)
        .chats
        .iter()
        .map(|c| c.id.clone())
        .collect();
    let children = stop::children_of(&d.spawns.snapshot(), &id, &chats, |sid| {
        d.session(sid).is_some()
    });
    let stopped = outcome == stop::Outcome::Stopped;
    if stopped {
        // Пометка в ленту уходит тем же каналом, что и весь ход, — с меткой
        // чата: без неё она легла бы в соседний разговор.
        crate::agent::emit_event(
            &app,
            &id,
            &crate::agent::AgentEvent::Stopped { by: "user".into(), children: children.clone() },
        );
    }
    json!({
        "ok": true,
        "stopped": stopped,
        "chatId": id,
        "children": children,
        "note": stop::note(outcome, &children),
    })
}

/// Привязать чат к сессии вручную («следи за этой»). Обычно привязка возникает
/// сама — когда Джарвис отправляет промпт в сессию из этого чата.
#[tauri::command]
pub fn agent_chain_watch(app: AppHandle, chat_id: Option<String>, session_id: String) -> Value {
    let id = match chain_chat(&app, chat_id) {
        Ok(id) => id,
        Err(e) => return err(e),
    };
    let sid = session_id.trim();
    if sid.is_empty() {
        return err("не сказано, за какой сессией следить");
    }
    if Daemon::get(&app).session(sid).is_none() {
        return err(format!("сессии {sid} нет в списке — следить не за чем"));
    }
    let mode = crate::agent::chain::mode_of(&app, &id);
    crate::agent::chain::chains().watch(&id, sid, mode);
    chain_changed(&app, &id)
}

/// Отправить предложенный заход (кнопка в ручном режиме). `text` — если человек
/// поправил формулировку; пусто — уходит предложенное.
#[tauri::command]
pub async fn agent_chain_send(
    app: AppHandle,
    chat_id: Option<String>,
    text: Option<String>,
) -> Value {
    let id = match chain_chat(&app, chat_id) {
        Ok(id) => id,
        Err(e) => return err(e),
    };
    let chains = crate::agent::chain::chains();
    let st = crate::agent::chain::state(&app, &id);
    let Some(sid) = st.session_id.clone() else {
        return err("цепочка ни за какой сессией не следит — отправлять некуда");
    };
    let prompt = text
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| chains.proposal(&id))
        .unwrap_or_default();
    if prompt.is_empty() {
        return err("нечего отправлять — заход ещё не предложен");
    }
    let d = Daemon::get(&app);
    let step = chains.next_step(&id);
    // Кнопка «отправить заход» будит ДЖАРВИСА, а не пишет в сессию напрямую.
    // Диспетчер один: у чата есть задача человека, а у цепочки — только событие.
    //
    // Отказ уже ушёл событием, но и ответ команды обязан быть честным: окно,
    // получившее ok на неудавшуюся отправку, нарисовало бы «заход пошёл».
    match crate::agent::chain::wake(&d, &id, &sid, &prompt, step).await {
        Ok(()) => chain_ok(&app, &id),
        Err(e) => err(e),
    }
}

/// «Начать заново»: забыть нить ОТКРЫТОГО чата, оставив его имя и место в
/// списке. Прошлый разговор остаётся на диске — теряется только ниточка к нему,
/// и вернуть её можно, вписав id обратно в настройки.
#[tauri::command]
pub fn agent_chat_reset(app: AppHandle) -> Value {
    let id = crate::agent::chat_book(&app).current().id.clone();
    edit_chat_book(&app, |b| b.set_session(&id, None))
}

/* ================= STT — панель настроек (инкремент 9, фаза 9) ================= */

/// Состояние STT для настроек: активный движок, список движков, доступность, хоткей.
/// Не дёргает `available()` напрямую — он блокирует (HTTP). Возвращает мгновенный срез.
#[tauri::command]
pub fn stt_get(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let cfg = crate::stt::config::SttConfig::from_settings(&d.settings.load());
    let engine_name = d.stt.engine_name();
    let st = crate::install::status();
    let whisper_model = st.whisper_model;
    // Whisper готов, ТОЛЬКО если И модель на диске, И вкомпилирована нативная фича.
    // Иначе движок — стаб: переключение давало «whisper-native feature не включён».
    let whisper_native = st.whisper_native_built;
    let whisper_ready = whisper_model && whisper_native;
    // Qwen3 «готов» = жив процесс сайдкара (мгновенно). РАНЬШЕ здесь был HTTP /health
    // (`d.stt.available()`, до 3с connect-timeout) — он морозил панель настроек на
    // время холодной загрузки модели (особенно сразу после смены движка). Реальную
    // готовность модели подтверждает сам transcribe (wait_ready), так что для UI
    // достаточно факта живого процесса — без блокирующего сетевого вызова.
    let qwen3_sidecar = d.stt.sidecar_pid().is_some();
    // Установлен ли сайдкар на диске (venv + stt-server.py) — отдельно от health:
    // панель предлагает «Установить», если файлов нет, даже когда демон не отвечает.
    let qwen3_installed = st.qwen3_sidecar;
    json!({
        "engine": engine_name,
        "engines": ["whisper-turbo", "qwen3-0.6b", "qwen3-1.7b"],
        "whisperReady": whisper_ready,
        "whisperModel": whisper_model,
        "whisperNativeBuilt": whisper_native,
        "qwen3Ready": qwen3_sidecar,
        "qwen3Installed": qwen3_installed,
        "available": qwen3_sidecar || (cfg.engine == "whisper-turbo" && whisper_ready),
        "noiseGate": cfg.noise_gate,
        "hotkey": if cfg.hotkey.is_empty() { "F8".to_string() } else { cfg.hotkey },
    })
}

/// Единый инвентарь всех моделей (STT/голос/wake/runtime) для раздела «Модели».
/// Только filesystem-срез — без health/HTTP-проверок (мгновенно, без блокировок).
#[tauri::command]
pub fn models_get() -> Value {
    json!({ "models": crate::install::model_inventory() })
}

/// История диктовки/реплик («что я говорил») — новые первыми. Для UI + копирования.
#[tauri::command]
pub fn transcripts_get(app: AppHandle) -> Value {
    json!({ "items": Daemon::get(&app).transcripts.list() })
}

/// Очистить историю реплик.
#[tauri::command]
pub fn transcripts_clear(app: AppHandle) -> Value {
    Daemon::get(&app).transcripts.clear();
    json!({ "ok": true })
}

/// Удалить одну реплику по id (для страницы истории).
#[tauri::command]
pub fn transcript_delete(app: AppHandle, id: u64) -> Value {
    let ok = Daemon::get(&app).transcripts.remove(id);
    json!({ "ok": ok })
}

/// ПЕРЕГЕНЕРИРОВАТЬ распознавание реплики из сохранённого аудио (если анализ дал
/// ошибку/мусор). Грузит сжатое аудио по id, прогоняет текущим STT-движком, заменяет
/// текст реплики. Тяжёлое — в blocking-пуле, не морозим IPC. { ok, text } | { ok:false }.
#[tauri::command]
pub async fn transcript_retranscribe(app: AppHandle, id: u64) -> Value {
    let d = Daemon::get(&app);
    let stt = d.stt.clone();
    let opts = stt.options();
    let res = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let pcm = crate::stt::audio_store::load(id)?;
        let r = stt
            .transcribe(&pcm, &opts)
            .map_err(|e| format!("распознавание: {e}"))?;
        Ok(r.text.trim().to_string())
    })
    .await;
    match res {
        Ok(Ok(text)) if !text.is_empty() => {
            d.transcripts.update_text(id, &text);
            json!({ "ok": true, "text": text })
        }
        Ok(Ok(_)) => err("распознавание дало пустой результат"),
        Ok(Err(e)) => err(e),
        Err(e) => err(format!("задача упала: {e}")),
    }
}

/// Умные промпты: настройки (флаг «умный режим») для UI.
#[tauri::command]
pub fn prompts_get_settings(app: AppHandle) -> Value {
    Daemon::get(&app).prompts.settings_json()
}

/// Включить/выключить умный режим (авто-преобразование надиктовки).
#[tauri::command]
pub fn prompts_set_smart(app: AppHandle, on: bool) -> Value {
    Daemon::get(&app).prompts.set_smart(on);
    json!({ "ok": true })
}

/// Библиотека преобразований (встроенные) для UI.
#[tauri::command]
pub fn prompts_get() -> Value {
    crate::stt::prompts::builtin_prompts_json()
}

/// Преобразовать надиктованный текст через LLM (Haiku). `style`: "prompt" | "clean".
/// Возвращает { ok, result } или { ok:false, error }. Блокирующее — async-команда.
#[tauri::command]
pub async fn transcript_enhance(text: String, style: String) -> Value {
    let t = text.trim();
    if t.is_empty() {
        return err("Пустой текст — надиктуй или напиши что-нибудь");
    }
    let prompt = crate::stt::enhance::enhance_prompt(&style, t);
    match crate::claude_bin::run_haiku(&prompt, std::time::Duration::from_secs(45)).await {
        Some(s) => json!({ "ok": true, "result": s.trim() }),
        None => err("Быстрая модель не ответила — таймаут или claude недоступен; попробуй ещё раз"),
    }
}

/// Сменить движок STT + сохранить в settings.json. Требует перезапуска демона.
#[tauri::command]
pub fn stt_set_engine(app: AppHandle, engine: String) -> Value {
    let allowed = ["whisper-turbo", "qwen3-0.6b", "qwen3-1.7b"];
    if !allowed.contains(&engine.as_str()) {
        return err(format!("Неизвестный STT-движок: {engine}"));
    }
    // Гейт: не переключаемся на движок без локальных весов/окружения — иначе
    // qwen-сайдкар уйдёт в бесконечную загрузку с HF (:8732 висит), а whisper
    // вернёт «модель не установлена». Сначала пользователь скачивает модель.
    let st = crate::install::status();
    let ready = crate::install::stt_engine_ready(
        &engine,
        st.whisper_model,
        st.whisper_native_built,
        crate::install::qwen_weights_present(&engine),
        st.qwen3_sidecar,
    );
    if !ready {
        // Честная, конкретная ошибка под каждый режим отказа (правда по модели).
        let msg = if engine == "whisper-turbo" && st.whisper_model && !st.whisper_native_built {
            "whisper-turbo: модель скачана, но нужна нативная сборка \
             (--features whisper-native) — пересоберите приложение"
                .to_string()
        } else {
            format!("{engine}: модель не скачана — сначала скачайте её в разделе «Модели»")
        };
        return err(msg);
    }
    let d = Daemon::get(&app);
    let mut patch = serde_json::Map::new();
    patch.insert("engine".into(), Value::String(engine));
    d.settings.set_stt(patch);
    // Горячая смена без перезапуска демона: пересобрать движок/сайдкар на месте.
    // Диктовка и wake-action держат тот же Arc<SttService> — мутация им видна.
    let cfg = crate::stt::config::SttConfig::from_settings(&d.settings.load());
    d.stt.set_engine(cfg);
    json!({ "ok": true, "restart": false })
}

/// Переназначить хоткей диктовки (push-to-talk). Валидирует аксельератор,
/// снимает старый глобальный шорткат, пишет в `settings.stt.hotkey` и регистрирует
/// новый. При провале регистрации (сочетание занято) — откат на прежний.
#[tauri::command]
pub fn stt_set_hotkey(app: AppHandle, hotkey: String) -> Value {
    let hotkey = hotkey.trim().to_string();
    if hotkey.is_empty() {
        return err("Пустое сочетание");
    }
    // Должно парситься как глобальный шорткат tauri (например "F8" или "Command+Shift+D").
    if hotkey.parse::<Shortcut>().is_err() {
        return err(format!("Не разобрал сочетание: {hotkey}"));
    }
    let d = Daemon::get(&app);
    let old = dictation_accelerator(&d);
    if hotkey == old {
        return json!({ "ok": true, "hotkey": hotkey });
    }
    let gs = d.app.global_shortcut();
    let _ = gs.unregister(old.as_str());
    if gs.register(hotkey.as_str()).is_err() {
        let _ = gs.register(old.as_str()); // откат на прежний
        return err(format!("Сочетание {hotkey} занято системой"));
    }
    let mut patch = serde_json::Map::new();
    patch.insert("hotkey".into(), Value::String(hotkey.clone()));
    d.settings.set_stt(patch);
    json!({ "ok": true, "hotkey": hotkey })
}

/// Тумблер шумодава (VAD-гейт диктовки): on=true — пропускать не-речь.
#[tauri::command]
pub fn stt_set_noise_gate(app: AppHandle, on: bool) {
    let mut patch = serde_json::Map::new();
    patch.insert("noiseGate".into(), Value::Bool(on));
    Daemon::get(&app).settings.set_stt(patch);
}

/// Открыть панель и переключить на вкладку «История голоса» (клик по карточке
/// «Услышал»). Зеркалит onboarding_open_settings: show_panel + событие в main.
#[tauri::command]
pub fn voice_history_open(app: AppHandle) {
    use tauri::Emitter;
    crate::windows::show_panel(&Daemon::get(&app));
    let _ = app.emit_to("main", "goto-voicehist", ());
}

/// Список устройств ввода (микрофоны) + текущее выбранное — для селектора в
/// настройках. `current` = null → системное устройство по умолчанию.
#[tauri::command]
pub fn stt_input_devices(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let cfg = crate::stt::config::SttConfig::from_settings(&d.settings.load());
    json!({
        "devices": crate::stt::hub::input_device_names(),
        "current": cfg.audio_device,
    })
}

/// Выбрать устройство ввода. `name` пустой/null → системное по умолчанию.
/// Пишем в `settings.stt.audioDevice` и ГОРЯЧО применяем к AudioHub. Применение —
/// в блокирующем потоке: рестарт cpal-захвата (join старого потока + открытие нового
/// устройства CoreAudio) занимает сотни мс, и синхронно он морозил UI при выборе.
#[tauri::command]
pub fn stt_set_input_device(app: AppHandle, name: Option<String>) -> Value {
    let d = Daemon::get(&app);
    let name = name.filter(|s| !s.trim().is_empty());
    let mut patch = serde_json::Map::new();
    patch.insert(
        "audioDevice".into(),
        name.clone().map(Value::String).unwrap_or(Value::Null),
    );
    d.settings.set_stt(patch);
    // Команда возвращается МГНОВЕННО: тяжёлый рестарт захвата (cpal teardown + open
    // нового CoreAudio-устройства, сотни мс) уходит в blocking-пул fire-and-forget.
    // Раньше синхронный вызов морозил UI на время переключения.
    let audio = d.audio.clone();
    tauri::async_runtime::spawn_blocking(move || audio.set_device(name));
    json!({ "ok": true })
}

/* ============== Раздел «Под капотом»: служебный LLM (Claude/Codex) ============== */

/// Текущая конфигурация служебного LLM + доступность бэкендов — для рендера
/// раздела «Под капотом» (бэкенд, модель Codex, effort, кнопка установки SDK).
#[tauri::command]
pub fn service_get(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let cfg = crate::claude_bin::ServiceConfig::from_settings(&d.settings.load());
    let st = crate::install::status();
    let backend = match cfg.backend {
        crate::claude_bin::ServiceBackend::Claude => "claude",
        crate::claude_bin::ServiceBackend::Codex => "codex",
        crate::claude_bin::ServiceBackend::Auto => "auto",
    };
    json!({
        "backend": backend,
        "codexModel": cfg.codex_model,
        "codexEffort": cfg.codex_effort,
        // Реальные модели из ~/.codex/models_cache.json (включая spark/mini).
        "codexModels": codex_models_list(),
        // minimal убран: часть моделей (spark) его не поддерживают (400).
        "efforts": ["low", "medium", "high"],
        "codexSidecar": st.codex_sdk_sidecar, // SDK-сайдкар установлен
        "claudeBin": crate::claude_bin::resolve_claude_bin().is_some(),
        "codexBin": crate::backend::codex::resolve_codex_bin().is_some(),
        // egress-прокси служебных вызовов (пусто → наследуется из env процесса)
        "proxy": cfg.proxy,
    })
}

/// Реальные модели Codex из ~/.codex/models_cache.json для пикера: [{value,label}].
/// Первый элемент — «По умолчанию» (пустой slug). review-only модели отфильтрованы.
/// Ошибка/нет файла → только «По умолчанию» + пара известных slug'ов как фолбэк.
fn codex_models_list() -> Vec<Value> {
    let mut out = vec![json!({ "value": "", "label": "По умолчанию" })];
    let path = crate::util::home_dir().join(".codex/models_cache.json");
    if let Ok(txt) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            if let Some(arr) = v.get("models").and_then(Value::as_array) {
                for m in arr {
                    let slug = m.get("slug").and_then(Value::as_str).unwrap_or("");
                    if slug.is_empty() || slug.contains("review") {
                        continue;
                    }
                    let label = m
                        .get("display_name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(slug);
                    out.push(json!({ "value": slug, "label": format!("{label} ({slug})") }));
                }
            }
        }
    }
    if out.len() == 1 {
        for s in ["gpt-5.5", "gpt-5.4"] {
            out.push(json!({ "value": s, "label": s }));
        }
    }
    out
}

/// Применить блок `service` из настроек к процесс-глобальному конфигу служебного
/// LLM (чтобы свободные run_service_llm сразу увидели смену без перезапуска).
fn apply_service_config(d: &std::sync::Arc<Daemon>) {
    crate::claude_bin::set_service_config(crate::claude_bin::ServiceConfig::from_settings(
        &d.settings.load(),
    ));
}

#[tauri::command]
pub fn service_set_backend(app: AppHandle, backend: String) -> Value {
    if !["auto", "claude", "codex"].contains(&backend.as_str()) {
        return err(format!("неизвестный бэкенд: {backend}"));
    }
    let d = Daemon::get(&app);
    let mut p = serde_json::Map::new();
    p.insert("backend".into(), Value::String(backend));
    let written = d.settings.try_set_block("service", p);
    apply_service_config(&d);
    saved(written)
}

#[tauri::command]
pub fn service_set_model(app: AppHandle, model: String) -> Value {
    let d = Daemon::get(&app);
    let mut p = serde_json::Map::new();
    p.insert("codexModel".into(), Value::String(model));
    let written = d.settings.try_set_block("service", p);
    apply_service_config(&d);
    saved(written)
}

#[tauri::command]
pub fn service_set_effort(app: AppHandle, effort: String) -> Value {
    if !["minimal", "low", "medium", "high", "xhigh"].contains(&effort.as_str()) {
        return err(format!("неизвестный effort: {effort}"));
    }
    let d = Daemon::get(&app);
    let mut p = serde_json::Map::new();
    p.insert("codexEffort".into(), Value::String(effort));
    let written = d.settings.try_set_block("service", p);
    apply_service_config(&d);
    saved(written)
}

/// Задать egress-прокси служебных вызовов (Codex по HTTPS требует HTTPS_PROXY —
/// без него на прокси-сети запрос висит в таймаут). Пустая строка → стереть
/// настройку, прокси снова наследуется из env. Тримминг + лёгкая валидация схемы.
#[tauri::command]
pub fn service_set_proxy(app: AppHandle, proxy: String) -> Value {
    let proxy = proxy.trim().to_string();
    if !proxy.is_empty()
        && !proxy.starts_with("http://")
        && !proxy.starts_with("https://")
        && !proxy.starts_with("socks5://")
    {
        return err("Прокси должен начинаться с http://, https:// или socks5://");
    }
    let d = Daemon::get(&app);
    let mut p = serde_json::Map::new();
    p.insert("proxy".into(), Value::String(proxy));
    let written = d.settings.try_set_block("service", p);
    apply_service_config(&d);
    saved(written)
}

/// Проверка служебного LLM: короткий запрос через ВЫБРАННЫЙ бэкенд (run_service_llm),
/// прямой ответ — какая модель отвечает. Для кнопки «Протестировать» в «Под капотом».
#[tauri::command]
pub async fn service_test() -> Value {
    let prompt = "Ответь ОДНОЙ строкой: какая ты модель — точное короткое название \
                  (например «Claude Haiku 4.5» или «GPT-5.3 Codex»). Только название модели, \
                  без преамбул, без пояснений, без слов вроде «сейчас скажу».";
    let started = std::time::Instant::now();
    match crate::claude_bin::run_service_llm(prompt, std::time::Duration::from_secs(25)).await {
        Some(s) => json!({
            "ok": true,
            "result": crate::util::one_line(s.trim()),
            "ms": started.elapsed().as_millis() as u64,
        }),
        None => err("Модель не ответила за 25 с — проверь ключ и прокси в «Под капотом»"),
    }
}

/* --- Аккаунт Claude: подключить подписку (OAuth-токен) или API-ключ --- */

/// Состояние подключения аккаунта Claude для раздела «Под капотом».
#[tauri::command]
pub fn claude_auth_get(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let cfg = crate::claude_bin::ServiceConfig::from_settings(&d.settings.load());
    let connected = !cfg.claude_auth_mode.is_empty() && !cfg.claude_secret.is_empty();
    // маска секрета: префикс…суффикс (ASCII — sk-ant-…/токены), без утечки
    let s = &cfg.claude_secret;
    let hint = if s.len() > 18 {
        format!("{}…{}", &s[..10], &s[s.len() - 4..])
    } else if connected {
        "••••".to_string()
    } else {
        String::new()
    };
    json!({
        "connected": connected,
        "mode": cfg.claude_auth_mode, // "key" | "subscription" | ""
        "hint": hint,
        "claudeBin": crate::claude_bin::resolve_claude_bin().is_some(),
    })
}

/// Подключить аккаунт Claude: валидируем крошечным `claude -p`, при успехе пишем
/// в settings.json (0600) и обновляем процесс-конфиг. mode ∈ key|subscription.
#[tauri::command]
pub async fn claude_auth_connect(app: AppHandle, mode: String, value: String) -> Value {
    let value = value.trim().to_string();
    if value.is_empty() {
        return err("пустой ключ/токен");
    }
    if mode != "key" && mode != "subscription" {
        return err(format!("неизвестный режим: {mode}"));
    }
    if crate::claude_bin::resolve_claude_bin().is_none() {
        return err("claude не найден в PATH — установи Claude Code");
    }
    let valid =
        crate::claude_bin::validate_claude_auth(&mode, &value, std::time::Duration::from_secs(40))
            .await;
    if !valid {
        return err("не сработало: проверь ключ/токен (или claude недоступен)");
    }
    let d = Daemon::get(&app);
    let mut p = serde_json::Map::new();
    p.insert("claudeAuthMode".into(), Value::String(mode));
    p.insert("claudeSecret".into(), Value::String(value));
    let written = d.settings.try_set_block("service", p);
    apply_service_config(&d);
    saved(written)
}

/// Отключить аккаунт Claude — снова используется собственный логин `claude` CLI.
#[tauri::command]
pub fn claude_auth_disconnect(app: AppHandle) -> Value {
    let d = Daemon::get(&app);
    let mut p = serde_json::Map::new();
    p.insert("claudeAuthMode".into(), Value::String(String::new()));
    p.insert("claudeSecret".into(), Value::String(String::new()));
    let written = d.settings.try_set_block("service", p);
    apply_service_config(&d);
    saved(written)
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
        let pcm = session.finish().map_err(|e| format!("захват: {e}"))?;
        let r = stt
            .transcribe(&pcm, &opts)
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
    let mut status = Daemon::get(&app).wake.status();
    // Без фичи `wakeword-ort` движок — стаб: UI должен видеть, что «Hey Jarvis»
    // в этой сборке не заработает даже со скачанными весами (ср. whisperNativeBuilt).
    if let Some(map) = status.as_object_mut() {
        map.insert(
            "ort_built".into(),
            json!(crate::install::status().wakeword_ort_built),
        );
    }
    status
}

/// Вкл/выкл always-on детектор. Поднимает/гасит consumer-поток и аудио-захват.
#[tauri::command]
pub fn wake_set_enabled(app: AppHandle, on: bool) -> Value {
    let d = Daemon::get(&app);
    // Гейт: без скачанных моделей openWakeWord детектор молча инертен (стаб) —
    // не даём включить, пока модель не установлена в разделе «Модели».
    let st = crate::install::status();
    if on && !st.wakeword_ort_built {
        return err("«Привет, Джарвис» в этой сборке не работает — нужна сборка с поддержкой wake-word");
    }
    if on && !st.wakeword_models {
        return err("Сначала скачай модели wake-word в разделе «Модели»");
    }
    let mut patch = serde_json::Map::new();
    patch.insert("enabled".into(), json!(on));
    let written = d.settings.try_set_block("wake", patch);
    d.wake.set_enabled(on);
    if let Err(e) = written {
        return err(format!("Детектор включён, но настройка не сохранилась: {e}"));
    }
    json!({ "ok": true, "status": d.wake.status() })
}

/// Установить порог срабатывания (0..1). Переконфигурирует детектор вживую.
#[tauri::command]
pub fn wake_set_threshold(app: AppHandle, threshold: f64) -> Value {
    let d = Daemon::get(&app);
    let mut patch = serde_json::Map::new();
    patch.insert("threshold".into(), json!(threshold.clamp(0.0, 1.0)));
    let written = d.settings.try_set_block("wake", patch);
    let root = d.settings.load();
    let wcfg = crate::wakeword::config::WakeConfig::from_settings(&root);
    let vcfg = crate::wakeword::config::VerifyConfig::from_settings(&root);
    d.wake.reconfigure(wcfg, vcfg);
    if let Err(e) = written {
        return err(format!("Порог применён, но не сохранился: {e}"));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- file_read: путь только из фактов сессии (спека 2026-07-18 §3.1/§4) ---

    // каталог на тест (cargo test параллелен — общий каталог дал бы гонку)
    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("jarvis-fileread-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // Claude-транскрипт из одного хода, где агент правил `path` (Edit)
    fn entries_touching(path: &str) -> Vec<Value> {
        vec![
            json!({"type":"user","uuid":"u1","timestamp":"2026-07-18T10:00:00Z",
                   "message":{"content":"поправь док"}}),
            json!({"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-07-18T10:00:05Z",
                   "message":{"content":[
                       {"type":"tool_use","name":"Edit","input":{"file_path": path}},
                       {"type":"text","text":"готово"}]}}),
        ]
    }

    fn claude_be() -> &'static dyn crate::backend::Backend {
        crate::backend::backend(crate::backend::Agent::Claude)
    }

    #[test]
    fn file_read_rejects_missing_session() {
        let res = file_read_dispatch(None, "docs/a.md");
        assert_eq!(res["ok"], json!(false));
        assert!(res["error"].as_str().unwrap().contains("Сессия"));
    }

    #[test]
    fn file_read_rejects_path_outside_facts() {
        // оба файла существуют, но в фактах только a.md — b.md не отдаём,
        // даже если путь пришёл «от чипа» (инъекция в транскрипт)
        let d = tmp_dir("outside");
        std::fs::write(d.join("a.md"), "# a").unwrap();
        std::fs::write(d.join("b.md"), "секрет").unwrap();
        let cwd = d.to_str().unwrap();
        let res = file_read_impl(Some(cwd), claude_be(), &entries_touching("a.md"), "b.md");
        assert_eq!(res["ok"], json!(false));
        assert!(res["error"].as_str().unwrap().contains("не из фактов"), "{res}");
    }

    #[test]
    fn file_read_returns_file_from_facts() {
        let d = tmp_dir("ok");
        std::fs::write(d.join("a.md"), "# Заголовок\nтело").unwrap();
        let cwd = d.to_str().unwrap();
        // в фактах путь абсолютный (как пишет Claude), запрос — относительный:
        // сверка канонизированных путей обязана их сматчить
        let abs = d.join("a.md");
        let res = file_read_impl(
            Some(cwd),
            claude_be(),
            &entries_touching(abs.to_str().unwrap()),
            "a.md",
        );
        assert_eq!(res["ok"], json!(true), "{res}");
        assert_eq!(res["name"], json!("a.md"));
        assert_eq!(res["content"], json!("# Заголовок\nтело"));
        assert_eq!(res["truncated"], json!(false));
    }

    #[test]
    fn file_read_truncates_large_file_head_tail() {
        let d = tmp_dir("trunc");
        let mut big = vec![b'H'; FILE_READ_HEAD];
        big.extend(std::iter::repeat(b'M').take(300 * 1024)); // середина — вырезается
        big.extend(std::iter::repeat(b'T').take(FILE_READ_TAIL));
        std::fs::write(d.join("big.log"), &big).unwrap();
        let cwd = d.to_str().unwrap();
        let res = file_read_impl(Some(cwd), claude_be(), &entries_touching("big.log"), "big.log");
        assert_eq!(res["ok"], json!(true), "{res}");
        assert_eq!(res["truncated"], json!(true));
        let content = res["content"].as_str().unwrap();
        assert!(content.starts_with('H') && content.ends_with('T'));
        assert!(content.contains("обрезано"));
        assert!(!content.contains('M'), "середина вырезана");
    }

    #[test]
    fn file_read_rejects_missing_file() {
        let d = tmp_dir("missing");
        let cwd = d.to_str().unwrap();
        let res = file_read_impl(Some(cwd), claude_be(), &entries_touching("нет.md"), "нет.md");
        assert_eq!(res["ok"], json!(false));
    }

    // --- file_diff: тот же гейт по фактам + режимы git (§3.2) ---

    fn git_q(dir: &std::path::Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn file_diff_rejects_path_outside_facts() {
        // b.md существует, но факты знают только a.md — дифф не отдаём
        let d = tmp_dir("diff-outside");
        std::fs::write(d.join("a.md"), "# a").unwrap();
        std::fs::write(d.join("b.md"), "секрет").unwrap();
        let res = file_diff_impl(
            Some(d.to_str().unwrap()),
            claude_be(),
            &entries_touching("a.md"),
            "b.md",
        );
        assert_eq!(res["ok"], json!(false));
        assert!(res["error"].as_str().unwrap().contains("не из фактов"), "{res}");
    }

    #[test]
    fn file_diff_worktree_mode_for_edited_doc() {
        // файл в фактах + незакоммиченная правка → mode worktree с ханками
        let d = tmp_dir("diff-wt").canonicalize().unwrap();
        git_q(&d, &["init", "-q"]);
        std::fs::write(d.join("a.md"), "один\nдва\nтри\n").unwrap();
        git_q(&d, &["add", "."]);
        git_q(&d, &["commit", "-q", "-m", "база"]);
        std::fs::write(d.join("a.md"), "один\nДВА\nтри\n").unwrap();
        let abs = d.join("a.md");
        let res = file_diff_impl(
            Some(d.to_str().unwrap()),
            claude_be(),
            &entries_touching(abs.to_str().unwrap()),
            "a.md",
        );
        assert_eq!(res["ok"], json!(true), "{res}");
        assert_eq!(res["mode"], json!("worktree"), "{res}");
        assert!(res["hunks"].as_array().unwrap().len() >= 1, "{res}");
    }

    // --- шаблон хоткеев выбора варианта (selectHotkeyTemplate) ---

    #[test]
    fn select_accel_substitutes_number() {
        assert_eq!(select_accel("Command+Alt+{n}", 3), "Command+Alt+3");
        assert_eq!(select_accel("Control+Shift+{n}", 9), "Control+Shift+9");
    }

    #[test]
    fn normalize_keeps_valid_template() {
        assert_eq!(
            normalize_select_template("Control+Shift+{n}"),
            "Control+Shift+{n}"
        );
    }

    #[test]
    fn normalize_falls_back_on_broken_template() {
        // без {n}, пусто, непарсибельный экземпляр → дефолт ⌘⌥{n}
        assert_eq!(normalize_select_template("Command+Alt+5"), SELECT_TEMPLATE_DEFAULT);
        assert_eq!(normalize_select_template(""), SELECT_TEMPLATE_DEFAULT);
        assert_eq!(normalize_select_template("Bogus+{n}"), SELECT_TEMPLATE_DEFAULT);
    }

    #[test]
    fn match_select_template_finds_number() {
        let sc: Shortcut = "Control+Shift+4".parse().unwrap();
        assert_eq!(match_select_template("Control+Shift+{n}", &sc), Some(4));
        // чужой шаблон это сочетание не матчит
        assert_eq!(match_select_template("Command+Alt+{n}", &sc), None);
    }

    #[test]
    fn match_select_template_rejects_non_digit_combo() {
        let sc: Shortcut = "Command+Alt+K".parse().unwrap();
        assert_eq!(match_select_template("Command+Alt+{n}", &sc), None);
    }

    // --- реестр действий HkAction ---

    #[test]
    fn hk_action_parse_roundtrip() {
        for a in HkAction::ALL {
            assert_eq!(HkAction::parse(a.id()), Some(a));
        }
        assert_eq!(HkAction::parse("bogus"), None);
    }

    #[test]
    fn accel_from_raw_empty_is_default() {
        assert_eq!(
            accel_from_raw("", HkAction::Quiet),
            Some("Command+Alt+J".to_string())
        );
        assert_eq!(accel_from_raw("", HkAction::Dictation), Some("F8".to_string()));
    }

    #[test]
    fn accel_from_raw_none_is_unassigned() {
        assert_eq!(accel_from_raw(HK_NONE, HkAction::Mute), None);
    }

    #[test]
    fn accel_from_raw_select_normalizes() {
        // битый шаблон мягко деградирует в дефолт, как normalize_select_template
        assert_eq!(
            accel_from_raw("Command+Alt+5", HkAction::Select),
            Some(SELECT_TEMPLATE_DEFAULT.to_string())
        );
    }

    // --- детект конфликтов ---

    fn b(a: HkAction, acc: &str) -> (HkAction, String) {
        (a, acc.to_string())
    }

    #[test]
    fn conflict_direct_hit() {
        let bindings = vec![b(HkAction::Mute, "Command+Alt+M")];
        assert_eq!(
            find_conflict(&bindings, HkAction::Quiet, "Command+Alt+M"),
            Some(HkAction::Mute)
        );
    }

    #[test]
    fn conflict_ignores_self_and_free() {
        let bindings = vec![
            b(HkAction::Quiet, "Command+Alt+J"),
            b(HkAction::Mute, "Command+Alt+M"),
        ];
        // то же действие — не конфликт (перезапись самого себя)
        assert_eq!(find_conflict(&bindings, HkAction::Quiet, "Command+Alt+J"), None);
        // свободное сочетание — не конфликт
        assert_eq!(find_conflict(&bindings, HkAction::Quiet, "Command+Alt+X"), None);
    }

    #[test]
    fn conflict_with_select_instance() {
        // ⌘⌥3 бьётся с экземпляром шаблона ⌘⌥{n}
        let bindings = vec![b(HkAction::Select, "Command+Alt+{n}")];
        assert_eq!(
            find_conflict(&bindings, HkAction::Dictation, "Command+Alt+3"),
            Some(HkAction::Select)
        );
    }

    #[test]
    fn conflict_new_select_template_vs_plain() {
        // новый шаблон ⌘⌃{n} бьётся с уже занятым ⌘⌃5
        let bindings = vec![b(HkAction::Repeat, "Command+Control+5")];
        assert_eq!(
            find_conflict(&bindings, HkAction::Select, "Command+Control+{n}"),
            Some(HkAction::Repeat)
        );
    }

    #[test]
    fn conflict_skips_broken_bindings() {
        let bindings = vec![b(HkAction::Mute, "Bogus+Nope")];
        assert_eq!(find_conflict(&bindings, HkAction::Quiet, "Command+Alt+M"), None);
    }

    // --- контракт question_answer (parse_question_choice) ---

    #[test]
    fn choice_new_contract_answers_and_texts() {
        let (answers, texts) = parse_question_choice(&json!({
            "answers": [[2], [], [1, 3]],
            "texts": [null, "свой ответ", null],
        }));
        assert_eq!(answers, vec![vec![2], vec![], vec![1, 3]]);
        assert_eq!(texts, vec![None, Some("свой ответ".to_string()), None]);
    }

    #[test]
    fn choice_back_compat_answers_without_texts() {
        let (answers, texts) = parse_question_choice(&json!({ "answers": [[1], [2]] }));
        assert_eq!(answers, vec![vec![1], vec![2]]);
        assert!(texts.is_empty());
    }

    #[test]
    fn choice_back_compat_legacy_indices() {
        // старый тост-контракт `{ indices }` — один вопрос, без кастома
        let (answers, texts) = parse_question_choice(&json!({ "indices": [1, 3] }));
        assert_eq!(answers, vec![vec![1, 3]]);
        assert!(texts.is_empty());
    }

    #[test]
    fn choice_texts_normalize_blank_to_none() {
        // пустые/пробельные строки — не кастом; края текста подрезаются
        let (_, texts) = parse_question_choice(&json!({
            "answers": [[1], [1], [1]],
            "texts": ["", "   ", "  да  "],
        }));
        assert_eq!(texts, vec![None, None, Some("да".to_string())]);
    }

    #[test]
    fn choice_garbage_is_empty() {
        let (answers, texts) = parse_question_choice(&json!({ "bogus": true }));
        assert!(answers.is_empty());
        assert!(texts.is_empty());
    }
}

/* ================= удалённые узлы ================= */

/// Список узлов с их живостью — вкладка «Удалённые».
#[tauri::command]
pub async fn remotes_list(app: AppHandle) -> Value {
    let _t = crate::log::Step::new("remotes_list");
    json!(Daemon::get(&app).remotes.list())
}

/// Добавить узел в настройки и поднять его. Список описывается целиком, поэтому
/// после записи перезапускаем весь слой — точечный старт оставил бы прежние
/// туннели жить от старого конфига.
#[tauri::command]
pub async fn remotes_add(app: AppHandle, cfg: Value) -> Value {
    let d = Daemon::get(&app);
    let name = cfg.get("name").and_then(Value::as_str).unwrap_or("").trim();
    let host = cfg.get("sshHost").and_then(Value::as_str).unwrap_or("").trim();
    let dir = cfg.get("jarvisDir").and_then(Value::as_str).unwrap_or("").trim();
    if name.is_empty() {
        return err("Нужно имя узла");
    }
    if host.is_empty() {
        return err("Нужен ssh-хост");
    }
    // Имя ходит и в ключ реестра, и в имя файла курсора — двоеточия, слэши и
    // пробелы там либо ломают разбор, либо схлопывают два разных узла в один
    // файл. Проще запретить на входе, чем чинить последствия.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return err("В имени узла — только латиница, цифры, дефис и подчёркивание");
    }
    // Имя — ключ реестра (`<узел>:<id>`): два узла под одним именем смешали бы
    // сессии разных машин.
    let mut list = remotes_array(&d);
    if list.iter().any(|v| {
        v.get("name").and_then(Value::as_str).map(str::trim) == Some(name)
    }) {
        return err(format!("Узел «{name}» уже есть"));
    }
    let mut entry = json!({ "name": name, "sshHost": host });
    if !dir.is_empty() {
        entry["jarvisDir"] = json!(dir);
    }
    list.push(entry);
    d.settings.set_top("remotes", Value::Array(list));
    d.start_remotes();
    ok()
}

/// Убрать узел: гасим туннель и забываем его сессии — иначе в списке остались
/// бы строки машины, за которой уже никто не следит.
#[tauri::command]
pub async fn remotes_remove(app: AppHandle, name: String) -> Value {
    let d = Daemon::get(&app);
    let name = name.trim();
    let list: Vec<Value> = remotes_array(&d)
        .into_iter()
        .filter(|v| v.get("name").and_then(Value::as_str).map(str::trim) != Some(name))
        .collect();
    d.settings.set_top("remotes", Value::Array(list));
    d.start_remotes();
    d.forget_remote_sessions(name);
    ok()
}

/// Проверка связи: поднят ли туннель и отвечает ли узел.
#[tauri::command]
pub async fn remotes_test(app: AppHandle, name: String) -> Value {
    let d = Daemon::get(&app);
    let Some(node) = d.remotes.node(name.trim()) else {
        return err("Узел не найден — сохрани его и попробуй снова");
    };
    // Туннеля нет — поднимаем сами, а не отсылаем человека ждать поллер.
    // «Проверить» должно ОТВЕЧАТЬ, почему не работает, иначе кнопка бесполезна
    // ровно тогда, когда нужна.
    // Поднимаем и когда порта нет, и когда ssh умер, а порт от него остался:
    // во втором случае в туннель просто некому отвечать, и «проверить» без
    // переподъёма честно врало бы «узел недоступен».
    if node.client().is_err() || !node.tunnel.is_up() {
        let n = node.clone();
        let state = tokio::task::spawn_blocking(move || n.tunnel.ensure_started())
            .await
            .unwrap_or(crate::remote::TunnelState::Failed);
        if state == crate::remote::TunnelState::Failed {
            let why = node.why();
            let host = &node.cfg.ssh_host;
            // Ровно та команда, которой это проверяется за пять секунд: без неё
            // человек остаётся один на один с «не работает».
            return err(if why.is_empty() {
                format!("ssh не поднял туннель. Проверь руками: ssh {host} true")
            } else {
                format!("туннель не поднялся: {why}\nПроверь руками: ssh {host} true")
            });
        }
        // ждём, пока форвард начнёт принимать, а не гадаем о таймингах
        let n = node.clone();
        let ready = tokio::task::spawn_blocking(move || {
            n.tunnel.wait_ready(std::time::Duration::from_secs(15))
        })
        .await
        .unwrap_or(false);
        if !ready {
            let why = node.why();
            let host = &node.cfg.ssh_host;
            return err(format!(
                "ssh не открыл туннель за 15 секунд{}\nПроверь руками: ssh {host} true",
                if why.is_empty() { String::new() } else { format!(": {why}") }
            ));
        }
    }
    let client = match node.client() {
        Ok(c) => c,
        Err(e) => return err(format!("{e}: {}", node.why())),
    };
    match client.hello().await {
        Ok(h) => {
            // «Проверить» — тоже рукопожатие: пусть строка узла сразу узнает
            // его версию, не дожидаясь круга поллера.
            node.saw_version(&h.version);
            json!({
                "ok": true, "host": h.host, "version": h.version,
                "buffered": h.buffered, "outdated": node.outdated(),
            })
        }
        // Узел не ответил при живом ssh — почти всегда это «сокета нет»:
        // узел не запущен на той стороне. Подсказываем, чем это проверить.
        Err(e) => err(format!(
            "{}\nУзел не ответил. На той машине: systemctl --user status jarvis-node",
            ellipsize(&one_line(&e), 160)
        )),
    }
}

/// Идёт ли установка узла прямо сейчас. Две параллельные писали бы в один и тот
/// же каталог на той машине и мешали бы друг другу заливать бинарь.
static REMOTE_INSTALL_BUSY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Разведка машины до установки: ОС, что там есть и как туда попадёт узел.
/// Долгая (ssh), поэтому в блокирующем потоке — иначе подвисает вся панель.
#[tauri::command]
pub async fn remotes_preflight(ssh_host: String, jarvis_dir: Option<String>) -> Value {
    let out = tokio::task::spawn_blocking(move || {
        crate::install::remote::preflight(&ssh_host, jarvis_dir.as_deref())
    })
    .await;
    match out {
        Ok(Ok(p)) => match serde_json::to_value(p) {
            Ok(Value::Object(mut m)) => {
                m.insert("ok".into(), Value::Bool(true));
                Value::Object(m)
            }
            _ => err("не смог разобрать ответ разведки"),
        },
        Ok(Err(e)) => err(e),
        Err(_) => err("разведка прервалась"),
    }
}

/// Поставить узел на машину с нуля: бинарь, шим, хуки, автозапуск, запись в
/// настройки. Возвращается сразу — ход установки едет событиями
/// `remote_install_progress`, конец — `remote_install_done`.
///
/// Не блокирующая команда, потому что это минуты: ssh-заходы, а иногда и сборка
/// на той стороне. Панель всё это время должна оставаться живой.
#[tauri::command]
pub fn remotes_install(app: AppHandle, cfg: Value) -> Value {
    use std::sync::atomic::Ordering;
    let d = Daemon::get(&app);
    let name = cfg.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let host = cfg.get("sshHost").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let dir = cfg
        .get("jarvisDir")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if name.is_empty() {
        return err("Нужно имя узла");
    }
    if host.is_empty() {
        return err("Нужен ssh-хост");
    }
    if REMOTE_INSTALL_BUSY.swap(true, Ordering::SeqCst) {
        return err("Уже ставлю другой узел — дождись конца");
    }

    std::thread::spawn(move || {
        // Паника внутри установки не должна оставить панель с вечным «ставлю»:
        // ловим её и отдаём как обычный отказ.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::install::remote::add(
                &|step| windows::emit_to_panel(&app, "remote_install_progress", &step),
                &name,
                &host,
                dir.as_deref(),
                // Порт для телефона поднимаем сразу: узнать, что его не хватает,
                // человек может только на чужом устройстве и уже без панели.
                // Кому не нужен — `jarvis-setup remote add --no-tcp`.
                Some(crate::install::remote::DEFAULT_TCP_PORT),
            )
        }));
        let res = match outcome {
            Ok(r) => r,
            Err(_) => Err("установщик аварийно остановился — повтори, это безопасно".into()),
        };
        // Замок снимаем сразу, как только установщик отработал: всё, что ниже,
        // к чужой машине уже не ходит, и падать там нечему — но если бы упало,
        // вечное «уже ставлю другой узел» пережило бы саму ошибку.
        REMOTE_INSTALL_BUSY.store(false, Ordering::SeqCst);
        if res.is_ok() {
            // Узел уже в settings.json (его записал установщик) — поднимаем
            // туннель и поллер, чтобы сессии поехали без перезапуска панели.
            d.start_remotes();
            d.push();
        }
        windows::emit_to_panel(
            &app,
            "remote_install_done",
            &match &res {
                Ok(()) => json!({ "ok": true, "name": name }),
                Err(e) => json!({ "ok": false, "name": name, "error": e }),
            },
        );
    });
    ok()
}

/// Публичный ssh-ключ этой машины — его человек вставляет в панель VPS, когда
/// доступа ещё нет. `create: true` — завести ed25519, если ключей нет вовсе.
///
/// Своего ключа Jarvis не заводит без спроса и чужие не трогает: доступ к чужим
/// машинам остаётся решением человека.
#[tauri::command]
pub async fn remotes_ssh_key(create: bool) -> Value {
    // Синхронная команда Tauri выполняется в ГЛАВНОМ потоке, а внутри —
    // порождение процесса. Один ssh-keygen, задумавшийся у промпта, вешал всё
    // окно намертво; в blocking-пуле он не мешает никому.
    let res = tauri::async_runtime::spawn_blocking(move || public_ssh_key(create)).await;
    match res {
        Ok(Ok((key, path, created))) => json!({
            "ok": true, "created": created, "path": path, "publicKey": key,
        }),
        Ok(Err(e)) => err(e),
        Err(_) => err("не удалось прочитать ssh-ключ"),
    }
}

/// Публичный ключ этой машины: `(ключ, путь, только что создан)`. Пустой ключ —
/// ключей нет, а заводить не просили.
fn public_ssh_key(create: bool) -> Result<(String, String, bool), String> {
    let dir = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => std::path::PathBuf::from(h).join(".ssh"),
        _ => return Err("не знаю домашний каталог".into()),
    };
    // Порядок — по предпочтительности: ed25519 короче и современнее, rsa
    // остаётся ради машин со старым sshd.
    for name in ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"] {
        let path = dir.join(name);
        if let Ok(key) = std::fs::read_to_string(&path) {
            let key = key.trim().to_string();
            if !key.is_empty() {
                return Ok((key, path.display().to_string(), false));
            }
        }
    }
    if !create {
        return Ok((String::new(), String::new(), false));
    }
    let key = dir.join("id_ed25519");
    // Приватный ключ на месте, а .pub нет — публичную часть ВЫВОДИМ из него.
    // Прежний код шёл сразу генерировать поверх, а ssh-keygen на это
    // спрашивает «Overwrite (y/n)?» — и, не дождавшись ответа, висел вечно.
    // Перезаписать чужой ключ он при этом мог бы и вовсе не спрашивая.
    if key.exists() {
        let out = std::process::Command::new("ssh-keygen")
            .args(["-y", "-f"])
            .arg(&key)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("не запустился ssh-keygen: {e}"))?;
        if !out.status.success() {
            return Err(
                "у ключа ~/.ssh/id_ed25519 нет публичной половины, а достать её не вышло —                  похоже, он под пассфразой. Добавь ключ в ssh-agent или укажи другой"
                    .into(),
            );
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let pub_path = key.with_extension("pub");
        let _ = std::fs::write(&pub_path, format!("{text}\n"));
        return Ok((text, pub_path.display().to_string(), false));
    }
    let out = std::process::Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-C", "jarvis", "-f"])
        .arg(&key)
        // Ни один вопрос ssh-keygen не должен уметь остановить приложение:
        // без stdin он упирается в конец ввода и честно завершается ошибкой.
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("не запустился ssh-keygen: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ssh-keygen: {}",
            ellipsize(&one_line(&String::from_utf8_lossy(&out.stderr)), 160)
        ));
    }
    let pub_path = key.with_extension("pub");
    let text = std::fs::read_to_string(&pub_path)
        .map_err(|e| format!("ключ создан, но не прочитался: {e}"))?;
    Ok((text.trim().to_string(), pub_path.display().to_string(), true))
}

/// Разовый вход по паролю: положить туда наш публичный ключ, чтобы дальше
/// ходить без пароля. Ключа нет — заводим (человек уже согласился, нажав
/// «войти по паролю»).
///
/// Пароль нужен ровно один раз и никуда не сохраняется. Иначе и нельзя:
/// туннель к узлу переподнимается сам после сна и смены сети, спросить пароль
/// в этот момент не у кого — транспорт обязан работать по ключу.
#[tauri::command]
pub async fn remotes_ssh_authorize(app: AppHandle, ssh_host: String, password: String) -> Value {
    let out = tokio::task::spawn_blocking(move || {
        let (key, _, created) = public_ssh_key(true)?;
        if key.is_empty() {
            return Err("не нашёл и не смог создать ssh-ключ".to_string());
        }
        crate::install::remote::authorize_key(
            &|step| windows::emit_to_panel(&app, "remote_install_progress", &step),
            &ssh_host,
            &password,
            &key,
        )?;
        Ok::<bool, String>(created)
    })
    .await;
    match out {
        Ok(Ok(created)) => json!({ "ok": true, "createdKey": created }),
        Ok(Err(e)) => err(e),
        Err(_) => err("вход по паролю прервался"),
    }
}

/// Ключ `remotes` настроек как массив (что угодно другое считаем пустым: список
/// правится и руками, и битое значение не повод терять команду).
fn remotes_array(d: &Arc<Daemon>) -> Vec<Value> {
    d.settings
        .load()
        .get("remotes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}


/// Песочница задачи: отдельный worktree рядом с проектом.
///
/// Так же, как у «Связки»: рабочая копия соседом (`../wt-<имя>`), ветка
/// `task/<имя>`. Соседом, а не в недрах `~/.jarvis`, — человек в неё
/// заглядывает, открывает редактором и коммитит руками.
///
/// Не репозиторий — не беда и не повод отказывать: задача поднимется прямо в
/// каталоге, просто без изоляции. Отказ здесь стоил бы дороже, чем польза.
async fn sandbox_for(host: &Host, cwd: &str) -> Result<String, String> {
    let cwd = cwd.trim_end_matches('/').to_string();
    if !crate::bundle::git::is_repo(host, &cwd).await {
        return Err(format!(
            "{cwd} — не репозиторий git: песочнице неоткуда взяться (сними галочку или заведи репозиторий)"
        ));
    }
    let base = crate::bundle::git::base_branch(host, &cwd).await?;
    let name = crate::util::basename(&cwd);
    let stamp = crate::util::now_ms() % 100_000;
    let slug = format!("{name}-{stamp}");
    let parent = crate::bundle::host::parent_of(&cwd);
    let dir = format!("{parent}/wt-{slug}");
    crate::bundle::git::add_worktree(host, &cwd, &dir, &format!("task/{slug}"), &base).await?;
    Ok(dir)
}

/* ================= изменения задачи ================= */

/// Где считать git-изменения сессии: здесь или на её узле.
///
/// Дифф с ЧУЖОЙ машины бессмысленно считать у себя: одноимённый каталог тут —
/// другой репозиторий, и человек увидел бы неправду (тот же довод, что у
/// file_read).
fn host_of(d: &std::sync::Arc<Daemon>, s: &crate::model::Session) -> Result<Host, String> {
    match &s.remote {
        None => Ok(Host::Local),
        Some(name) => match d.remotes.node(name) {
            Some(node) => Ok(Host::Ssh {
                machine: name.clone(),
                host: node.cfg.ssh_host.clone(),
            }),
            None => Err(format!("узел «{name}» не найден в настройках")),
        },
    }
}

/// Где и в каком каталоге считать изменения этой сессии.
fn session_place(app: &AppHandle, session_id: &str) -> Result<(Host, String), String> {
    let d = Daemon::get(app);
    let s = d.session(session_id).ok_or("сессия не найдена")?;
    let cwd = s
        .cwd
        .clone()
        .filter(|c| !c.trim().is_empty())
        .ok_or("у сессии нет рабочего каталога")?;
    Ok((host_of(&d, &s)?, cwd))
}

/// Свод изменений задачи: что агент наделал в рабочем каталоге.
#[tauri::command]
pub async fn session_changes(app: AppHandle, session_id: String) -> Value {
    match session_place(&app, &session_id) {
        Err(e) => err(e),
        Ok((host, cwd)) => match crate::changes::collect(&host, &cwd).await {
            Ok(v) => v,
            Err(e) => err(e),
        },
    }
}

/// Дифф файла из свода. Гейт — сам свод: показываем только то, что git и
/// правда считает изменённым, а не любой путь, пришедший из webview.
#[tauri::command]
pub async fn session_change_diff(app: AppHandle, session_id: String, path: String) -> Value {
    let (host, cwd) = match session_place(&app, &session_id) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let listed = match crate::changes::collect(&host, &cwd).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let Some(file) = listed_file(&listed, &path) else {
        return err("файл не в списке изменений");
    };
    match crate::changes::file_diff(&host, &cwd, &path, file.1).await {
        Ok(v) => v,
        Err(e) => err(e),
    }
}

/// Найти файл в своде: возвращает (есть ли, неотслеживаемый ли).
fn listed_file(listed: &Value, path: &str) -> Option<(bool, bool)> {
    listed
        .get("files")?
        .as_array()?
        .iter()
        .find(|f| f.get("path").and_then(Value::as_str) == Some(path))
        .map(|f| {
            (
                true,
                f.get("untracked").and_then(Value::as_bool).unwrap_or(false),
            )
        })
}

/// Принять правки: закоммитить выбранные файлы.
#[tauri::command]
pub async fn session_commit(
    app: AppHandle,
    session_id: String,
    message: String,
    paths: Vec<String>,
) -> Value {
    let (host, cwd) = match session_place(&app, &session_id) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let listed = match crate::changes::collect(&host, &cwd).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    // Каждый путь обязан быть в своде: коммит по подделанному пути забрал бы в
    // историю файл, которого человек не видел.
    for p in &paths {
        if listed_file(&listed, p).is_none() {
            return err(format!("«{p}» не в списке изменений"));
        }
    }
    match crate::changes::commit(&host, &cwd, &message, &paths).await {
        Ok(sha) => json!({ "ok": true, "sha": sha }),
        Err(e) => err(e),
    }
}

/// Позвать агента посмотреть на правки.
///
/// Ревью идёт там же, где правки: у задачи на узле — на узле. Модель берём из
/// настроек цикловского критика, чтобы «кем ревьюить» настраивалось в одном
/// месте, а не в двух.
#[tauri::command]
pub async fn session_review(app: AppHandle, session_id: String) -> Value {
    let (host, cwd) = match session_place(&app, &session_id) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let d = Daemon::get(&app);
    let model = d.settings.string("reviewModel");
    let model = if model.trim().is_empty() { None } else { Some(model) };
    match crate::changes::review(&host, &cwd, model.as_deref()).await {
        Ok((verdict, text)) => json!({ "ok": true, "verdict": verdict, "text": text }),
        Err(e) => err(e),
    }
}

/// Что тронула правка в файле: объявления, в которые попали изменения.
///
/// Ответ на «что именно он трогал» — список функций читается в сто раз
/// быстрее, чем сорок номеров строк.
#[tauri::command]
pub async fn session_touched(app: AppHandle, session_id: String, path: String) -> Value {
    let (host, cwd) = match session_place(&app, &session_id) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let listed = match crate::changes::collect(&host, &cwd).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let Some((_, untracked)) = listed_file(&listed, &path) else {
        return err("файл не в списке изменений");
    };
    let hunks = crate::changes::file_hunks(&host, &cwd, &path, untracked).await;
    // Содержимое читаем на той машине, где файл лежит, и ТОЛЬКО из stdout:
    // ворчание удалённого шелла, подмешанное к тексту файла, поехало бы в
    // разбор объявлений как строка кода.
    let Ok(text) = host
        .sh_data(
            &cwd,
            &format!("cat -- {}", crate::util::shell_quote(&path)),
            std::time::Duration::from_secs(20),
        )
        .await
    else {
        return err("файл не прочитался");
    };
    let syms = crate::symbols::symbols_of(&text);
    let touched = crate::symbols::touched(&syms, &crate::symbols::changed_lines(&hunks));
    json!({ "ok": true, "touched": touched })
}

/// Открыть превью: локальный адрес того, что подняла задача.
#[tauri::command]
pub async fn preview_open(app: AppHandle, url: String) -> Value {
    match crate::windows::preview_url(&url) {
        Err(e) => err(e),
        Ok(u) => match crate::windows::create_preview(&app, &u) {
            Ok(_) => json!({ "ok": true, "url": u }),
            Err(e) => err(format!("окно превью не открылось: {e}")),
        },
    }
}

/// Поиск по проекту задачи.
#[tauri::command]
pub async fn session_search(app: AppHandle, session_id: String, query: String) -> Value {
    let (host, cwd) = match session_place(&app, &session_id) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    match crate::search::search(&host, &cwd, &query).await {
        Ok(hits) => json!({
            "ok": true,
            "hits": hits,
            // Упёрлись в потолок — говорим об этом: «двести совпадений» и
            // «ровно двести» человек читает по-разному.
            "capped": hits.len() >= crate::search::MAX_HITS,
        }),
        Err(e) => err(e),
    }
}

/// Отправить коммиты задачи в удалённый репозиторий.
#[tauri::command]
pub async fn session_push(app: AppHandle, session_id: String) -> Value {
    let (host, cwd) = match session_place(&app, &session_id) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    match crate::changes::push(&host, &cwd).await {
        Ok(branch) => json!({ "ok": true, "branch": branch }),
        Err(e) => err(e),
    }
}

/// Откатить правку файла к последнему коммиту.
#[tauri::command]
pub async fn session_revert(app: AppHandle, session_id: String, path: String) -> Value {
    let (host, cwd) = match session_place(&app, &session_id) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let listed = match crate::changes::collect(&host, &cwd).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    match listed_file(&listed, &path) {
        None => err("файл не в списке изменений"),
        // Новый файл git не помнит — откатывать его нечем, а удалять молча
        // панель не станет: это единственная копия работы.
        Some((_, true)) => err("файл новый: git его не помнит — убери сам, если он лишний"),
        Some(_) => match crate::changes::revert(&host, &cwd, &path).await {
            Ok(_) => json!({ "ok": true }),
            Err(e) => err(e),
        },
    }
}

#[cfg(test)]
mod turn_ipc_tests {
    use super::*;

    /// «Проекты» строят заголовки из транскриптов и про переименование не знают —
    /// имя обязано лечь поверх, иначе один чат зовётся в двух списках по-разному.
    #[test]
    fn chat_name_overrides_the_history_title() {
        let mut projects = json!([{
            "project": "jarvis",
            "sessions": [
                { "id": "abc", "title": "Fix the migration parser" },
                { "id": "xyz", "title": "Другой чат" },
            ],
        }]);
        overlay_names(&mut projects, |id| (id == "abc").then(|| "БД".to_string()));
        assert_eq!(projects[0]["sessions"][0]["title"], "БД");
        assert_eq!(projects[0]["sessions"][0]["name"], "БД");
        assert_eq!(
            projects[0]["sessions"][1]["title"], "Другой чат",
            "безымянный чат остаётся на автозаголовке"
        );
        assert!(projects[0]["sessions"][1].get("name").is_none());

        // ошибка сборки истории приходит объектом, а не списком — не спотыкаемся
        let mut broken = json!({ "error": "история не собралась" });
        overlay_names(&mut broken, |_| Some("БД".into()));
        assert_eq!(broken["error"], "история не собралась");
    }

    /* Песочница задачи на НАСТОЯЩЕМ git: без этого «изоляция» — обещание на
     * словах. CI гоняет тест на macos-14, то есть там, где живёт панель. */
    #[tokio::test]
    async fn a_task_sandbox_is_a_real_worktree_next_to_the_project() {
        let repo = std::env::temp_dir()
            .join(format!("jarvis-sandbox-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let h = &Host::Local;
        assert_eq!(h.git(&repo, &["init", "-q", "-b", "main", "."]).await.0, 0);
        h.git(&repo, &["config", "user.email", "test@jarvis"]).await;
        h.git(&repo, &["config", "user.name", "jarvis"]).await;
        std::fs::write(std::path::Path::new(&repo).join("main.rs"), "fn main() {}\n").unwrap();
        h.git(&repo, &["add", "."]).await;
        assert_eq!(h.git(&repo, &["commit", "-q", "-m", "первый"]).await.0, 0);

        let dir = sandbox_for(h, &repo).await.expect("песочница");
        // Соседом с проектом, а не внутри него: туда заглядывают и открывают
        // редактором, а вложенный worktree путал бы сам себя.
        assert!(!dir.starts_with(&format!("{repo}/")), "песочница внутри проекта: {dir}");
        assert!(std::path::Path::new(&dir).join("main.rs").exists(), "рабочая копия пуста");
        // Своя ветка: правки задачи не смешаются с чужой работой.
        let (_, branch) = h.git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
        assert!(branch.trim().starts_with("task/"), "ветка песочницы: {branch}");

        let _ = h.git(&repo, &["worktree", "remove", "--force", &dir]).await;
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Не репозиторий — честный отказ с советом, а не запуск мимо изоляции.
    #[tokio::test]
    async fn a_sandbox_for_a_plain_directory_is_refused_with_a_reason() {
        let dir = std::env::temp_dir()
            .join(format!("jarvis-sandbox-plain-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let e = sandbox_for(&Host::Local, &dir).await.unwrap_err();
        assert!(e.contains("не репозиторий"), "{e}");
        assert!(e.contains("галочку"), "совет, что делать, не дан: {e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_user_file_relative_and_missing() {
        let dir = std::env::temp_dir().join(format!("jarvis-ipc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.rs"), "x").unwrap();
        let cwd = dir.to_string_lossy().to_string();

        let ok = resolve_user_file(Some(&cwd), "sub/a.rs").unwrap();
        assert!(ok.ends_with("sub/a.rs"));
        let abs = resolve_user_file(None, ok.to_str().unwrap()).unwrap();
        assert_eq!(abs, ok);

        assert!(resolve_user_file(Some(&cwd), "нет/такого.rs").is_err());
        assert!(resolve_user_file(None, "relative/without/cwd.rs").is_err());
        assert!(resolve_user_file(Some(&cwd), "sub").is_err(), "каталог — не файл");
    }

    #[test]
    fn remote_projects_keep_both_ids() {
        // Панели нужен ключ реестра (по нему она узнаёт уже известную сессию),
        // а агенту на той машине — его собственный id. Путать их нельзя:
        // `--resume vps:abc` там не найдёт ничего.
        let listing = json!([{
            "cwd": "/home/bob/my-proj",
            "count": 2,
            "lastAt": 1700,
            "sessions": [{ "id": "abc", "at": 1700 }, { "id": "def", "at": 1600 }],
        }]);
        let got = remote_projects_to_history("vps", listing);
        let g = &got[0];
        assert_eq!(g["project"], "my-proj", "имя проекта — из cwd, а не из имени каталога");
        assert_eq!(g["remote"], "vps");
        assert_eq!(g["sessions"][0]["id"], "vps:abc");
        assert_eq!(g["sessions"][0]["agentId"], "abc");
        assert_eq!(g["sessions"][0]["title"], "", "заголовков с узла нет — не выдумываем");
    }

    #[test]
    fn remote_projects_survive_a_listing_without_cwd() {
        // узел не смог достать cwd (пустой транскрипт, чужой формат) — список
        // всё равно должен нарисоваться, а не исчезнуть целиком
        let got = remote_projects_to_history("vps", json!([{ "sessions": [] }]));
        assert_eq!(got[0]["project"], "другое");
        assert_eq!(got[0]["cwd"], "");
        assert!(remote_projects_to_history("vps", json!("не массив")).as_array().unwrap().is_empty());
    }

    #[test]
    fn grants_patch_keeps_only_auto_approve() {
        // Через панель проходит поимённый список — и ничего сверх него: попытка
        // дописать классы/политику из патча вылетает при нормализации.
        let got = normalize_grants(&json!({
            "agent": { "autoApprove": ["sessions.reply", 7], "classes": ["admin"], "confirm": "never" },
        }));
        assert_eq!(got, json!({ "agent": { "autoApprove": ["sessions.reply"] } }));
        assert_eq!(normalize_grants(&json!({ "agent": "admin" })), json!({ "agent": { "autoApprove": [] } }));
        assert_eq!(normalize_grants(&json!("мусор")), json!({}));
    }

    #[test]
    fn force_reveal_blocks_executable_docs() {
        use std::path::Path;
        assert!(force_reveal(Path::new("a.command")), "исполняемый документ");
        assert!(force_reveal(Path::new("A.COMMAND")), "регистр не важен");
        assert!(force_reveal(Path::new("/tmp/x/run.scpt")));
        assert!(!force_reveal(Path::new("a.rs")), "обычный файл открываем");
        assert!(!force_reveal(Path::new("Makefile")), "без расширения — не блок");
    }

    /* --- бюджет перед дорогой работой --- */

    /// Отчёт бюджета той же формы, что отдаёт `budget::report`.
    fn budget_rep(rung: &str, reason: &str, left: f64, night: bool) -> Value {
        json!({
            "providers": {
                "claude": {
                    "rung": rung,
                    "reason": reason,
                    "weekLeftPct": left,
                    "reservePct": 14.2,
                    "weekResetAt": now_ms() + 2 * 3_600_000 + 40 * 60_000,
                    "bufferPct": 5.0,
                    "bufferAvailable": false,
                    "bufferReason": crate::budget::BUFFER_REASON,
                },
            },
            "night": { "active": night },
        })
    }

    /// Запуск сессии на ступени «стоп» отказывает ЧИСЛАМИ и временем сброса, а
    /// не «сейчас нельзя». После снятия потолка на число сессий это вообще
    /// единственная стена перед подъёмом — молчать ей нечем.
    #[test]
    fn a_spawn_on_the_stop_rung_is_refused_with_numbers() {
        let rep = budget_rep(
            "stop",
            "резерв начал расходоваться: осталось 12.5% при резерве 14.2%",
            12.5,
            false,
        );
        let e = budget_refusal("claude", &rep, false).expect("стоп обязан отказать");
        assert!(e.contains("12.5") && e.contains("14.2"), "отказ без чисел: {e}");
        assert!(e.contains("сброс через 2ч 40м"), "отказ без времени сброса: {e}");
        assert!(e.contains("sessions.close"), "отказ обязан сказать, что делать: {e}");
        assert!(e.contains("резерв начал расходоваться"), "причину переписали: {e}");
    }

    /// Фон отказывают раньше: `queue` — это и есть «отложить фоновое». На глазах
    /// у человека та же ступень запуску не мешает.
    #[test]
    fn a_background_pass_is_refused_one_rung_earlier() {
        let rep = budget_rep("queue", "запаса хода 30 ч против 107 ч до сброса", 40.0, false);
        let e = budget_refusal("claude", &rep, true).expect("фон на queue не идёт");
        assert!(e.contains("40.0") && e.contains("сброс через"), "{e}");
        assert!(e.contains("в очередь"), "фону надо сказать, что он отложен: {e}");
        assert!(budget_refusal("claude", &rep, false).is_none(), "человеку queue не стена");

        // спокойная ступень не мешает никому
        let ok = budget_rep("ok", "темп 3.0%/сут при норме 8.8%/сут", 60.0, false);
        assert!(budget_refusal("claude", &ok, true).is_none());
        assert!(budget_refusal("claude", &ok, false).is_none());
    }

    /// Ночной потолок — независимый ограничитель: он говорит «стоп» при живом
    /// дневном запасе, и отказ обязан назвать и потолок, и недоступный буфер.
    #[test]
    fn the_night_cap_stops_earlier_than_the_daily_norm() {
        let rep = budget_rep("stop", "ночной потолок 15% недели исчерпан (20.0%) — до утра стоп", 70.0, true);
        let e = budget_refusal("claude", &rep, false).expect("ночью потолок стоит раньше нормы");
        assert!(e.contains("ночной потолок"), "{e}");
        assert!(e.contains("70.0"), "70% остатка — а всё равно стоп: {e}");
        assert!(e.contains(crate::budget::BUFFER_REASON), "буфер ночью недоступен: {e}");
    }

    /// Стену сделал залп собственных запусков — отказ обязан это сказать
    /// числом: «осталось 20%» и молчаливый отказ выглядят враньём.
    #[test]
    fn a_wall_made_of_reservations_says_so() {
        let mut rep = budget_rep("stop", "резерв начал расходоваться", 20.0, false);
        rep["providers"]["claude"]["reservedPct"] = json!(6.5);
        rep["providers"]["claude"]["reservedCount"] = json!(13);
        let e = budget_refusal("claude", &rep, false).expect("стоп обязан отказать");
        assert!(e.contains("6.5") && e.contains("13"), "про придержанное молчат: {e}");
        assert!(e.contains("придержано"), "{e}");
        // а без броней текст прежний — лишних скобок в обычном отказе нет
        let plain = budget_refusal("claude", &budget_rep("stop", "резерв", 20.0, false), false).unwrap();
        assert!(!plain.contains("придержано"), "{plain}");
    }

    /// Молчание добытчика — не стена: `unknown` работу не рвёт, а у codex своей
    /// подписки в бюджете нет вовсе.
    #[test]
    fn silence_of_the_fetcher_is_not_a_wall() {
        let rep = budget_rep("unknown", "опросчик ещё не ходил за числами", 0.0, false);
        assert!(budget_refusal("claude", &rep, true).is_none());
        assert!(budget_refusal("kimi", &rep, true).is_none(), "чужого провайдера в отчёте нет");
        assert_eq!(budget_provider("claude"), Some("claude"));
        assert_eq!(budget_provider(" Kimi "), Some("kimi"));
        assert_eq!(budget_provider("codex"), None, "codex судить не по чему");
    }
}
