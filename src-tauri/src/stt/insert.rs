//! Вставка текста в активное приложение через синтез ⌘V.
//!
//! Алгоритм:
//!   1. Записать `text` в буфер и сохранить его для ручного восстановления.
//!   2. Проверить разрешение и исходное приложение ввода.
//!   3. Отправить один ⌘V в исходное приложение, не меняя фокус.
//!   4. Проверить изменение того же AX-элемента (best-effort).
//!
//! Вставка требует разрешения Accessibility (в подписанном .app).
//! В тестах CGEvent-вызовы не отправляются — они вырезаны через #[cfg(not(test))].

/// Виртуальный кейкод 'V' (kVK_ANSI_V = 9). Нужен только синтезу CGEvent на
/// macOS: на Linux нажатие шлёт xdotool/wtype по имени клавиши, а не по коду.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn paste_keycode() -> u16 {
    9
}

/// Скопировать `text` в буфер обмена и оставить его там. Нужно, чтобы результат
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

/// Delivery information for the HUD. `Confirmed` requires observing the same
/// editor field change; successfully posting a shortcut alone is not proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertOutcome {
    pub verdict: InsertVerdict,
    pub copied: bool,
    pub paste_sent: bool,
    pub error: Option<String>,
    pub permission_required: bool,
    pub cancelled: bool,
}

/// Cancellation belongs to one dictation. A stale HUD cannot cancel the next
/// attempt, and cancellation never clears the transcript or clipboard.
#[derive(Default)]
pub struct InsertionControl {
    current: std::sync::atomic::AtomicU64,
    cancelled: std::sync::atomic::AtomicU64,
}
impl InsertionControl {
    pub fn begin(&self) -> u64 {
        self.current
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            + 1
    }
    pub fn current(&self) -> u64 {
        self.current.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn cancel(&self, id: u64) -> bool {
        if id == 0 || self.current() != id {
            return false;
        }
        self.cancelled
            .store(id, std::sync::atomic::Ordering::Release);
        true
    }
    pub fn is_cancelled(&self, id: u64) -> bool {
        self.current() != id || self.cancelled.load(std::sync::atomic::Ordering::Acquire) == id
    }
}

/// The original app must still be foreground. Never reactivate it or send the
/// user's words into whichever unrelated app gained focus during transcription.
fn target_is_current(target: Option<i32>, current: Option<i32>) -> bool {
    matches!((target, current), (Some(a), Some(b)) if a > 0 && a == b)
}

#[derive(Clone, Default)]
pub struct InsertionTarget {
    pub process_id: Option<i32>,
    pub focus: Option<super::ax::FocusIdentity>,
}

impl InsertionTarget {
    pub fn capture(app: Option<&tauri::AppHandle>) -> Self {
        let process_id = app.and_then(super::ax::frontmost_pid_main);
        let focus = app.and_then(super::ax::focus_identity_main);
        Self { process_id, focus }
    }

    /// A known original element must still exist and be focused. If AX was
    /// unavailable from the beginning, allow the existing PID-only fallback.
    fn field_is_current(&self, current: Option<&super::ax::FocusIdentity>) -> bool {
        match &self.focus {
            Some(original) => {
                self.process_id == Some(original.process_id)
                    && current.is_some_and(|now| original.same_element(now))
            }
            None => true,
        }
    }
}

/// Copy once and retain the dictated text for delayed paste consumers and manual
/// recovery. Restoring after a fixed 120 ms races slow Electron applications.
/// The app and original AX element are captured before showing the voice HUD.
pub fn insert_text(
    text: &str,
    app: Option<&tauri::AppHandle>,
    target: &InsertionTarget,
) -> InsertOutcome {
    insert_text_cancellable(text, app, target, &|| false)
}

pub fn insert_text_cancellable(
    text: &str,
    app: Option<&tauri::AppHandle>,
    target: &InsertionTarget,
    cancelled: &dyn Fn() -> bool,
) -> InsertOutcome {
    let mut outcome = InsertOutcome {
        verdict: InsertVerdict::Unconfirmed,
        copied: false,
        paste_sent: false,
        error: None,
        permission_required: false,
        cancelled: false,
    };
    if text.is_empty() {
        return outcome;
    }
    // Two asynchronously finishing dictations must not interleave clipboard
    // writes and shortcuts. This guard never blocks the main/UI thread.
    static INSERTION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = INSERTION.lock().unwrap_or_else(|p| p.into_inner());
    if let Err(e) = copy_to_clipboard(text) {
        outcome.error = Some(e);
        return outcome;
    }
    outcome.copied = true;
    if cancelled() {
        outcome.cancelled = true;
        return outcome;
    }
    if !super::ax::input_permission_granted() {
        outcome.permission_required = true;
        outcome.error = Some("Для автоматической вставки нужен Универсальный доступ.".into());
        return outcome;
    }
    #[cfg(all(target_os = "macos", not(test)))]
    if !target_is_current(
        target.process_id,
        app.and_then(super::ax::frontmost_pid_main),
    ) {
        outcome.error = Some("Приложение ввода сменилось или недоступно. Текст скопирован — вернись в нужное поле и нажми ⌘V.".into());
        return outcome;
    }
    let snap = || app.and_then(super::ax::focus_snapshot_main);
    let focus_before = snap();
    // Recheck after AX (an unresponsive app can take a little time to answer).
    #[cfg(all(target_os = "macos", not(test)))]
    if !target_is_current(
        target.process_id,
        app.and_then(super::ax::frontmost_pid_main),
    ) {
        outcome.error =
            Some("Фокус ввода изменился. Текст скопирован — нажми ⌘V в нужном поле.".into());
        return outcome;
    }
    let current_field = app.and_then(super::ax::focus_identity_main);
    if !target.field_is_current(current_field.as_ref()) {
        outcome.error = Some(
            "Поле, вкладка или окно ввода изменилось. Текст скопирован — нажми ⌘V в нужном поле."
                .into(),
        );
        return outcome;
    }
    // AX equality is also marshalled onto main. Do not let an app switch while
    // it was answering redirect delivery after the previous process check.
    #[cfg(all(target_os = "macos", not(test)))]
    if !target_is_current(
        target.process_id,
        app.and_then(super::ax::frontmost_pid_main),
    ) {
        outcome.error =
            Some("Приложение ввода изменилось. Текст скопирован — нажми ⌘V в нужном поле.".into());
        return outcome;
    }
    if cancelled() {
        outcome.cancelled = true;
        return outcome;
    }
    #[cfg(not(test))]
    if let Err(e) = synth_paste(target.process_id) {
        outcome.error = Some(e);
        return outcome;
    }
    outcome.paste_sent = true;

    // Observe, never retry ⌘V: an unreadable AX value does not mean failure and
    // replaying would duplicate the text. The clipboard remains valid throughout.
    let attempts = if cfg!(test) { 1 } else { 5 };
    for _ in 0..attempts {
        #[cfg(not(test))]
        std::thread::sleep(std::time::Duration::from_millis(100));
        let focus_after = snap();
        if target.focus.is_some()
            && matches!((&focus_before, &focus_after), (Some(before), Some(after))
            if super::ax::snapshot_confirms_insert(before, after, text))
            && target.field_is_current(app.and_then(super::ax::focus_identity_main).as_ref())
        {
            outcome.verdict = InsertVerdict::Confirmed;
            break;
        }
        // AX confirmation cannot work without a usable initial field snapshot.
        if !focus_before.as_ref().is_some_and(|s| s.editable) {
            break;
        }
    }
    crate::log::line(&format!(
        "[insert] copied={} paste_sent={} confirmed={}",
        outcome.copied,
        outcome.paste_sent,
        outcome.verdict == InsertVerdict::Confirmed
    ));
    outcome
}

/* ================= синтез нажатия «вставить» ================= */

/// macOS: CGEvent с флагом Command — работает в любом приложении, но требует
/// разрешения «Мониторинг ввода»/Accessibility.
#[cfg(all(target_os = "macos", not(test)))]
fn synth_paste(target_pid: Option<i32>) -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| "[insert] CGEventSource::new failed".to_string())?;
    let keycode = paste_keycode();

    let down = CGEvent::new_keyboard_event(source.clone(), keycode, true)
        .map_err(|_| "[insert] CGEvent keydown failed".to_string())?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    // Address the app captured at hotkey-down so an intervening activation
    // cannot redirect the keyboard event to Jarvis or another application.
    if let Some(pid) = target_pid {
        down.post_to_pid(pid);
    } else {
        down.post(CGEventTapLocation::HID);
    }
    std::thread::sleep(std::time::Duration::from_millis(20));

    let up = CGEvent::new_keyboard_event(source, keycode, false)
        .map_err(|_| "[insert] CGEvent keyup failed".to_string())?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    if let Some(pid) = target_pid {
        up.post_to_pid(pid);
    } else {
        up.post(CGEventTapLocation::HID);
    }
    Ok(())
}

/// Linux: единого системного API синтеза ввода нет — под X11 это XTEST
/// (`xdotool`), под Wayland ввод изолирован и нужен `wtype` с поддержкой
/// протокола со стороны композитора. Пробуем оба; если ни одного нет, честно
/// говорим об этом — текст уже лежит в буфере обмена, и юзер вставит сам.
#[cfg(all(not(target_os = "macos"), not(test)))]
fn synth_paste(_target_pid: Option<i32>) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let wayland = std::env::var("WAYLAND_DISPLAY").is_ok_and(|v| !v.is_empty());
    // порядок по сессии: на Wayland xdotool бесполезен, на X11 — наоборот
    let candidates: [(&str, &[&str]); 2] = if wayland {
        [
            ("wtype", &["-M", "ctrl", "v", "-m", "ctrl"]),
            ("xdotool", &["key", "--clearmodifiers", "ctrl+v"]),
        ]
    } else {
        [
            ("xdotool", &["key", "--clearmodifiers", "ctrl+v"]),
            ("wtype", &["-M", "ctrl", "v", "-m", "ctrl"]),
        ]
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
    Err(
        "[insert] нечем синтезировать Ctrl+V: поставь xdotool (X11) или wtype (Wayland) — \
         текст уже в буфере обмена"
            .to_string(),
    )
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
        let outcome = insert_text("", None, &InsertionTarget::default());
        assert!(!outcome.copied);
        assert!(!outcome.paste_sent);
        assert_eq!(outcome.verdict, InsertVerdict::Unconfirmed);
    }

    // insert_text с непустой строкой в тест-режиме (без реального CGEvent/clipboard):
    // должен вернуть Ok (все #[cfg(not(test))] пути вырезаны); без AppHandle
    // подтверждения быть не может → Unconfirmed.
    #[test]
    fn nonempty_text_returns_ok_in_test_mode() {
        let outcome = insert_text("привет мир", None, &InsertionTarget::default());
        assert!(outcome.copied);
        assert!(outcome.paste_sent);
        assert_eq!(outcome.verdict, InsertVerdict::Unconfirmed);
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn paste_rejects_changed_or_unknown_target() {
        assert!(target_is_current(Some(42), Some(42)));
        assert!(!target_is_current(Some(42), Some(43)));
        assert!(!target_is_current(Some(42), None));
        assert!(!target_is_current(None, Some(42)));
        assert!(!target_is_current(None, None));
        assert!(!target_is_current(Some(0), Some(0)));
    }

    #[test]
    fn same_process_new_field_or_tab_is_not_the_original_target() {
        let original = super::super::ax::FocusIdentity {
            process_id: 42,
            element_id: 7,
        };
        let target = InsertionTarget {
            process_id: Some(42),
            focus: Some(original.clone()),
        };
        assert!(target.field_is_current(Some(&original)));
        let other_field = super::super::ax::FocusIdentity {
            process_id: 42,
            element_id: 8,
        };
        assert!(!target.field_is_current(Some(&other_field)));
        assert!(
            !target.field_is_current(None),
            "A formerly known field cannot disappear silently"
        );
        let other_app = super::super::ax::FocusIdentity {
            process_id: 43,
            element_id: 7,
        };
        assert!(!target.field_is_current(Some(&other_app)));
    }

    #[test]
    fn unavailable_original_ax_keeps_pid_fallback_without_claiming_confirmation() {
        let target = InsertionTarget {
            process_id: Some(42),
            focus: None,
        };
        assert!(target.field_is_current(None));
        let result = insert_text("текст", None, &target);
        assert!(result.copied);
        assert!(result.paste_sent);
        assert_eq!(result.verdict, InsertVerdict::Unconfirmed);
    }

    #[test]
    fn lost_original_field_recovers_clipboard_and_never_posts_paste() {
        let target = InsertionTarget {
            process_id: Some(42),
            focus: Some(super::super::ax::FocusIdentity {
                process_id: 42,
                element_id: 7,
            }),
        };
        let result = insert_text("восстановить текст", None, &target);
        assert!(result.copied);
        assert!(!result.paste_sent);
        assert_eq!(result.verdict, InsertVerdict::Unconfirmed);
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("Поле, вкладка или окно"));
    }

    #[test]
    fn app_switch_during_original_capture_cannot_validate_a_different_field() {
        let target = InsertionTarget {
            process_id: Some(42),
            focus: Some(super::super::ax::FocusIdentity {
                process_id: 43,
                element_id: 7,
            }),
        };
        assert!(!target.field_is_current(target.focus.as_ref()));
    }

    #[test]
    fn copy_to_clipboard_empty_is_noop() {
        assert!(copy_to_clipboard("").is_ok());
    }

    #[test]
    fn copy_to_clipboard_nonempty_ok_in_test_mode() {
        assert!(copy_to_clipboard("надиктованный текст").is_ok());
    }
    #[test]
    fn cancelled_attempt_copies_but_never_sends_a_paste_and_does_not_cancel_next() {
        let control = InsertionControl::default();
        let first = control.begin();
        assert!(control.cancel(first));
        let result = insert_text_cancellable(
            "Сохранить 12,50 RUB",
            None,
            &InsertionTarget::default(),
            &|| control.is_cancelled(first),
        );
        assert!(result.cancelled);
        assert!(result.copied);
        assert!(!result.paste_sent);
        let next = control.begin();
        assert!(!control.cancel(first));
        assert!(!control.is_cancelled(next));
        let result = insert_text_cancellable(
            "Следующая диктовка",
            None,
            &InsertionTarget::default(),
            &|| control.is_cancelled(next),
        );
        assert!(!result.cancelled);
        assert!(result.paste_sent);
    }
    #[test]
    fn cancellation_is_rechecked_after_target_inspection() {
        let calls = std::cell::Cell::new(0);
        let result = insert_text_cancellable(
            "Текст остаётся",
            None,
            &InsertionTarget::default(),
            &|| {
                calls.set(calls.get() + 1);
                calls.get() > 1
            },
        );
        assert!(result.copied);
        assert!(result.cancelled);
        assert!(!result.paste_sent);
    }
}
