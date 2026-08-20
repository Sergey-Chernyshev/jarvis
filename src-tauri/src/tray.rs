//! Меню-бар: ромб состояния флота + не больше одного счётчика, и контекст-меню.
//!
//! Ширина строки в меню-баре — общий ресурс: на маке с чёлкой раздувшийся сосед
//! выталкивает других (или себя) под вырез. Поэтому в строке живёт ровно один
//! счётчик — самый срочный, — а бейджи плагинов уехали в меню, где их состояние
//! и так расписано словами. Что происходит целиком — в подсказке при наведении.
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
    #[allow(unused_mut)]
    let mut b = TrayIconBuilder::with_id("main")
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
        });
    #[cfg(target_os = "macos")]
    {
        b = b.icon(diamond(false)).icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        b = b.title(glyph(false));
    }
    b.build(&d.app)?;
    Ok(())
}

/// Ромб состояния флота: залитый — «тебя ждут», контурный — «никто не ждёт».
/// Та же оппозиция заливка/контур, что у точек в списке сессий, — она переживает
/// монохромный меню-бар и дальтонизм, в отличие от цвета.
///
/// На macOS рисуем картинку, а не пишем глиф: текст в меню-баре гуляет по ширине
/// от шрифта и рендерится цветной эмодзи, а template-картинка монохромна,
/// фиксированной ширины и одинаково читается на светлой и тёмной панели.
/// В Linux-трее иконку рисует окружение и понятия «template» там нет — глиф
/// текстом честнее чёрного ромба на тёмной панели.
#[cfg(target_os = "macos")]
fn diamond(filled: bool) -> tauri::image::Image<'static> {
    const S: usize = 36; // 18pt @2x — во всю высоту строки меню-бара
    const R: f64 = 15.0; // полудиагональ ромба, пиксели
    const STROKE: f64 = 3.5; // толщина контура
    let c = (S as f64 - 1.0) / 2.0;
    let mut rgba = vec![0u8; S * S * 4];
    for py in 0..S {
        for px in 0..S {
            // 3×3 подпикселя — иначе диагональ ромба выглядит лесенкой
            let mut hits = 0u32;
            for sy in 0..3 {
                for sx in 0..3 {
                    let x = px as f64 + (sx as f64 + 0.5) / 3.0 - 0.5 - c;
                    let y = py as f64 + (sy as f64 + 0.5) / 3.0 - 0.5 - c;
                    let d = x.abs() + y.abs(); // ромб — это |dx| + |dy| ≤ R
                    if d <= R && (filled || d >= R - STROKE) {
                        hits += 1;
                    }
                }
            }
            // template-картинке важна только альфа: цвет macOS подставит сама
            rgba[(py * S + px) * 4 + 3] = (255 * hits / 9) as u8;
        }
    }
    tauri::image::Image::new_owned(rgba, S as u32, S as u32)
}

#[cfg(not(target_os = "macos"))]
fn glyph(filled: bool) -> &'static str {
    if filled {
        "◆"
    } else {
        "◇"
    }
}

fn tray(d: &Arc<Daemon>) -> Option<TrayIcon> {
    d.app.tray_by_id("main")
}

/// Состояние флота в меню-баре: ромб (ждут/не ждут) + один счётчик.
pub fn update(d: &Arc<Daemon>, list: &[Session]) {
    let Some(tray) = tray(d) else { return };
    let waiting = list.iter().filter(|s| s.status == Status::Waiting).count();
    let working = list.iter().filter(|s| s.status == Status::Working).count();
    let done = list.iter().filter(|s| s.status == Status::Done).count();
    let filled = waiting > 0;

    let mut title = String::new();
    #[cfg(target_os = "macos")]
    set_diamond(&tray, filled);
    #[cfg(not(target_os = "macos"))]
    title.push_str(glyph(filled));
    let count = counter(waiting, working, done);
    if !count.is_empty() {
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(&count);
    }
    let _ = tray.set_title(Some(title));
    // то, чему не хватило ширины: полная раскладка + бейджи плагинов (☕⌒)
    let _ = tray.set_tooltip(Some(tooltip(waiting, working, done, &d.power.badges())));

    refresh_menu(d);
}

/// Единственный счётчик строки — самый срочный. «Готово» видно, только когда
/// никто не ждёт и никто не работает: доделанное подождёт, а живой вопрос — нет.
/// Глифы — текстовые (не эмодзи), иначе macOS рисует их цветными.
fn counter(waiting: usize, working: usize, done: usize) -> String {
    let n = |n: usize| if n > 99 { "99+".into() } else { n.to_string() };
    if waiting > 0 {
        format!("?{}", n(waiting)) // ждут ТВОЕГО ответа — это вопрос, а не пауза
    } else if working > 0 {
        format!("▸{}", n(working))
    } else if done > 0 {
        format!("✓{}", n(done))
    } else {
        String::new()
    }
}

/// Подсказка при наведении: вся картина словами, включая то, что не влезло.
fn tooltip(waiting: usize, working: usize, done: usize, badges: &str) -> String {
    let mut parts = vec!["Jarvis".to_string()];
    for (n, word) in [(waiting, "ждут"), (working, "работают"), (done, "готово")] {
        if n > 0 {
            parts.push(format!("{word} {n}"));
        }
    }
    if parts.len() == 1 {
        parts.push("сессий нет".into());
    }
    if !badges.is_empty() {
        parts.push(badges.to_string());
    }
    parts.join(" · ")
}

/// Иконку трогаем только на смене состояния: `update` зовётся на каждое событие
/// демона, а перерисовка status item'а — поход в AppKit.
#[cfg(target_os = "macos")]
fn set_diamond(tray: &TrayIcon, filled: bool) {
    use std::sync::atomic::{AtomicI8, Ordering};
    static LAST: AtomicI8 = AtomicI8::new(-1);
    let now = i8::from(filled);
    if LAST.swap(now, Ordering::SeqCst) == now {
        return;
    }
    let _ = tray.set_icon_with_as_template(Some(diamond(filled)), true);
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
    sig.push_str(if d.is_quiet() { "|q1" } else { "|q0" });
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
    menu.append(&MenuItem::with_id(app, "agent-chat", "Чат с агентом…", true, None::<&str>)?)?;
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
    menu.append(&CheckMenuItem::with_id(
        app, "quiet", "Тихий режим (разработчик) · ⌘⌥J", true, d.is_quiet(), None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(app, "reinstall", "Переустановить интеграцию…", true, None::<&str>)?)?;
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
        "agent-chat" => {
            let _ = windows::create_agent_chat(&d.app);
        }
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
        "quiet" => {
            d.toggle_quiet(); // тумблер тихого режима; перерисует меню сам
        }
        "reinstall" => {
            let _ = windows::create_onboarding(&d.app);
        }
        "quit" => {
            d.app.exit(0); // уборка — в RunEvent::Exit
        }
        other => {
            Power::handle_menu(d, other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Строка меню-бара не должна гулять по ширине с нагрузкой: один счётчик,
    /// самый срочный, и потолок на числе.
    #[test]
    fn menubar_counter_shows_only_the_most_urgent() {
        assert_eq!(counter(0, 0, 0), "");
        assert_eq!(counter(2, 3, 4), "?2"); // ждут важнее всего
        assert_eq!(counter(0, 3, 4), "▸3");
        assert_eq!(counter(0, 0, 4), "✓4"); // «доделано» видно, когда работа кончилась
        assert_eq!(counter(1234, 0, 0), "?99+");
        for (w, r, dn) in [(0, 0, 0), (2, 3, 4), (0, 0, 7), (1234, 5, 6)] {
            assert!(counter(w, r, dn).chars().count() <= 4, "{w} {r} {dn}");
        }
    }

    /// Подсказка держит то, чему не хватило ширины, — включая бейджи плагинов.
    #[test]
    fn tooltip_spells_out_what_the_title_dropped() {
        assert_eq!(tooltip(0, 0, 0, ""), "Jarvis · сессий нет");
        assert_eq!(tooltip(2, 3, 4, ""), "Jarvis · ждут 2 · работают 3 · готово 4");
        assert_eq!(tooltip(0, 1, 0, "☕⌒"), "Jarvis · работают 1 · ☕⌒");
    }

    /// Залитый ромб против контурного: разница должна быть в пикселях, а не
    /// только в намерении, — и оба должны остаться ромбами (углы прозрачны).
    #[cfg(target_os = "macos")]
    #[test]
    fn filled_diamond_differs_from_outlined() {
        let (empty, full) = (diamond(false), diamond(true));
        let (w, h) = (empty.width() as usize, empty.height() as usize);
        let alpha = |img: &tauri::image::Image<'_>, x: usize, y: usize| img.rgba()[(y * w + x) * 4 + 3];

        assert_eq!(alpha(&full, w / 2, h / 2), 255, "залитый — непрозрачный центр");
        assert_eq!(alpha(&empty, w / 2, h / 2), 0, "контурный — пустой центр");
        for img in [&empty, &full] {
            assert_eq!(alpha(img, 0, 0), 0, "угол ромба прозрачен");
            assert_eq!(alpha(img, w / 2, 1), 0, "ромб не касается края");
            assert!(alpha(img, w / 2, h / 2 - 13) > 0, "грань ромба на месте");
        }
    }
}
