//! Вставка текста в активное приложение через синтез ⌘V.
//!
//! Алгоритм:
//!   1. Снапшот буфера обмена.
//!   2. Записать `text` в буфер обмена.
//!   3. Синтезировать ⌘V через CGEvent (keyDown + keyUp).
//!   4. Восстановить исходный снапшот.
//!
//! Вставка требует разрешения Accessibility (в подписанном .app).
//! В тестах CGEvent-вызовы не отправляются — они вырезаны через #[cfg(not(test))].

/// Виртуальный кейкод 'V' (kVK_ANSI_V = 9). Нужен только синтезу CGEvent на
/// macOS: на Linux нажатие шлёт xdotool/wtype по имени клавиши, а не по коду.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn paste_keycode() -> u16 {
    9
}

/// Скопировать `text` в буфер обмена и ОСТАВИТЬ его там (в отличие от
/// `insert_text`, который восстанавливает прежний буфер). Нужно, чтобы результат
/// диктовки можно было вставить ещё раз вручную. Пустая строка → no-op.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    #[cfg(not(test))]
    {
        let mut cb = arboard::Clipboard::new().map_err(|e| format!("[copy] clipboard new: {e}"))?;
        cb.set_text(text)
            .map_err(|e| format!("[copy] clipboard set: {e}"))?;
    }
    Ok(())
}

/// Исход вставки: подтверждена ли она наблюдением за сфокусированным полем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertVerdict {
    /// Фокус был на редактируемом элементе и его значение изменилось,
    /// включая хвост вставленного текста, — «точно вставилось».
    Confirmed,
    /// Вставка отправлена (⌘V ушёл), но подтвердить не вышло: нет фокуса на
    /// поле ввода / AX недоступен / значение не читается. НЕ ошибка.
    Unconfirmed,
}

/// Вставить `text` в активное приложение через ⌘V.
///
/// `app` — для AX-снимков с главного потока (клиентский AX не потокобезопасен);
/// None (тесты/нет хэндла) → вердикт всегда Unconfirmed.
/// Пустая строка → Ok(Unconfirmed) без операций.
/// Ошибки буфера обмена или CGEvent → Err(String); не паникует.
pub fn insert_text(text: &str, app: Option<&tauri::AppHandle>) -> Result<InsertVerdict, String> {
    if text.is_empty() {
        return Ok(InsertVerdict::Unconfirmed);
    }

    // ── 0. Снимок сфокусированного элемента ДО вставки (best-effort AX) ─────
    let snap = || app.and_then(super::ax::focus_snapshot_main);
    let focus_before = snap();

    // ── 1. Снапшот буфера обмена ────────────────────────────────────────────
    let _snapshot = {
        #[cfg(not(test))]
        {
            let mut cb =
                arboard::Clipboard::new().map_err(|e| format!("[insert] clipboard new: {e}"))?;
            cb.get_text().ok() // None = буфер пуст или не текст — это нормально
        }
        #[cfg(test)]
        {
            // В тестах реальный буфер обмена не трогаем.
            None::<String>
        }
    };

    // ── 2. Записать text в буфер обмена ─────────────────────────────────────
    #[cfg(not(test))]
    {
        let mut cb =
            arboard::Clipboard::new().map_err(|e| format!("[insert] clipboard new: {e}"))?;
        cb.set_text(text)
            .map_err(|e| format!("[insert] clipboard set: {e}"))?;
    }

    // ── 3. Синтезировать «вставить» ─────────────────────────────────────────
    #[cfg(not(test))]
    {
        // Небольшая пауза — дать приложению время принять фокус после записи
        // буфера обмена. 60 мс — эмпирически достаточно для большинства приложений.
        std::thread::sleep(std::time::Duration::from_millis(60));
        synth_paste()?;
        // Пауза после вставки — дать приложению время переработать событие
        // до восстановления буфера обмена. 120 мс — практический минимум.
        std::thread::sleep(std::time::Duration::from_millis(120));
    }

    // ── 4. Восстановить буфер обмена ────────────────────────────────────────
    #[cfg(not(test))]
    {
        let mut cb = arboard::Clipboard::new()
            .map_err(|e| format!("[insert] clipboard restore new: {e}"))?;
        match _snapshot {
            Some(prev) => {
                // Ошибка при восстановлении — не фатальна: текст уже вставлен.
                if let Err(e) = cb.set_text(prev) {
                    crate::log::line(&format!("[insert] clipboard restore: {e}"));
                }
            }
            None => {
                // Буфер был пуст; не можем «очистить» arboard-ом надёжно,
                // оставляем вставленный текст в буфере — допустимый трейд-офф.
            }
        }
    }

    // ── 5. Подтверждение вставки: тот же элемент, значение изменилось ───────
    // (после пауз из шага 3 приложение уже переварило ⌘V)
    let focus_after = snap();
    // диагностика без конф. данных: роль/editable/длина значения, не текст
    let dbg = |f: &Option<super::ax::FocusSnapshot>| match f {
        Some(s) => format!(
            "role={} editable={} value_len={:?}",
            s.role,
            s.editable,
            s.value.as_ref().map(|v| v.chars().count())
        ),
        None => "нет".to_string(),
    };
    crate::log::line(&format!(
        "[insert] AX до: {} · после: {}",
        dbg(&focus_before),
        dbg(&focus_after)
    ));
    let confirmed = match (&focus_before, focus_after) {
        (Some(before), Some(after)) if before.editable => {
            super::ax::value_confirms_insert(&before.value, &after.value, text)
        }
        _ => false,
    };
    Ok(if confirmed {
        InsertVerdict::Confirmed
    } else {
        InsertVerdict::Unconfirmed
    })
}

/* ================= синтез нажатия «вставить» ================= */

/// macOS: CGEvent с флагом Command — работает в любом приложении, но требует
/// разрешения «Мониторинг ввода»/Accessibility.
#[cfg(all(target_os = "macos", not(test)))]
fn synth_paste() -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "[insert] CGEventSource::new failed".to_string())?;
    let keycode = paste_keycode();

    let down = CGEvent::new_keyboard_event(source.clone(), keycode, true)
        .map_err(|_| "[insert] CGEvent keydown failed".to_string())?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);

    let up = CGEvent::new_keyboard_event(source, keycode, false)
        .map_err(|_| "[insert] CGEvent keyup failed".to_string())?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    up.post(CGEventTapLocation::HID);
    Ok(())
}

/// Linux: единого системного API синтеза ввода нет — под X11 это XTEST
/// (`xdotool`), под Wayland ввод изолирован и нужен `wtype` с поддержкой
/// протокола со стороны композитора. Пробуем оба; если ни одного нет, честно
/// говорим об этом — текст уже лежит в буфере обмена, и юзер вставит сам.
#[cfg(all(not(target_os = "macos"), not(test)))]
fn synth_paste() -> Result<(), String> {
    use std::process::{Command, Stdio};

    let wayland = std::env::var("WAYLAND_DISPLAY").is_ok_and(|v| !v.is_empty());
    // порядок по сессии: на Wayland xdotool бесполезен, на X11 — наоборот
    let candidates: [(&str, &[&str]); 2] = if wayland {
        [("wtype", &["-M", "ctrl", "v", "-m", "ctrl"]), ("xdotool", &["key", "--clearmodifiers", "ctrl+v"])]
    } else {
        [("xdotool", &["key", "--clearmodifiers", "ctrl+v"]), ("wtype", &["-M", "ctrl", "v", "-m", "ctrl"])]
    };

    for (bin, args) in candidates {
        let ok = Command::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Ok(());
        }
    }
    Err("[insert] нечем синтезировать Ctrl+V: поставь xdotool (X11) или wtype (Wayland) — \
         текст уже в буфере обмена"
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // paste_keycode() = 9 (kVK_ANSI_V)
    #[test]
    fn paste_keycode_is_v() {
        assert_eq!(paste_keycode(), 9);
    }

    // insert_text("") — пустая строка: нет операций, возвращает Ok(Unconfirmed)
    #[test]
    fn empty_text_is_noop() {
        // Не должен трогать буфер обмена и не должен паниковать.
        assert_eq!(insert_text("", None), Ok(InsertVerdict::Unconfirmed));
    }

    // insert_text с непустой строкой в тест-режиме (без реального CGEvent/clipboard):
    // должен вернуть Ok (все #[cfg(not(test))] пути вырезаны); без AppHandle
    // подтверждения быть не может → Unconfirmed.
    #[test]
    fn nonempty_text_returns_ok_in_test_mode() {
        assert_eq!(
            insert_text("привет мир", None),
            Ok(InsertVerdict::Unconfirmed)
        );
    }

    #[test]
    fn copy_to_clipboard_empty_is_noop() {
        assert!(copy_to_clipboard("").is_ok());
    }

    #[test]
    fn copy_to_clipboard_nonempty_ok_in_test_mode() {
        assert!(copy_to_clipboard("надиктованный текст").is_ok());
    }
}
