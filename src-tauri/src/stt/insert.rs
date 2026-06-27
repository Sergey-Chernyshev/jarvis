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

/// Виртуальный кейкод 'V' (kVK_ANSI_V = 9).
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
        let mut cb =
            arboard::Clipboard::new().map_err(|e| format!("[copy] clipboard new: {e}"))?;
        cb.set_text(text).map_err(|e| format!("[copy] clipboard set: {e}"))?;
    }
    Ok(())
}

/// Вставить `text` в активное приложение через ⌘V.
///
/// Пустая строка → Ok(()) без операций.
/// Ошибки буфера обмена или CGEvent → Err(String); не паникует.
pub fn insert_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }

    // ── 1. Снапшот буфера обмена ────────────────────────────────────────────
    let snapshot = {
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
        cb.set_text(text).map_err(|e| format!("[insert] clipboard set: {e}"))?;
    }

    // ── 3. Синтезировать ⌘V ─────────────────────────────────────────────────
    #[cfg(not(test))]
    {
        use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        // Небольшая пауза — дать приложению время принять фокус после записи
        // буфера обмена. 60 мс — эмпирически достаточно для большинства приложений.
        std::thread::sleep(std::time::Duration::from_millis(60));

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

        // Пауза после вставки — дать приложению время переработать событие
        // до восстановления буфера обмена. 120 мс — практический минимум.
        std::thread::sleep(std::time::Duration::from_millis(120));
    }

    // ── 4. Восстановить буфер обмена ────────────────────────────────────────
    #[cfg(not(test))]
    {
        let mut cb =
            arboard::Clipboard::new().map_err(|e| format!("[insert] clipboard restore new: {e}"))?;
        match snapshot {
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // paste_keycode() = 9 (kVK_ANSI_V)
    #[test]
    fn paste_keycode_is_v() {
        assert_eq!(paste_keycode(), 9);
    }

    // insert_text("") — пустая строка: нет операций, возвращает Ok
    #[test]
    fn empty_text_is_noop() {
        // Не должен трогать буфер обмена и не должен паниковать.
        assert!(insert_text("").is_ok());
    }

    // insert_text с непустой строкой в тест-режиме (без реального CGEvent/clipboard):
    // должен вернуть Ok (все #[cfg(not(test))] пути вырезаны).
    #[test]
    fn nonempty_text_returns_ok_in_test_mode() {
        assert!(insert_text("привет мир").is_ok());
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
