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

pub fn create_panel(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Jarvis")
        .inner_size(PANEL_W, PANEL_H)
        .visible(false)
        .decorations(false)
        // настоящий блюр подложки: нативный NSVisualEffectView, не CSS
        .transparent(true)
        .effects(WindowEffectsConfig {
            effects: vec![Effect::UnderWindowBackground],
            state: Some(EffectState::Active), // блюр не гаснет у неактивного окна (тихий показ)
            radius: Some(12.0),
            color: None,
        })
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .skip_taskbar(true)
        .shadow(true)
        .theme(Some(Theme::Dark)) // вибранси всегда тёмный, как дизайн
        .accept_first_mouse(true)
        .build()?;
    macos::float_above_everything(&win);
    Ok(win)
}

/// Окно онбординга первого запуска (стеклянное, по центру). Повторный вызов из
/// меню — показать и сфокусировать существующее, а не плодить копии.
pub fn create_onboarding(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(win) = app.get_webview_window("onboarding") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(win);
    }
    let win = WebviewWindowBuilder::new(app, "onboarding", WebviewUrl::App("onboarding.html".into()))
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
        .theme(Some(Theme::Dark))
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
    let win = WebviewWindowBuilder::new(app, "agent-chat", WebviewUrl::App("agent-chat.html".into()))
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
        .theme(Some(Theme::Dark))
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
        .theme(Some(Theme::Dark))
        .build()?;
    macos::float_above_everything(&win);
    Ok(win)
}

/* ================= доставка событий в окна ================= */

pub fn emit_to_panel<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: &P) {
    let _ = app.emit_to("main", event, payload.clone());
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
    d.toast_ready.store(true, std::sync::atomic::Ordering::SeqCst);
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
) {
    toast_emit(
        d,
        "toast-add",
        json!({
            "id": id, "title": title, "body": body,
            "sessionId": session_id, "kind": kind, "question": question,
        }),
    );
}

/* ================= позиционирование и показ панели ================= */

/// Панель — на дисплей с курсором (геометрия — в macos::place_panel:
/// AppKit-поинты, без конвертаций Tauri, иначе на смешанном DPI окно
/// уезжает на предыдущий экран).
pub fn position_panel(d: &Arc<Daemon>) {
    let Some(panel) = d.app.get_webview_window("main") else { return };
    let corner = d.settings.string("position") == "corner";
    macos::place_panel(&panel, PANEL_W, PANEL_H, corner);
}

/// Тихий режим: трей, клик по уведомлению — показать, не забирая фокус
/// у кино/терминала.
pub fn show_panel(d: &Arc<Daemon>) {
    // пока интеграция не установлена — основное приложение «заперто»: ведём к онбордингу
    if !crate::install::status().integrated() {
        let _ = create_onboarding(&d.app);
        return;
    }
    let Some(panel) = d.app.get_webview_window("main") else { return };
    position_panel(d);
    emit_to_panel(&d.app, "panel-shown", &json!(null));
    macos::show_inactive(&panel);
    d.push();
}

/// Raycast-режим: хоткей — с фокусом, потеря фокуса спрячет панель.
pub fn show_panel_focused(d: &Arc<Daemon>) {
    if !crate::install::status().integrated() {
        let _ = create_onboarding(&d.app);
        return;
    }
    let Some(panel) = d.app.get_webview_window("main") else { return };
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
        hide_panel(d);
    } else {
        show_panel_focused(d);
    }
}

/* ================= тост-окно ================= */

/// Рендерер тостов сообщает нужную высоту стека; 0 — спрятаться.
/// Низ прибит к краю экрана — окно растёт вверх.
pub fn toast_resize(d: &Arc<Daemon>, h: f64) {
    let Some(toast) = d.app.get_webview_window("toast") else { return };
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
