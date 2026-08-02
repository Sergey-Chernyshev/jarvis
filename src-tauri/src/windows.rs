//! Окна Jarvis: панель (raycast-стиль) и стек тостов.
//!
//! Оба окна создаются на старте скрытыми и живут весь срок демона:
//! закрытие панели (⌘W, крестик) — это hide, не destroy.

use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tauri::utils::config::WindowEffectsConfig;
use tauri::window::{Effect, EffectState};
use tauri::{AppHandle, Emitter, Manager, Theme, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::daemon::Daemon;
use crate::macos;

pub const PANEL_W: f64 = 820.0;
pub const PANEL_H: f64 = 620.0;
pub const TOAST_W: f64 = 440.0;
pub const TOAST_MAX_H: f64 = 480.0;
pub const ONBOARD_W: f64 = 480.0;
pub const ONBOARD_H: f64 = 600.0;
pub const AGENT_W: f64 = 460.0;
pub const AGENT_H: f64 = 600.0;

/// Оконный режим (макет 14h): список слева 264px + диалог справа.
pub const WINDOW_W: f64 = 1120.0;
pub const WINDOW_H: f64 = 640.0;
pub const WINDOW_MIN_W: f64 = 720.0;
pub const WINDOW_MIN_H: f64 = 420.0;

/// Настройка `mode`: `true` — обычное окно, `false` — накладка ⌘J.
/// try_state, а не Daemon::get: окна строятся до регистрации стейта.
pub fn is_window_mode(app: &AppHandle) -> bool {
    app.try_state::<Arc<Daemon>>()
        .map(|d| d.settings.string("mode") == "window")
        .unwrap_or(false)
}

/// Запомненный размер окна (или размер из макета, если ещё не меняли).
fn window_size(app: &AppHandle) -> (f64, f64) {
    let Some(d) = app.try_state::<Arc<Daemon>>() else {
        return (WINDOW_W, WINDOW_H);
    };
    let cfg = d.settings.load();
    let num = |k: &str, def: f64| cfg.get(k).and_then(|v| v.as_f64()).unwrap_or(def);
    (
        num("windowW", WINDOW_W).max(WINDOW_MIN_W),
        num("windowH", WINDOW_H).max(WINDOW_MIN_H),
    )
}

/// Тема нативного материала окна. Панель — непрозрачная «бумага», но по
/// скруглённым углам просвечивает NSVisualEffectView: он должен совпадать с
/// выбранной темой, иначе на светлой панели видна тёмная кайма.
/// `auto` отдаём системе (`None` — Tauri берёт системную).
fn window_theme(app: &AppHandle) -> Option<Theme> {
    // окна строятся на старте — демон может быть ещё не зарегистрирован в state,
    // поэтому try_state, а не Daemon::get (тот паникует на отсутствующем стейте)
    let theme = app
        .try_state::<Arc<Daemon>>()
        .map(|d| d.settings.string("theme"))
        .unwrap_or_else(|| "light".into());
    match theme.as_str() {
        "dark" => Some(Theme::Dark),
        "auto" => None,
        _ => Some(Theme::Light),
    }
}

/// Главное окно. В накладке — раскладка Raycast (поверх всего, без дока,
/// не тянется); в оконном режиме — обычное окно: тянется, сворачивается,
/// живёт на своём уровне. Разметка одна и та же, её перестраивает CSS
/// по `data-mode` (см. `ui/theme.js`).
pub fn create_panel(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let window_mode = is_window_mode(app);
    let (w, h) = if window_mode {
        window_size(app)
    } else {
        (PANEL_W, PANEL_H)
    };
    let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Jarvis")
        .inner_size(w, h)
        .min_inner_size(WINDOW_MIN_W, WINDOW_MIN_H)
        .visible(false)
        // светофор рисуем сами (14h), поэтому системных декораций нет в обоих режимах
        .decorations(false)
        // настоящий блюр подложки: нативный NSVisualEffectView, не CSS
        .transparent(true)
        .effects(WindowEffectsConfig {
            effects: vec![Effect::UnderWindowBackground],
            state: Some(EffectState::Active), // блюр не гаснет у неактивного окна (тихий показ)
            radius: Some(16.0),
            color: None,
        })
        .resizable(window_mode)
        .minimizable(window_mode)
        .maximizable(window_mode)
        .skip_taskbar(!window_mode)
        .shadow(true)
        .theme(window_theme(app)) // материал под тему из настроек (см. window_theme)
        .accept_first_mouse(true)
        .build()?;
    if window_mode {
        macos::float_normal(&win);
    } else {
        macos::float_above_everything(&win);
    }
    Ok(win)
}

/// Переключение режима на лету: окно уже создано, поэтому меняем его свойства,
/// а не пересоздаём (иначе улетели бы открытый чат и позиция). Иконка в доке
/// (ActivationPolicy) ставится на старте — она подхватится со следующего запуска.
pub fn apply_mode(d: &Arc<Daemon>) {
    let Some(win) = d.app.get_webview_window("main") else {
        return;
    };
    let window_mode = d.settings.string("mode") == "window";
    let _ = win.set_resizable(window_mode);
    let _ = win.set_maximizable(window_mode);
    let _ = win.set_minimizable(window_mode);
    let _ = win.set_skip_taskbar(!window_mode);
    if window_mode {
        macos::float_normal(&win);
        let (w, h) = window_size(&d.app);
        let _ = win.set_size(tauri::LogicalSize::new(w, h));
        let _ = win.center();
        let _ = win.show();
        let _ = win.set_focus();
    } else {
        macos::float_above_everything(&win);
        let _ = win.set_size(tauri::LogicalSize::new(PANEL_W, PANEL_H));
        position_panel(d);
    }
}

/// Запомнить размер окна, чтобы следующий запуск открылся таким же.
pub fn remember_window_size(d: &Arc<Daemon>, w: f64, h: f64) {
    if d.settings.string("mode") != "window" {
        return; // накладка не тянется — её размер считает place_panel
    }
    let mut patch = serde_json::Map::new();
    patch.insert("windowW".into(), json!(w.round()));
    patch.insert("windowH".into(), json!(h.round()));
    d.settings.save(patch);
}

/// Окно онбординга первого запуска (стеклянное, по центру). Повторный вызов из
/// меню — показать и сфокусировать существующее, а не плодить копии.
pub fn create_onboarding(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(win) = app.get_webview_window("onboarding") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(win);
    }
    let win =
        WebviewWindowBuilder::new(app, "onboarding", WebviewUrl::App("onboarding.html".into()))
            .title("Jarvis")
            .inner_size(ONBOARD_W, ONBOARD_H)
            .visible(true)
            .decorations(false)
            .transparent(true)
            .effects(WindowEffectsConfig {
                effects: vec![Effect::UnderWindowBackground],
                state: Some(EffectState::Active),
                radius: Some(16.0),
                color: None,
            })
            .resizable(false)
            .minimizable(false)
            .maximizable(false)
            .skip_taskbar(true)
            .shadow(true)
            .center()
            .theme(window_theme(app))
            .accept_first_mouse(true)
            .build()?;
    let _ = win.set_focus();
    Ok(win)
}

/// Окно чата с агентом (фаза 7): стеклянное, по центру, ресайзится. Повторный
/// вызов — показать существующее, а не плодить копии.
pub fn create_agent_chat(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(win) = app.get_webview_window("agent-chat") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(win);
    }
    let win =
        WebviewWindowBuilder::new(app, "agent-chat", WebviewUrl::App("agent-chat.html".into()))
            .title("Jarvis · агент")
            .inner_size(AGENT_W, AGENT_H)
            .min_inner_size(360.0, 380.0)
            .visible(true)
            .decorations(false)
            .transparent(true)
            .effects(WindowEffectsConfig {
                effects: vec![Effect::UnderWindowBackground],
                state: Some(EffectState::Active),
                radius: Some(16.0),
                color: None,
            })
            .resizable(true)
            .minimizable(false)
            .maximizable(false)
            .skip_taskbar(true)
            .shadow(true)
            .center()
            .theme(window_theme(app))
            .accept_first_mouse(true)
            .build()?;
    let _ = win.set_focus();
    Ok(win)
}

pub fn create_toast(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let win = WebviewWindowBuilder::new(app, "toast", WebviewUrl::App("toast.html".into()))
        .title("")
        .inner_size(TOAST_W, 120.0)
        .visible(false)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .skip_taskbar(true)
        .shadow(false) // форму рисует карточка, а не системное окно
        .focusable(false) // клики работают, фокус не воруется
        .accept_first_mouse(true)
        .theme(window_theme(app))
        .build()?;
    macos::float_above_everything(&win);
    Ok(win)
}

/* ================= доставка событий в окна ================= */

pub fn emit_to_panel<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: &P) {
    let _ = app.emit_to("main", event, payload.clone());
}

/// Тема/краска сменились — разослать всем окнам, чтобы панель, тосты, чат и
/// онбординг перекрасились одновременно (`theme.js` слушает `appearance`).
pub fn broadcast_appearance(app: &AppHandle, theme: &str, paint: &str, mode: &str) {
    let payload = json!({ "theme": theme, "paint": paint, "mode": mode });
    for label in ["main", "toast", "agent-chat", "onboarding"] {
        let _ = app.emit_to(label, "appearance", payload.clone());
    }
}

/// Эмит события напрямую в окно `toast` (для прямых эмиттеров вне `Daemon`,
/// напр. AudioHub — он держит только `AppHandle`, не буфер тостов).
pub fn emit_to_toast_window<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: &P) {
    let _ = app.emit_to("toast", event, payload.clone());
}

/// Голос начал говорить эту карточку — держим открытой (не закрываем по TTL).
pub fn toast_hold(app: &AppHandle, id: &str) {
    let _ = app.emit_to("toast", "toast-hold", json!({ "id": id }));
}

/// Голос закончил — карточка живёт ещё `ms` (≈3.5с после речи).
pub fn toast_extend(app: &AppHandle, id: &str, ms: u64) {
    let _ = app.emit_to("toast", "toast-extend", json!({ "id": id, "ms": ms }));
}

/// Снять карточку тоста по id (вопрос ответили → убрать «липкую» карточку).
pub fn toast_remove(d: &Daemon, id: &str) {
    toast_emit(d, "toast-remove", json!({ "id": id }));
}

/// События тостов до загрузки webview буферятся (аналог did-finish-load
/// в Electron) — уведомления первых секунд после старта демона не теряются.
fn toast_emit(d: &Daemon, event: &'static str, payload: serde_json::Value) {
    if d.toast_ready.load(std::sync::atomic::Ordering::SeqCst) {
        let _ = d.app.emit_to("toast", event, payload);
    } else {
        d.pending_toasts.lock().unwrap().push((event, payload));
    }
}

/// Эмит голосового HUD-события (`voice-hud`) в окно `toast`. НАПРЯМУЮ (не через
/// буфер ранних тостов): фазы цикла — реалтайм, проигрывать «протухшую» фазу с
/// прошлого запуска бессмысленно; а буфер флашится по armed()=onAdd+onUpdate, и
/// voice-hud мог флашнуться ДО регистрации своего слушателя (F1).
pub fn hud_emit(d: &Daemon, payload: serde_json::Value) {
    let _ = d.app.emit_to("toast", "voice-hud", payload);
}

/// Мост тостов загрузился: доливаем накопленное в исходном порядке.
pub fn toast_flush(d: &Daemon) {
    d.toast_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    for (event, payload) in d.pending_toasts.lock().unwrap().drain(..) {
        let _ = d.app.emit_to("toast", event, payload);
    }
}

pub fn toast_add(
    d: &Daemon,
    id: &str,
    title: &str,
    body: &str,
    session_id: Option<&str>,
    kind: &str,
    question: Option<&serde_json::Value>,
    meta: &serde_json::Value,
) {
    let payload = toast_payload(
        &d.settings.load(),
        id,
        title,
        body,
        session_id,
        kind,
        question,
        meta,
    );
    toast_emit(d, "toast-add", payload);
}

#[allow(clippy::too_many_arguments)]
fn toast_payload(
    settings: &serde_json::Value,
    id: &str,
    title: &str,
    body: &str,
    session_id: Option<&str>,
    kind: &str,
    question: Option<&serde_json::Value>,
    meta: &serde_json::Value,
) -> serde_json::Value {
    let ttl_ms = settings
        .pointer("/notify/ttlSec")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(8)
        .saturating_mul(1_000);

    json!({
        "id": id, "title": title, "body": body,
        "sessionId": session_id, "kind": kind, "question": question,
        "meta": meta, "ttlMs": ttl_ms,
    })
}

/* ================= позиционирование и показ панели ================= */

/// Панель — на дисплей с курсором (геометрия — в macos::place_panel:
/// AppKit-поинты, без конвертаций Tauri, иначе на смешанном DPI окно
/// уезжает на предыдущий экран).
pub fn position_panel(d: &Arc<Daemon>) {
    let Some(panel) = d.app.get_webview_window("main") else {
        return;
    };
    // окно пользователь ставит сам — не таскаем его под курсор на каждый показ
    if d.settings.string("mode") == "window" {
        return;
    }
    let corner = d.settings.string("position") == "corner";
    macos::place_panel(&panel, PANEL_W, PANEL_H, corner);
}

/// Тихий режим: трей, клик по уведомлению — показать, не забирая фокус
/// у кино/терминала.
pub fn show_panel(d: &Arc<Daemon>) {
    // пока интеграция не установлена — основное приложение «заперто»: ведём к онбордингу
    if !crate::install::integration_health().ok() {
        let _ = create_onboarding(&d.app);
        return;
    }
    let Some(panel) = d.app.get_webview_window("main") else {
        return;
    };
    position_panel(d);
    emit_to_panel(&d.app, "panel-shown", &json!(null));
    if d.settings.string("mode") == "window" {
        let _ = panel.show();
    } else {
        macos::show_inactive(&panel);
    }
    d.push();
}

/// Raycast-режим: хоткей — с фокусом, потеря фокуса спрячет панель.
pub fn show_panel_focused(d: &Arc<Daemon>) {
    if !crate::install::integration_health().ok() {
        let _ = create_onboarding(&d.app);
        return;
    }
    let Some(panel) = d.app.get_webview_window("main") else {
        return;
    };
    position_panel(d);
    emit_to_panel(&d.app, "panel-shown", &json!(null));
    let _ = panel.show();
    let _ = panel.set_focus();
    d.push();
}

pub fn panel_visible(d: &Arc<Daemon>) -> bool {
    d.app
        .get_webview_window("main")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

pub fn hide_panel(d: &Arc<Daemon>) {
    // запись сочетания не должна пережить панель — вернуть хоткеи
    crate::ipc::hotkeys_set_suspended(d, false);
    if let Some(panel) = d.app.get_webview_window("main") {
        let _ = panel.hide();
    }
}

pub fn toggle_panel(d: &Arc<Daemon>) {
    if panel_visible(d) {
        hide_panel(d);
    } else {
        show_panel(d);
    }
}

pub fn toggle_hotkey_panel(d: &Arc<Daemon>) {
    if panel_visible(d) {
        // Окно живёт под другими окнами: ⌘J по нему должен поднимать, а не прятать.
        // Прячем только когда оно уже в фокусе — тогда хоткей читается как «убрать».
        if d.settings.string("mode") == "window"
            && !d
                .app
                .get_webview_window("main")
                .and_then(|w| w.is_focused().ok())
                .unwrap_or(false)
        {
            show_panel_focused(d);
            return;
        }
        hide_panel(d);
    } else {
        show_panel_focused(d);
    }
}

/* ================= тост-окно ================= */

/// Рендерер тостов сообщает нужную высоту стека; 0 — спрятаться.
/// Низ прибит к краю экрана — окно растёт вверх.
pub fn toast_resize(d: &Arc<Daemon>, h: f64) {
    let Some(toast) = d.app.get_webview_window("toast") else {
        return;
    };
    if h <= 0.0 {
        let _ = toast.hide();
        return;
    }
    let height = h.round().clamp(1.0, TOAST_MAX_H);
    macos::place_toast(&toast, TOAST_W, height);
    if !toast.is_visible().unwrap_or(false) {
        macos::show_inactive(&toast);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_toast_ttl_reaches_payload_in_milliseconds() {
        for (seconds, milliseconds) in [(5, 5_000), (8, 8_000), (0, 0)] {
            let settings = json!({ "notify": { "ttlSec": seconds } });
            let payload = toast_payload(
                &settings,
                "id",
                "title",
                "body",
                None,
                "done",
                None,
                &json!([]),
            );

            assert_eq!(payload["ttlMs"], milliseconds);
        }
    }

    #[test]
    fn invalid_toast_ttl_falls_back_to_eight_seconds() {
        for settings in [
            json!({}),
            json!({ "notify": { "ttlSec": "5" } }),
            json!({ "notify": { "ttlSec": -1 } }),
        ] {
            let payload = toast_payload(
                &settings,
                "id",
                "title",
                "body",
                None,
                "done",
                None,
                &json!([]),
            );

            assert_eq!(payload["ttlMs"], 8_000);
        }
    }
}
