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

const NS_SCREEN_SAVER_WINDOW_LEVEL: isize = 1000;
/// NSWindowCollectionBehaviorCanJoinAllSpaces | NSWindowCollectionBehaviorFullScreenAuxiliary
const COLLECTION_BEHAVIOR: usize = (1 << 0) | (1 << 8);
/// NSWindowCollectionBehaviorMoveToActiveSpace: окно переезжает на текущий
/// Space вместо переключения пользователя на «родной». НЕСОВМЕСТИМ с
/// CanJoinAllSpaces — вместе валят AppKit ассертом на старте.
const NS_COLLECTION_BEHAVIOR_MOVE_TO_ACTIVE_SPACE: usize = 1 << 1;
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
///
/// Симметрична `float_normal`: снимает и то, что та выставила. Resizable в
/// стайл-маске обязателен оконному режиму, но накладке он оставляет управляемое
/// Mission Control окно — а такое окно AppKit держит на том Space, где его
/// активировали, и над фуллскрином оно не встаёт. Поэтому маска возвращается
/// к накладочной, иначе после «окно → накладка» ⌘J показывал бы панель только
/// на исходном рабочем столе.
pub fn float_above_everything(win: &WebviewWindow) {
    on_main(win, |w| unsafe {
        let _: () = msg_send![w, setLevel: NS_SCREEN_SAVER_WINDOW_LEVEL];
        let _: () = msg_send![w, setCollectionBehavior: COLLECTION_BEHAVIOR];
        let _: () = msg_send![w, setHidesOnDeactivate: false];
        let mask: usize = msg_send![w, styleMask];
        let _: () = msg_send![w, setStyleMask: mask & !(1 << 3)]; // без Resizable
    });
}

/// Показывать окно на ТЕКУЩЕМ рабочем столе, а не утаскивать пользователя на
/// тот, где окно было создано. Для окон чата агента и онбординга: они живут на
/// своём Space, и `set_focus` заставлял macOS переключать Space целиком.
///
/// Только для окон БЕЗ `CanJoinAllSpaces`: эти биты взаимоисключающие, и вместе
/// роняют AppKit ассертом. Панели и тостам это не нужно — они и так на всех
/// Spaces (см. `float_above_everything`).
pub fn move_to_active_space(win: &WebviewWindow) {
    on_main(win, |w| unsafe {
        let _: () = msg_send![w, setCollectionBehavior: NS_COLLECTION_BEHAVIOR_MOVE_TO_ACTIVE_SPACE];
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
///
/// Space здесь НЕ трогаем: `MoveToActiveSpace` несовместим с
/// `CanJoinAllSpaces` (взаимоисключающие биты NSWindowCollectionBehavior) —
/// AppKit уходит в assert, и падение прилетает внутри did_finish_launching,
/// где его нельзя размотать. Проверено: приложение не стартует вовсе.
pub fn show_inactive(win: &WebviewWindow) {
    on_main(win, |w| unsafe {
        let _: () = msg_send![w, orderFrontRegardless];
        // Прозрачное окно: форму тени AppKit кэширует по непрозрачным пикселям
        // на момент отрисовки. Без сброса от прошлого размера остаётся контур —
        // это и есть «артефакты теней вокруг».
        let _: () = msg_send![w, invalidateShadow];
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
            size: CGSize { width: pw, height: ph },
        };
        let _: () = msg_send![window, setFrame: frame, display: false];
        // Панель тоже прозрачная и меняет размер под дисплей — без сброса тени
        // на экране остаётся контур от прежней геометрии.
        let _: () = msg_send![window, invalidateShadow];
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
        let window = ptr as *mut AnyObject;
        let frame: CGRect = msg_send![window, frame];
        let m: CGPoint = msg_send![class!(NSEvent), mouseLocation];
        // окно схлопнуто (карточек нет) — ховер не важен, гасим залипший флаг
        let over = frame.size.height >= 4.0
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
        let moved = over && (prev_y - yi).abs() > 3;
        if prev_over != over || moved {
            let payload = serde_json::json!({ "over": over, "x": rel_x, "y": dom_y });
            let _ = w.app_handle().emit_to("toast", "toast-hover", payload);
        }
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
        // Стек тостов меняет высоту на каждой карточке. У прозрачного окна тень
        // кэшируется по прежней форме, поэтому вокруг остаются контуры старого
        // размера — сбрасываем её после каждого изменения кадра.
        let _: () = msg_send![window, invalidateShadow];
    });
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
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Играет ли сейчас какое-либо медиа (now-playing).
pub fn media_is_playing() -> bool {
    mra_run(&["get"]).map(|s| s.contains("\"playing\":true")).unwrap_or(false)
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
