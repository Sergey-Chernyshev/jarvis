//! Linux-реализация платформенного слоя (см. `platform/mod.rs`).
//!
//! Принцип: берём кросс-платформенный API Tauri везде, где он есть, и внешние
//! утилиты там, где без них никак (медиа — `playerctl`, звук — `pactl`).
//! Утилиты необязательны: нет в системе — функция тихо ничего не делает.
//! Ронять из-за отсутствия playerctl приложение-монитор сессий нельзя.

use std::process::{Command, Stdio};
use tauri::{LogicalPosition, LogicalSize, WebviewWindow};

/* ================= окна ================= */

/// Накладка: поверх всего и на всех рабочих столах.
///
/// На X11 хватает штатного always-on-top — менеджер окон честно держит такое
/// окно сверху. На Wayland (Sway и прочие wlroots) обе просьбы клиенту НЕ
/// принадлежат: и «поверх всех», и «на всех столах» решает композитор. Зовём
/// их всё равно — там, где протокол это умеет, сработает, — а на Sway то же
/// самое делается правилом `floating enable, sticky enable` (docs/sway).
pub fn float_above_everything(win: &WebviewWindow) {
    let _ = win.set_always_on_top(true);
    let _ = win.set_visible_on_all_workspaces(true);
}

/// Обычное окно: снять «поверх всего» и вернуть на свой рабочий стол.
pub fn float_normal(win: &WebviewWindow) {
    let _ = win.set_always_on_top(false);
    let _ = win.set_visible_on_all_workspaces(false);
}

/// Показать, не забирая фокус.
///
/// Точного аналога `orderFrontRegardless` в X11/Wayland нет: политику фокуса
/// решает оконный менеджер, а не приложение. Показываем как есть — окно уже
/// always-on-top, так что видно его в любом случае.
pub fn show_inactive(win: &WebviewWindow) {
    let _ = win.show();
}

/// Логическая геометрия монитора, на котором показывать окно.
///
/// Берём монитор самого окна, иначе основной. На macOS панель уезжает на экран
/// под курсором, но там для этого есть глобальная позиция курсора в AppKit;
/// здесь надёжнее и предсказуемее держаться текущего/основного монитора.
fn target_monitor(win: &WebviewWindow) -> Option<(LogicalPosition<f64>, LogicalSize<f64>)> {
    let mon = match win.current_monitor() {
        Ok(Some(m)) => Some(m),
        _ => win.primary_monitor().ok().flatten(),
    }?;
    let scale = mon.scale_factor();
    let pos = mon.position().to_logical::<f64>(scale);
    let size = mon.size().to_logical::<f64>(scale);
    Some((pos, size))
}

/// Панель: по центру монитора с отступом сверху ~⅓ (как накладка Raycast),
/// либо в правом верхнем углу. Размер адаптируется к высоте экрана — та же
/// формула, что и в macOS-реализации, чтобы поведение совпадало.
///
/// На Wayland позицию клиент не выбирает — `set_position` там ничего не
/// делает, и панель встанет туда, куда решит композитор (у sway — по правилу
/// floating). Размер при этом уважается, поэтому считаем его в любом случае.
pub fn place_panel(win: &WebviewWindow, w: f64, h: f64, corner: bool) {
    let Some((origin, screen)) = target_monitor(win) else {
        return;
    };
    // запас под панель/док окружения — рабочую область Tauri не отдаёт
    let margin = 48.0;
    let avail_h = (screen.height - margin).max(240.0);

    let factor = (avail_h / 900.0).clamp(1.0, 1.7);
    let pw = (w * factor).min(screen.width * 0.90).round();
    let ph = (h * factor).min(avail_h * 0.92).round();

    let (x, y) = if corner {
        (origin.x + screen.width - pw - 12.0, origin.y + 12.0)
    } else {
        (
            origin.x + ((screen.width - pw) / 2.0).round(),
            origin.y + ((avail_h - ph) / 3.0).round(),
        )
    };

    let _ = win.set_size(LogicalSize::new(pw, ph));
    let _ = win.set_position(LogicalPosition::new(x, y));
}

/// Стек тостов: по центру снизу, отступ от края 14.
pub fn place_toast(win: &WebviewWindow, w: f64, h: f64) {
    let Some((origin, screen)) = target_monitor(win) else {
        return;
    };
    let margin = 48.0; // тот же запас под панель окружения
    let x = origin.x + ((screen.width - w) / 2.0).round();
    let y = origin.y + screen.height - margin - h - 14.0;
    let _ = win.set_position(LogicalPosition::new(x, y.max(origin.y)));
}

/// На macOS это обход WKWebView: он не шлёт `:hover`, пока приложение неактивно,
/// а тост всплывает поверх чужого окна. WebKitGTK такой проблемой не страдает —
/// CSS-ховер в `toast.html` работает сам, нативный опрос курсора не нужен.
pub fn poll_toast_hover(_win: &WebviewWindow) {}

/* ================= медиа: MPRIS через playerctl ================= */
/* Аналог MediaRemote: playerctl говорит по D-Bus с любым MPRIS-плеером —
 * браузер, Spotify, VLC, mpv. Нет playerctl — все команды — no-op, а
 * `media_is_playing` честно отвечает «не играет», и шторка просто не сработает. */

fn playerctl(args: &[&str]) -> Option<String> {
    let out = Command::new("playerctl")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Играет ли сейчас хоть один MPRIS-плеер.
pub fn media_is_playing() -> bool {
    // при нескольких плеерах playerctl отдаёт статус каждого построчно
    playerctl(&["-a", "status"])
        .map(|s| s.lines().any(|l| l.trim() == "Playing"))
        .unwrap_or(false)
}

/// Пауза всех играющих плееров.
pub fn media_pause() {
    let _ = playerctl(&["-a", "pause"]);
}

/// Возобновить воспроизведение.
pub fn media_play() {
    let _ = playerctl(&["play"]);
}

/// Переключить play/pause.
pub fn media_toggle() {
    let _ = playerctl(&["play-pause"]);
}

/// Следующий трек.
pub fn media_next() {
    let _ = playerctl(&["next"]);
}

/// Предыдущий трек.
pub fn media_prev() {
    let _ = playerctl(&["previous"]);
}

/* ================= аудиовыход ================= */

/// Подключён ли Bluetooth-выход. Как и на macOS — fail-open: при любой ошибке
/// отвечаем `true`, чтобы не заглушить речь из-за неудачной проверки.
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

/// PipeWire/PulseAudio: имя дефолтного sink'а у Bluetooth-устройств содержит
/// `bluez` (`bluez_output.XX_XX_…`). Нет pactl — fail-open.
fn detect_bluetooth_output() -> bool {
    let out = Command::new("pactl")
        .arg("get-default-sink")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let sink = String::from_utf8_lossy(&o.stdout).to_ascii_lowercase();
            sink.contains("bluez") || sink.contains("bluetooth")
        }
        _ => true, // pactl нет или ошибка → не глушим речь
    }
}
