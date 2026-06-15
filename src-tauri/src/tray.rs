//! Меню-бар: текстовый title (◇ + бейджи плагинов + счётчики) и контекст-меню.
//!
//! Клик — панель, правый клик — меню. В отличие от Electron, у Tauri меню
//! строится заранее, а не в момент клика — поэтому пересобираем его при каждом
//! изменении состояния (с дедупом по сигнатуре, чтобы не дёргать AppKit зря).

use std::sync::{Arc, Mutex, OnceLock};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::Wry;
use tauri_plugin_autostart::ManagerExt;

use crate::daemon::Daemon;
use crate::model::{Session, Status};
use crate::power::{Power, TrayItem};
use crate::windows;

static MENU_SIGNATURE: OnceLock<Mutex<String>> = OnceLock::new();

pub fn init(d: &Arc<Daemon>) -> tauri::Result<()> {
    let menu = build_menu(d)?;
    let d_menu = d.clone();
    let d_click = d.clone();
    TrayIconBuilder::with_id("main")
        .title("◇")
        .tooltip("Jarvis — монитор сессий Claude Code")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |_app, event| on_menu(&d_menu, event.id().as_ref()))
        .on_tray_icon_event(move |_tray, event| {
            if let TrayIconEvent::Click { button, button_state, .. } = event {
                match (button, button_state) {
                    (MouseButton::Left, MouseButtonState::Up) => windows::toggle_panel(&d_click),
                    (MouseButton::Right, MouseButtonState::Down) => {
                        // освежить кандидатов «пока жив процесс» и состояние
                        // крышки к СЛЕДУЮЩЕМУ открытию меню
                        Power::refresh_processes(&d_click);
                    }
                    _ => {}
                }
            }
        })
        .build(&d.app)?;
    Ok(())
}

fn tray(d: &Arc<Daemon>) -> Option<TrayIcon> {
    d.app.tray_by_id("main")
}

/// Title: ◇ + бейджи плагинов (☕⌒) + ⏸ ждут · ⚙ работают · ✓ готово.
pub fn update(d: &Arc<Daemon>, list: &[Session]) {
    let Some(tray) = tray(d) else { return };
    let waiting = list.iter().filter(|s| s.status == Status::Waiting).count();
    let working = list.iter().filter(|s| s.status == Status::Working).count();
    let done = list.iter().filter(|s| s.status == Status::Done).count();

    let mut title = String::from("◇");
    let badges = d.power.badges();
    if !badges.is_empty() {
        title.push(' ');
        title.push_str(&badges);
    }
    let mut parts = Vec::new();
    if waiting > 0 {
        parts.push(format!("⏸{waiting}"));
    }
    if working > 0 {
        parts.push(format!("⚙{working}"));
    }
    if parts.is_empty() && done > 0 {
        parts.push(format!("✓{done}"));
    }
    if !parts.is_empty() {
        title.push(' ');
        title.push_str(&parts.join(" "));
    }
    let _ = tray.set_title(Some(title));

    refresh_menu(d);
}

/// Пересборка контекст-меню — только если его содержимое реально изменилось.
fn refresh_menu(d: &Arc<Daemon>) {
    let signature = menu_signature(d);
    let cell = MENU_SIGNATURE.get_or_init(|| Mutex::new(String::new()));
    {
        let mut last = cell.lock().unwrap();
        if *last == signature {
            return;
        }
        *last = signature;
    }
    let Some(tray) = tray(d) else { return };
    if let Ok(menu) = build_menu(d) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// Сигнатура меню: всё, от чего зависит его содержимое.
fn menu_signature(d: &Arc<Daemon>) -> String {
    let mut sig = String::new();
    push_items_signature(&d.power.tray_items(d), &mut sig);
    sig.push_str(&format!("|login:{}", autostart_enabled(d)));
    sig.push_str(if d.voice.is_muted() { "|mute1" } else { "|mute0" });
    sig
}

fn push_items_signature(items: &[TrayItem], sig: &mut String) {
    for item in items {
        match item {
            TrayItem::Label { text } => sig.push_str(&format!("L:{text};")),
            TrayItem::Action { id, text } => sig.push_str(&format!("A:{id}:{text};")),
            TrayItem::Check { id, text, checked, enabled } => {
                sig.push_str(&format!("C:{id}:{text}:{checked}:{enabled};"))
            }
            TrayItem::Submenu { text, items } => {
                sig.push_str(&format!("S:{text}["));
                push_items_signature(items, sig);
                sig.push(']');
            }
            TrayItem::Separator => sig.push('-'),
        }
    }
}

fn autostart_enabled(d: &Arc<Daemon>) -> bool {
    d.app.autolaunch().is_enabled().unwrap_or(false)
}

fn build_menu(d: &Arc<Daemon>) -> tauri::Result<Menu<Wry>> {
    let app = &d.app;
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "show-panel", "Показать панель", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "test-notify", "Тестовое уведомление", true, None::<&str>)?)?;

    let plugin_items = d.power.tray_items(d);
    if !plugin_items.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        append_items(d, &menu, &plugin_items)?;
    }

    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&CheckMenuItem::with_id(
        app, "voice-mute", "Без звука", true, d.voice.is_muted(), None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(app, "voice-test", "Тест голоса", true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&CheckMenuItem::with_id(
        app, "autostart", "Запускать при старте компьютера", true, autostart_enabled(d), None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Выйти", true, None::<&str>)?)?;
    Ok(menu)
}

fn append_items(d: &Arc<Daemon>, menu: &Menu<Wry>, items: &[TrayItem]) -> tauri::Result<()> {
    let app = &d.app;
    for item in items {
        match item {
            TrayItem::Label { text } => {
                menu.append(&MenuItem::new(app, text, false, None::<&str>)?)?;
            }
            TrayItem::Action { id, text } => {
                menu.append(&MenuItem::with_id(app, id, text, true, None::<&str>)?)?;
            }
            TrayItem::Check { id, text, checked, enabled } => {
                menu.append(&CheckMenuItem::with_id(app, id, text, *enabled, *checked, None::<&str>)?)?;
            }
            TrayItem::Submenu { text, items } => {
                let sub = Submenu::new(app, text, true)?;
                for inner in items {
                    match inner {
                        TrayItem::Label { text } => {
                            sub.append(&MenuItem::new(app, text, false, None::<&str>)?)?
                        }
                        TrayItem::Action { id, text } => {
                            sub.append(&MenuItem::with_id(app, id, text, true, None::<&str>)?)?
                        }
                        _ => {}
                    }
                }
                menu.append(&sub)?;
            }
            TrayItem::Separator => {
                menu.append(&PredefinedMenuItem::separator(app)?)?;
            }
        }
    }
    Ok(())
}

fn on_menu(d: &Arc<Daemon>, id: &str) {
    match id {
        "show-panel" => windows::show_panel(d),
        "test-notify" => {
            d.notify("Jarvis на связи", "Уведомления работают", None, "done");
        }
        "voice-mute" => {
            d.voice.set_mute(!d.voice.is_muted());
            refresh_menu(d);
        }
        "voice-test" => {
            d.voice.test_phrase("Проверка голоса. Пиксела: четыре из шести задач, сейчас docker-compose.");
        }
        "autostart" => {
            let autolaunch = d.app.autolaunch();
            let enabled = autolaunch.is_enabled().unwrap_or(false);
            let _ = if enabled { autolaunch.disable() } else { autolaunch.enable() };
            refresh_menu(d);
        }
        "quit" => {
            d.app.exit(0); // уборка — в RunEvent::Exit
        }
        other => {
            Power::handle_menu(d, other);
        }
    }
}
