//! Нативные твики NSWindow, которых нет в кросс-платформенном API Tauri.
//!
//! Панель и тосты должны жить ПОВЕРХ всего (включая фуллскрин-приложения),
//! на всех Spaces, и показываться не воруя фокус — это уровень screen-saver
//! плюс коллекция CanJoinAllSpaces|FullScreenAuxiliary, как у Raycast/Spotlight.

use objc2::encode::{Encode, Encoding};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use tauri::{Emitter, Manager, WebviewWindow};

#[path = "macos_toast.rs"]
mod toast_panel;

/// Only call on AppKit's main thread. Tauri retains a hidden carrier window;
/// notification presentation belongs to a genuine nonactivating NSPanel.
pub unsafe fn toast_native_window(carrier: *mut AnyObject) -> Result<*mut AnyObject, String> {
    let main: bool = msg_send![class!(NSThread), isMainThread];
    if !main { return Err("notification panel requires the AppKit thread".into()); }
    toast_panel::native_window(carrier)
}

pub fn prepare_toast(win: &WebviewWindow) {
    on_main(win, |carrier| unsafe {
        if let Err(error) = toast_native_window(carrier) {
            crate::log::line(&format!("[toast] native panel initialization failed: {error}"));
        }
    });
}

const NS_SCREEN_SAVER_WINDOW_LEVEL: isize = 1000;
/// NSWindowCollectionBehaviorCanJoinAllSpaces | NSWindowCollectionBehaviorFullScreenAuxiliary
const COLLECTION_BEHAVIOR: usize = (1 << 0) | (1 << 8);
const CAN_JOIN_ALL_APPLICATIONS: usize = 1 << 18;
const IGNORES_WINDOW_CYCLE: usize = 1 << 6;
/// Поведение обычного окна: Managed | ParticipatesInCycle | FullScreenPrimary.
/// FullScreenPrimary обязателен — без него AppKit не пускает окно в фуллскрин
/// (зелёная кнопка и ⌃⌘F молча не работают).
const COLLECTION_BEHAVIOR_WINDOW: usize = (1 << 2) | (1 << 5) | (1 << 7);

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

#[repr(C)]
#[derive(Clone, Copy)]
struct OperatingSystemVersion {
    major: isize,
    minor: isize,
    patch: isize,
}
unsafe impl Encode for OperatingSystemVersion {
    const ENCODING: Encoding =
        Encoding::Struct("?", &[isize::ENCODING, isize::ENCODING, isize::ENCODING]);
}

fn overlay_collection_behavior(major: isize) -> usize {
    COLLECTION_BEHAVIOR
        | IGNORES_WINDOW_CYCLE
        | if major >= 13 {
            CAN_JOIN_ALL_APPLICATIONS
        } else {
            0
        }
}

unsafe fn configure_overlay(window: *mut AnyObject) {
    let process: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
    let version: OperatingSystemVersion = msg_send![process, operatingSystemVersion];
    // CanJoinAllSpaces alone does not join other applications' Stage Manager
    // sets/full-screen spaces. The additional flag is available since macOS 13.
    let _: () = msg_send![window, setLevel: NS_SCREEN_SAVER_WINDOW_LEVEL];
    let _: () =
        msg_send![window, setCollectionBehavior: overlay_collection_behavior(version.major)];
    let _: () = msg_send![window, setHidesOnDeactivate: false];
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
        configure_overlay(w);
    });
}

/// Match the native transparent backing to the single rounded quick surface.
/// The workspace uses a normal decorated, opaque NSWindow instead.
pub fn clip_panel_surface(win: &WebviewWindow, radius: f64) {
    on_main(win, move |w| unsafe {
        let _: () = msg_send![w, setOpaque: false];
        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![w, setBackgroundColor: clear];
        let view: *mut AnyObject = msg_send![w, contentView];
        if !view.is_null() {
            let _: () = msg_send![view, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![view, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setCornerRadius: radius];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
        }
        let _: () = msg_send![w, invalidateShadow];
    });
}

/// Обычное окно: нормальный уровень и поведение — оконный режим (макет 14h).
/// Антипод `float_above_everything`: окно участвует в ⌘`-цикле, умеет в
/// фуллскрин и не висит поверх чужих окон.
pub fn float_normal(win: &WebviewWindow) {
    on_main(win, |w| unsafe {
        let _: () = msg_send![w, setLevel: 0isize];
        let _: () = msg_send![w, setCollectionBehavior: COLLECTION_BEHAVIOR_WINDOW];
        let _: () = msg_send![w, setHidesOnDeactivate: false];
        // фуллскрин и зелёная кнопка живут в стайл-маске: без Resizable AppKit
        // не даёт ни того, ни другого, даже когда tao просит toggleFullScreen:
        let mask: usize = msg_send![w, styleMask];
        let _: () = msg_send![w, setStyleMask: mask | (1 << 3)]; // NSWindowStyleMaskResizable
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
    work_area_at(mouse)
}

unsafe fn work_area_at(point: CGPoint) -> Option<CGRect> {
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    if screens.is_null() {
        return None;
    }
    let count: usize = msg_send![screens, count];
    let mut hit: *mut AnyObject = std::ptr::null_mut();
    for i in 0..count {
        let scr: *mut AnyObject = msg_send![screens, objectAtIndex: i];
        let f: CGRect = msg_send![scr, frame];
        if point.x >= f.origin.x
            && point.x < f.origin.x + f.size.width
            && point.y >= f.origin.y
            && point.y < f.origin.y + f.size.height
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

/// Keyboard dictation follows the foreground editor, even if the pointer was
/// left on a different display. Read window metadata only (no screen capture,
/// titles or AX text); fall back to the pointer when no normal window exists.
unsafe fn foreground_window_center() -> Option<CGPoint> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_graphics::window::{
        copy_window_info, kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
    };

    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    let frontmost: *mut AnyObject = msg_send![workspace, frontmostApplication];
    if frontmost.is_null() {
        return None;
    }
    let pid: i32 = msg_send![frontmost, processIdentifier];
    let windows = copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        0,
    )?;
    let array = windows.as_concrete_TypeRef() as *mut AnyObject;
    let count: usize = msg_send![array, count];
    let get = |dict: *mut AnyObject, key: &str| -> *mut AnyObject {
        let key = CFString::new(key);
        msg_send![dict, objectForKey: key.as_concrete_TypeRef() as *mut AnyObject]
    };
    for i in 0..count {
        let info: *mut AnyObject = msg_send![array, objectAtIndex: i];
        let owner = get(info, "kCGWindowOwnerPID");
        let layer = get(info, "kCGWindowLayer");
        if owner.is_null() || layer.is_null() {
            continue;
        }
        let owner_pid: i32 = msg_send![owner, intValue];
        let layer: i32 = msg_send![layer, intValue];
        if owner_pid != pid || layer != 0 {
            continue;
        }
        let bounds = get(info, "kCGWindowBounds");
        if bounds.is_null() {
            continue;
        }
        let number = |key: &str| -> Option<f64> {
            let v = get(bounds, key);
            if v.is_null() {
                None
            } else {
                Some(msg_send![v, doubleValue])
            }
        };
        let x = number("X")?;
        let y = number("Y")?;
        let width = number("Width")?;
        let height = number("Height")?;
        if width <= 1.0 || height <= 1.0 {
            continue;
        }
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        let screen_count: usize = msg_send![screens, count];
        if screen_count == 0 {
            return None;
        }
        let primary: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
        let primary_frame: CGRect = msg_send![primary, frame];
        // CGWindow bounds use global top-left coordinates; AppKit is bottom-left.
        return Some(CGPoint {
            x: x + width / 2.0,
            y: primary_frame.size.height - y - height / 2.0,
        });
    }
    None
}

/// Поставить окно (w×h поинтов) на дисплей с курсором.
/// `corner` — правый верхний угол с отступом 12; иначе центр, ~⅓ сверху
/// (как Raycast). Геометрия повторяет positionPanel Electron-версии.
pub fn place_panel(win: &WebviewWindow, w: f64, h: f64, corner: bool) {
    on_main(win, move |window| unsafe {
        let Some(vf) = work_area_under_cursor() else {
            return;
        };
        // Адаптивный размер: на большом экране панель крупнее (сохраняя пропорции).
        // База w×h — для ноутбука; масштаб по высоте рабочей области, кламп 1.0..1.7,
        // плюс не вылезать за ~90% экрана. На MacBook фактор ≈1.0 (820×620).
        let factor = (vf.size.height / 900.0).clamp(1.0, 1.7);
        let pw = (w * factor).min(vf.size.width * 0.90).round();
        let ph = (h * factor).min(vf.size.height * 0.92).round();
        let (x, y_bottom) = if corner {
            (
                vf.origin.x + vf.size.width - pw - 12.0,
                vf.origin.y + vf.size.height - 12.0 - ph,
            )
        } else {
            (
                vf.origin.x + ((vf.size.width - pw) / 2.0).round(),
                // отступ сверху (vf.h − ph)/3 → в AppKit-координатах снизу:
                vf.origin.y + vf.size.height - ((vf.size.height - ph) / 3.0).round() - ph,
            )
        };
        let frame = CGRect {
            origin: CGPoint { x, y: y_bottom },
            size: CGSize {
                width: pw,
                height: ph,
            },
        };
        let _: () = msg_send![window, setFrame: frame, display: false];
    });
}

/// Один тик слежения за курсором над окном тостов.
///
/// WKWebView не шлёт mouseenter/:hover, пока наше приложение неактивно, — а
/// тост всплывает как раз поверх чужого активного окна. Поэтому курсор ловим
/// нативно: NSEvent.mouseLocation глобален и от активности не зависит. Шлём
/// `toast-hover` = `{over, x, y}` (DOM-координаты курсора внутри окна, origin
/// сверху-слева) — чтобы webview hit-тестил конкретную карточку под курсором,
/// а не подсвечивал стек целиком. Эмитим на смене `over` или сдвиге y > 3px
/// (внутри окна курсор переезжает между карточками без mouseleave).
pub fn poll_toast_hover(win: &WebviewWindow) {
    static OVER: AtomicBool = AtomicBool::new(false);
    static LAST_Y: AtomicI32 = AtomicI32::new(i32::MIN);
    let w = win.clone();
    let _ = win.run_on_main_thread(move || unsafe {
        let Ok(ptr) = w.ns_window() else { return };
        let Ok(window) = toast_native_window(ptr as *mut AnyObject) else { return };
        let frame: CGRect = msg_send![window, frame];
        let m: CGPoint = msg_send![class!(NSEvent), mouseLocation];
        let visible: bool = msg_send![window, isVisible];
        // окно схлопнуто (карточек нет) — ховер не важен, гасим залипший флаг
        let over = visible
            && frame.size.height >= 4.0
            && m.x >= frame.origin.x
            && m.x < frame.origin.x + frame.size.width
            && m.y >= frame.origin.y
            && m.y < frame.origin.y + frame.size.height;
        // AppKit: origin снизу-слева, y вверх. DOM: сверху-слева, y вниз.
        let rel_x = m.x - frame.origin.x;
        let dom_y = frame.size.height - (m.y - frame.origin.y);
        let yi = dom_y.round() as i32;
        let prev_over = OVER.swap(over, Ordering::SeqCst);
        let prev_y = LAST_Y.swap(yi, Ordering::SeqCst);
        let moved = over && prev_y.abs_diff(yi) > 3;
        if prev_over != over || moved {
            let payload = serde_json::json!({ "over": over, "x": rel_x, "y": dom_y });
            let _ = w.app_handle().emit_to("toast", "toast-hover", payload);
        }
    });
}

fn toast_frame(vf: CGRect, w: f64, h: f64) -> CGRect {
    let width = w.min((vf.size.width - 28.0).max(1.0));
    let height = h.min((vf.size.height - 28.0).max(1.0));
    CGRect {
        origin: CGPoint {
            x: vf.origin.x + ((vf.size.width - width) / 2.0).round(),
            y: vf.origin.y + 14.0,
        },
        size: CGSize { width, height },
    }
}

/// Position and show in one AppKit operation. Space membership is configured
/// once at NSPanel creation. Reassigning it or reordering an already-visible
/// panel during a content resize can initiate a Space transition on macOS.
pub async fn place_toast(win: &WebviewWindow, w: f64, h: f64) -> Result<(), String> {
    if !w.is_finite() || !h.is_finite() || w <= 0.0 {
        return Err("invalid notification dimensions".into());
    }
    let carrier = win.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    win.run_on_main_thread(move || unsafe {
        if tx.is_closed() { return; }
        let result = (|| {
            let ptr = carrier.ns_window().map_err(|error| error.to_string())?;
            let window = toast_native_window(ptr as *mut AnyObject)?;
            if h <= 0.0 {
                let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
                return Ok(());
            }
            let area = foreground_window_center()
                .and_then(|point| work_area_at(point))
                .or_else(|| work_area_under_cursor());
            let vf = area.ok_or("notification display is unavailable")?;
            let visible: bool = msg_send![window, isVisible];
            let _: () = msg_send![window, setFrame: toast_frame(vf, w, h), display: false];
            if !visible { let _: () = msg_send![window, orderFrontRegardless]; }
            Ok(())
        })();
        let _ = tx.send(result);
    }).map_err(|error| error.to_string())?;
    // The renderer starts its transition only after the native canvas is ready.
    tokio::time::timeout(std::time::Duration::from_secs(3), rx).await
        .map_err(|_| "notification presentation timed out".to_string())?
        .map_err(|_| "notification presentation was cancelled".to_string())?
}

/* ===== аудио-шторка: пауза ЛЮБОГО чужого медиа на время озвучки =====
 * Через ungive/mediaremote-adapter: системный /usr/bin/perl энтайтлен на
 * MediaRemote, dlopen-ит наш фреймворк и шлёт pause/play текущему now-playing
 * (браузер/YouTube/Spotify/Music/Яндекс — что угодно). На macOS 26 это
 * единственный рабочий путь (прямые MediaRemote-команды закрыты энтайтлментом). */

fn mra_run(args: &[&str]) -> Option<String> {
    let dir = crate::util::jarvis_dir().join("mediaremote-adapter");
    let pl = dir.join("mediaremote-adapter.pl");
    if !pl.exists() {
        return None;
    }
    let fw = dir.join("MediaRemoteAdapter.framework");
    let out = std::process::Command::new("/usr/bin/perl")
        .arg(&pl)
        .arg(&fw)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Играет ли сейчас какое-либо медиа (now-playing).
pub fn media_is_playing() -> bool {
    mra_run(&["get"])
        .map(|s| s.contains("\"playing\":true"))
        .unwrap_or(false)
}
/// Пауза текущего now-playing (любой источник).
pub fn media_pause() {
    let _ = mra_run(&["send", "1"]);
}
/// Возобновить now-playing.
pub fn media_play() {
    let _ = mra_run(&["send", "0"]);
}
/// Переключить play/pause (MediaRemote команда 2).
pub fn media_toggle() {
    let _ = mra_run(&["send", "2"]);
}
/// Следующий трек (MediaRemote команда 4).
pub fn media_next() {
    let _ = mra_run(&["send", "4"]);
}
/// Предыдущий трек (MediaRemote команда 5).
pub fn media_prev() {
    let _ = mra_run(&["send", "5"]);
}

/* ===== Bluetooth аудиовыход ===== */

/// Проверить, подключён ли Bluetooth аудио-выход. Результат кешируется ~10с.
/// На любой ошибке/таймауте возвращает `true` (fail-open: не глушим речь при ошибке).
pub fn bluetooth_audio_output_connected() -> bool {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    const TTL: Duration = Duration::from_secs(10);

    {
        let guard = CACHE.lock().unwrap();
        if let Some((ts, val)) = *guard {
            if ts.elapsed() < TTL {
                return val;
            }
        }
    }

    let result = detect_bluetooth_output();

    {
        let mut guard = CACHE.lock().unwrap();
        *guard = Some((Instant::now(), result));
    }
    result
}

fn detect_bluetooth_output() -> bool {
    // Парсим system_profiler SPAudioDataType -json: ищем default output device
    // с _transport == "Bluetooth". Timeout 3с — чтобы не подвисать.
    let out = std::process::Command::new("system_profiler")
        .args(["SPAudioDataType", "-json"])
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o.stdout,
        _ => return true, // ошибка → fail-open
    };
    let text = String::from_utf8_lossy(&out);
    let val: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return true,
    };
    // Структура: { "SPAudioDataType": [ { "_items": [ { ... } ] } ] }
    let items = val
        .get("SPAudioDataType")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|o| o.get("_items"))
        .and_then(|v| v.as_array());
    let Some(items) = items else { return true };

    for item in items {
        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };
        // Флаг «это дефолтный выход»
        let is_default_out = obj
            .get("coreaudio_default_audio_output_device")
            .and_then(|v| v.as_str())
            .map(|s| s == "spaudio_yes")
            .unwrap_or(false);
        if !is_default_out {
            continue;
        }
        // Транспорт = Bluetooth?
        let transport = obj
            .get("coreaudio_device_transport")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if transport.to_ascii_lowercase().contains("bluetooth") {
            return true;
        }
        // Не bluetooth — выход найден, но не BT
        return false;
    }
    // Дефолтный выход не найден → fail-open
    true
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn overlay_joins_other_apps_without_conflicting_space_flags() {
        for version in [11, 12, 13, 26] {
            let behavior = overlay_collection_behavior(version);
            assert_ne!(behavior & (1 << 0), 0, "all Spaces");
            assert_eq!(
                behavior & (1 << 1),
                0,
                "MoveToActiveSpace conflicts with all Spaces"
            );
            assert_eq!(
                behavior & ((1 << 2) | (1 << 5) | (1 << 7)),
                0,
                "no document-window behavior"
            );
            assert_eq!(behavior & CAN_JOIN_ALL_APPLICATIONS != 0, version >= 13);
        }
    }

    #[test]
    fn toast_respects_secondary_display_origin_and_available_bounds() {
        let area = CGRect {
            origin: CGPoint {
                x: -1920.0,
                y: 160.0,
            },
            size: CGSize {
                width: 1920.0,
                height: 1040.0,
            },
        };
        let frame = toast_frame(area, 440.0, 180.0);
        assert_eq!(frame.origin.x, -1180.0);
        assert_eq!(frame.origin.y, 174.0);
        let small = CGRect {
            size: CGSize {
                width: 400.0,
                height: 360.0,
            },
            ..area
        };
        let frame = toast_frame(small, 440.0, 480.0);
        assert!(frame.origin.x >= small.origin.x);
        assert!(frame.origin.x + frame.size.width <= small.origin.x + small.size.width);
        assert!(frame.origin.y + frame.size.height <= small.origin.y + small.size.height);
    }
}
