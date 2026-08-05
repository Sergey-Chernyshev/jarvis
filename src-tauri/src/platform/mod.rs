//! Платформенный слой: всё, что нельзя сделать кросс-платформенным API Tauri.
//!
//! Один и тот же набор функций реализован под каждую ОС, поэтому остальной код
//! зовёт `crate::platform::…` и ничего не знает про AppKit или X11:
//!
//! | функция                          | macOS                       | Linux                          |
//! | -------------------------------- | --------------------------- | ------------------------------ |
//! | `float_above_everything`         | NSWindow level + Spaces     | always-on-top + все рабочие столы |
//! | `float_normal`                   | обычный уровень окна        | снять always-on-top            |
//! | `show_inactive`                  | orderFrontRegardless        | обычный show (аналога нет)     |
//! | `place_panel` / `place_toast`    | экран под курсором          | основной монитор               |
//! | `poll_toast_hover`               | нативный опрос курсора      | не нужен: `:hover` работает    |
//! | `media_*`                        | MediaRemote через perl-адаптер | MPRIS через playerctl       |
//! | `bluetooth_audio_output_connected` | system_profiler           | pactl                          |
//!
//! Контракт у всех одинаковый; поведение деградирует мягко — если внешнего
//! инструмента нет, функция ничего не делает, а не роняет приложение.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(target_os = "macos"))]
mod linux;
#[cfg(not(target_os = "macos"))]
pub use linux::*;
