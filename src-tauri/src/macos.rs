//! Нативные твики NSWindow, которых нет в кросс-платформенном API Tauri.
//!
//! Панель и тосты должны жить ПОВЕРХ всего (включая фуллскрин-приложения),
//! на всех Spaces, и показываться не воруя фокус — это уровень screen-saver
//! плюс коллекция CanJoinAllSpaces|FullScreenAuxiliary, как у Raycast/Spotlight.

use objc2::encode::{Encode, Encoding};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use tauri::WebviewWindow;

const NS_SCREEN_SAVER_WINDOW_LEVEL: isize = 1000;
/// NSWindowCollectionBehaviorCanJoinAllSpaces | NSWindowCollectionBehaviorFullScreenAuxiliary
const COLLECTION_BEHAVIOR: usize = (1 << 0) | (1 << 8);

/* CGPoint/CGRect для msg_send — свои repr(C), чтобы не тянуть objc2-foundation */

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

/// Все вызовы AppKit — строго на главном потоке.
fn on_main(win: &WebviewWindow, f: impl FnOnce(*mut AnyObject) + Send + 'static) {
    let w = win.clone();
    let _ = win.run_on_main_thread(move || {
        if let Ok(ptr) = w.ns_window() {
            f(ptr as *mut AnyObject);
        }
    });
}

/// Поверх всего, на всех Spaces, над фуллскрином — но без кражи фокуса при показе.
pub fn float_above_everything(win: &WebviewWindow) {
    on_main(win, |w| unsafe {
        let _: () = msg_send![w, setLevel: NS_SCREEN_SAVER_WINDOW_LEVEL];
        let _: () = msg_send![w, setCollectionBehavior: COLLECTION_BEHAVIOR];
        let _: () = msg_send![w, setHidesOnDeactivate: false];
    });
}

/// Показать окно, не активируя приложение (аналог showInactive в Electron):
/// orderFrontRegardless выводит окно на экран, не делая его key.
pub fn show_inactive(win: &WebviewWindow) {
    on_main(win, |w| unsafe {
        let _: () = msg_send![w, orderFrontRegardless];
    });
}

/* ================= позиционирование на дисплее с курсором ================= */
/* Считаем ЦЕЛИКОМ в AppKit-поинтах (NSEvent.mouseLocation → NSScreen.
 * visibleFrame → setFrame:) — это та же логическая система координат, что
 * DIP у Electron. Конвертации Tauri physical↔logical на маках со смешанным
 * DPI дают рассинхрон: окно уезжало на предыдущий дисплей. */

/// visibleFrame экрана под курсором (рабочая область без меню-бара и дока).
/// Хит-тест — по полному frame; мимо всех экранов → mainScreen.
unsafe fn work_area_under_cursor() -> Option<CGRect> {
    let mouse: CGPoint = msg_send![class!(NSEvent), mouseLocation];
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    if screens.is_null() {
        return None;
    }
    let count: usize = msg_send![screens, count];
    let mut hit: *mut AnyObject = std::ptr::null_mut();
    for i in 0..count {
        let scr: *mut AnyObject = msg_send![screens, objectAtIndex: i];
        let f: CGRect = msg_send![scr, frame];
        if mouse.x >= f.origin.x
            && mouse.x < f.origin.x + f.size.width
            && mouse.y >= f.origin.y
            && mouse.y < f.origin.y + f.size.height
        {
            hit = scr;
            break;
        }
    }
    if hit.is_null() {
        hit = msg_send![class!(NSScreen), mainScreen];
        if hit.is_null() {
            return None;
        }
    }
    Some(msg_send![hit, visibleFrame])
}

/// Поставить окно (w×h поинтов) на дисплей с курсором.
/// `corner` — правый верхний угол с отступом 12; иначе центр, ~⅓ сверху
/// (как Raycast). Геометрия повторяет positionPanel Electron-версии.
pub fn place_panel(win: &WebviewWindow, w: f64, h: f64, corner: bool) {
    on_main(win, move |window| unsafe {
        let Some(vf) = work_area_under_cursor() else { return };
        let (x, y_bottom) = if corner {
            (
                vf.origin.x + vf.size.width - w - 12.0,
                vf.origin.y + vf.size.height - 12.0 - h,
            )
        } else {
            (
                vf.origin.x + ((vf.size.width - w) / 2.0).round(),
                // отступ сверху (vf.h − h)/3 → в AppKit-координатах снизу:
                vf.origin.y + vf.size.height - ((vf.size.height - h) / 3.0).round() - h,
            )
        };
        let frame = CGRect {
            origin: CGPoint { x, y: y_bottom },
            size: CGSize { width: w, height: h },
        };
        let _: () = msg_send![window, setFrame: frame, display: false];
    });
}

/// Стек тостов: по центру дисплея с курсором, низ прибит к краю (отступ 14).
pub fn place_toast(win: &WebviewWindow, w: f64, h: f64) {
    on_main(win, move |window| unsafe {
        let Some(vf) = work_area_under_cursor() else { return };
        let frame = CGRect {
            origin: CGPoint {
                x: vf.origin.x + ((vf.size.width - w) / 2.0).round(),
                y: vf.origin.y + 14.0,
            },
            size: CGSize { width: w, height: h },
        };
        let _: () = msg_send![window, setFrame: frame, display: true];
    });
}
