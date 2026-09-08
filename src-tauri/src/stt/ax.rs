//! Best-effort проверка вставки диктовки через Accessibility API: был ли фокус
//! на редактируемом элементе и изменилось ли его значение после ⌘V.
//!
//! Всё строго best-effort: AX может быть недоступен (нет разрешения — хотя для
//! синтеза ⌘V оно то же самое), элемент может не отдавать AXValue (secure-поля,
//! кастомные редакторы) — любой сбой означает «вставка не подтверждена», это
//! НЕ ошибка. Никаких паник, никаких блокировок пайплайна диктовки.

#![allow(non_upper_case_globals)]

/// The original editor element is retained throughout a dictation. Equality is
/// checked with CFEqual on AppKit's thread; a CFHash is not a unique identity.
#[derive(Clone)]
pub struct FocusIdentity {
    pub process_id: i32,
    #[cfg(all(target_os = "macos", not(test)))]
    element: std::sync::Arc<RetainedFocus>,
    #[cfg(any(not(target_os = "macos"), test))]
    pub element_id: usize,
}

#[cfg(all(target_os = "macos", not(test)))]
struct RetainedFocus {
    // The address is only dereferenced on the main thread. This struct owns
    // the Copy-rule reference until its final Arc is dropped.
    address: usize,
    app: tauri::AppHandle,
}

#[cfg(all(target_os = "macos", not(test)))]
impl Drop for RetainedFocus {
    fn drop(&mut self) {
        let address = self.address;
        let _ = self.app.run_on_main_thread(move || unsafe {
            core_foundation::base::CFRelease(address as core_foundation::base::CFTypeRef);
        });
    }
}

impl FocusIdentity {
    pub fn same_element(&self, other: &Self) -> bool {
        if self.process_id <= 0 || self.process_id != other.process_id {
            return false;
        }
        #[cfg(all(target_os = "macos", not(test)))]
        {
            let before = self.element.clone();
            let after = other.element.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = self.element.app.run_on_main_thread(move || unsafe {
                use core_foundation::base::{CFEqual, CFTypeRef};
                // Keep both references alive until this comparison has run,
                // even if the waiting caller times out.
                let same = CFEqual(before.address as CFTypeRef, after.address as CFTypeRef) != 0;
                let _ = tx.send(same);
            });
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .unwrap_or(false)
        }
        #[cfg(any(not(target_os = "macos"), test))]
        {
            self.element_id == other.element_id
        }
    }
}

/// Read identity only: do not fetch editor contents or build a browser's entire
/// AX tree on hotkey-down. An unavailable AX element leaves the PID fallback.
pub fn focus_identity_main(app: &tauri::AppHandle) -> Option<FocusIdentity> {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let owner = app.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        app.run_on_main_thread(move || {
            let identity = ffi::focus_identity().map(|(process_id, address)| FocusIdentity {
                process_id,
                element: std::sync::Arc::new(RetainedFocus {
                    address,
                    app: owner,
                }),
            });
            let _ = tx.send(identity);
        })
        .ok()?;
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .ok()
            .flatten()
    }
    #[cfg(any(not(target_os = "macos"), test))]
    {
        let _ = app;
        None
    }
}

/// Снимок сфокусированного элемента до/после вставки.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusSnapshot {
    /// Process and AX element identity; a focus change cannot confirm a paste.
    pub process_id: i32,
    pub element_id: usize,
    /// AX-роль элемента (для диагностики в логе).
    pub role: String,
    /// Похож ли элемент на поле ввода (роль или settable AXValue).
    pub editable: bool,
    /// Текстовое значение элемента (None — не отдаёт/не строка).
    pub value: Option<String>,
}

/// Confirm only a change in the same field, never text in another application.
pub fn snapshot_confirms_insert(before: &FocusSnapshot, after: &FocusSnapshot, text: &str) -> bool {
    before.editable
        && before.process_id > 0
        && before.process_id == after.process_id
        && before.element_id == after.element_id
        && value_confirms_insert(&before.value, &after.value, text)
}

/// Capture the intended app before any voice HUD appears. NSWorkspace works
/// even when an Electron editor has not built its Accessibility tree yet.
pub fn frontmost_pid_main(app: &tauri::AppHandle) -> Option<i32> {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        app.run_on_main_thread(move || unsafe {
            use objc2::runtime::AnyObject;
            use objc2::{class, msg_send};
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let frontmost: *mut AnyObject = msg_send![workspace, frontmostApplication];
            let pid = if frontmost.is_null() {
                None
            } else {
                let pid: i32 = msg_send![frontmost, processIdentifier];
                (pid > 0).then_some(pid)
            };
            let _ = tx.send(pid);
        })
        .ok()?;
        rx.recv_timeout(std::time::Duration::from_millis(400))
            .ok()
            .flatten()
    }
    #[cfg(any(not(target_os = "macos"), test))]
    {
        let _ = app;
        None
    }
}

pub fn input_permission_granted() -> bool {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        ffi::input_permission_granted()
    }
    #[cfg(any(not(target_os = "macos"), test))]
    {
        true
    }
}

/// Подтверждает ли пара снимков, что `inserted` реально вставился:
/// значение изменилось И содержит хвост вставленного текста.
/// Чистая функция — покрыта юнитами.
pub fn value_confirms_insert(
    before: &Option<String>,
    after: &Option<String>,
    inserted: &str,
) -> bool {
    let Some(after) = after else { return false };
    if before.as_deref() == Some(after.as_str()) {
        return false; // значение не изменилось
    }
    // Нормализация: схлопнуть пробелы/переводы строк — приложения могут
    // переносить текст, менять NBSP и т.п.
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let after_n = norm(after);
    let ins_n = norm(inserted);
    if ins_n.is_empty() {
        return false;
    }
    // Сверяем по хвосту (≤ 80 символов): длинные тексты элемент может
    // показывать не целиком, но вставка идёт в позицию курсора — хвост виден.
    let tail: String = {
        let chars: Vec<char> = ins_n.chars().collect();
        let start = chars.len().saturating_sub(80);
        chars[start..].iter().collect()
    };
    let previous_occurrences = before
        .as_ref()
        .map(|s| norm(s).matches(&tail).count())
        .unwrap_or(0);
    after_n.matches(&tail).count() > previous_occurrences
}

// ── AX FFI (только вне тестов: в CI/юнитах системного AX нет) ───────────────

#[cfg(all(target_os = "macos", not(test)))]
mod ffi {
    use super::FocusSnapshot;
    use core_foundation::base::{CFHash, CFRelease, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};

    type AXUIElementRef = CFTypeRef;
    type AXError = i32;
    const K_AX_ERROR_SUCCESS: AXError = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementIsAttributeSettable(
            element: AXUIElementRef,
            attribute: CFStringRef,
            settable: *mut bool,
        ) -> AXError;
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
        fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout: f32) -> AXError;
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: CFTypeRef,
        ) -> AXError;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
    }

    pub fn input_permission_granted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    /// Скопировать строковый AX-атрибут элемента (None — нет/не строка).
    /// Ошибку атрибута пишем в лог (диагностика «почему не подтверждается»).
    unsafe fn copy_string_attr(el: AXUIElementRef, name: &str) -> Option<String> {
        let attr = CFString::new(name);
        let mut out: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(el, attr.as_concrete_TypeRef(), &mut out);
        if err != K_AX_ERROR_SUCCESS || out.is_null() {
            crate::log::line(&format!("[insert] AX атрибут {name}: err={err}"));
            return None;
        }
        if CFGetTypeID(out) != CFStringGetTypeID() {
            crate::log::line(&format!("[insert] AX атрибут {name}: не строка"));
            CFRelease(out);
            return None;
        }
        let s = CFString::wrap_under_create_rule(out as CFStringRef).to_string();
        Some(s)
    }

    /// Скопировать элемент-атрибут (AXFocusedApplication/AXFocusedUIElement).
    unsafe fn copy_element_attr(el: AXUIElementRef, name: &str) -> Option<AXUIElementRef> {
        let attr = CFString::new(name);
        let mut out: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(el, attr.as_concrete_TypeRef(), &mut out);
        if err != K_AX_ERROR_SUCCESS || out.is_null() {
            crate::log::line(&format!("[insert] AX элемент {name}: err={err}"));
            return None;
        }
        Some(out)
    }

    /// Returns an owned AX reference. The caller must release it on main.
    pub fn focus_identity() -> Option<(i32, usize)> {
        unsafe {
            if !AXIsProcessTrusted() {
                return None;
            }
            let sys = AXUIElementCreateSystemWide();
            if sys.is_null() {
                return None;
            }
            let _ = AXUIElementSetMessagingTimeout(sys, 0.04);
            let focused = match copy_element_attr(sys, "AXFocusedApplication") {
                Some(app) => {
                    let element = copy_element_attr(app, "AXFocusedUIElement");
                    CFRelease(app);
                    element
                }
                None => copy_element_attr(sys, "AXFocusedUIElement"),
            };
            CFRelease(sys);
            let focused = focused?;
            let mut pid = 0;
            let valid = AXUIElementGetPid(focused, &mut pid) == K_AX_ERROR_SUCCESS
                && pid > 0
                && copy_string_attr(focused, "AXRole").is_some();
            if valid {
                Some((pid, focused as usize))
            } else {
                CFRelease(focused);
                None
            }
        }
    }

    /// Снимок сфокусированного элемента системы. None — AX не отдал фокус.
    /// Двухходовка: фокус-приложение → его фокус-элемент (прямой запрос
    /// system-wide → элемент даёт invalid (-25202) на атрибутах в ряде систем).
    pub fn focus_snapshot() -> Option<FocusSnapshot> {
        unsafe {
            if !AXIsProcessTrusted() {
                crate::log::line("[insert] AX: процесс не доверен (нет Accessibility)");
                return None;
            }
            let sys = AXUIElementCreateSystemWide();
            if sys.is_null() {
                return None;
            }
            // A hung editor must not freeze Jarvis' main thread indefinitely.
            let _ = AXUIElementSetMessagingTimeout(sys, 0.10);
            // 1) сфокусированное приложение; 2) его фокус-элемент.
            // Fallback — прямой AXFocusedUIElement у system-wide.
            let (focused, app_title) = match copy_element_attr(sys, "AXFocusedApplication") {
                Some(app) => {
                    let title = copy_string_attr(app, "AXTitle");
                    let mut e = copy_element_attr(app, "AXFocusedUIElement");
                    // Chromium/Electron не строят AX-дерево, пока ассистивный клиент
                    // не попросит: элемент отдаётся, но атрибуты отвечают invalid
                    // (-25202). Явно включаем и перечитываем фокус.
                    let probe_dead = e
                        .map(|el| {
                            let dead = copy_string_attr(el, "AXRole").is_none();
                            if dead {
                                CFRelease(el);
                            }
                            dead
                        })
                        .unwrap_or(false);
                    if probe_dead {
                        let manual = CFString::new("AXManualAccessibility");
                        let yes = core_foundation::boolean::CFBoolean::true_value();
                        let serr = AXUIElementSetAttributeValue(
                            app,
                            manual.as_concrete_TypeRef(),
                            yes.as_concrete_TypeRef() as CFTypeRef,
                        );
                        crate::log::line(&format!(
                            "[insert] AX: включаю AXManualAccessibility для «{}» (err={serr})",
                            title.as_deref().unwrap_or("?")
                        ));
                        // Never sleep on the UI thread. A later confirmation
                        // snapshot retries once the editor has built its tree.
                        e = copy_element_attr(app, "AXFocusedUIElement");
                    }
                    CFRelease(app);
                    (e, title)
                }
                None => (copy_element_attr(sys, "AXFocusedUIElement"), None),
            };
            CFRelease(sys);
            let Some(focused) = focused else {
                return None;
            };

            let role = copy_string_attr(focused, "AXRole").unwrap_or_default();
            if role.is_empty() {
                crate::log::line(&format!(
                    "[insert] AX: приложение «{}» не отдаёт атрибуты фокуса",
                    app_title.as_deref().unwrap_or("?")
                ));
            }
            // классические поля ввода; всё остальное добираем через settable AXValue
            let editable_role = matches!(
                role.as_str(),
                "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField"
            );
            let mut settable = false;
            if !editable_role {
                let vattr = CFString::new("AXValue");
                let _ = AXUIElementIsAttributeSettable(
                    focused,
                    vattr.as_concrete_TypeRef(),
                    &mut settable,
                );
            }
            let value = copy_string_attr(focused, "AXValue");
            let mut process_id = 0;
            let _ = AXUIElementGetPid(focused, &mut process_id);
            let element_id = CFHash(focused) as usize;
            CFRelease(focused);
            Some(FocusSnapshot {
                process_id,
                element_id,
                role,
                editable: editable_role || settable,
                value,
            })
        }
    }
}

/// Снимок сфокусированного элемента (best-effort; в тестах всегда None).
pub fn focus_snapshot() -> Option<FocusSnapshot> {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        // AX-вызовы уходят в чужие процессы — защищаемся от любых сюрпризов.
        std::panic::catch_unwind(ffi::focus_snapshot).unwrap_or(None)
    }
    // Аналог AX на Linux — AT-SPI, но он даёт снимок фокуса только когда
    // приложение-цель само его отдаёт (GTK/Qt с включённой доступностью), а у
    // терминалов и Electron это не работает. Поэтому снимка нет: вставка
    // проверяется по факту, а не предсказанием. См. insert.rs.
    #[cfg(any(not(target_os = "macos"), test))]
    {
        None
    }
}

/// Снимок с ГЛАВНОГО потока: клиентский AX-API не потокобезопасен — с фонового
/// потока диктовки атрибуты элементов отвечают invalid (-25202). Ждём ответ
/// не дольше 400 мс (не подвешивать пайплайн диктовки, если main занят).
pub fn focus_snapshot_main(app: &tauri::AppHandle) -> Option<FocusSnapshot> {
    let (tx, rx) = std::sync::mpsc::channel();
    if app
        .run_on_main_thread(move || {
            let _ = tx.send(focus_snapshot());
        })
        .is_err()
    {
        return None;
    }
    match rx.recv_timeout(std::time::Duration::from_millis(400)) {
        Ok(snap) => snap,
        Err(_) => {
            crate::log::line("[insert] AX: main-поток не ответил за 400мс");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn confirms_when_value_grew_with_tail() {
        assert!(value_confirms_insert(&s("привет"), &s("привет мир"), "мир"));
        // пустое поле → появился текст
        assert!(value_confirms_insert(
            &None,
            &s("надиктовано"),
            "надиктовано"
        ));
    }

    #[test]
    fn rejects_unchanged_or_missing_value() {
        assert!(!value_confirms_insert(
            &s("такой же"),
            &s("такой же"),
            "текст"
        ));
        assert!(!value_confirms_insert(&s("что-то"), &None, "текст"));
        assert!(!value_confirms_insert(&None, &None, "текст"));
    }

    #[test]
    fn rejects_change_without_inserted_tail() {
        // значение изменилось, но вставленного там нет (например, элемент сам обновился)
        assert!(!value_confirms_insert(
            &s("до"),
            &s("после"),
            "надиктованный текст"
        ));
    }

    #[test]
    fn normalizes_whitespace_and_checks_tail_of_long_text() {
        let long = "слово ".repeat(50); // 300 символов
        let shown = format!(
            "начало поля {}",
            long.split_whitespace().collect::<Vec<_>>().join(" ")
        );
        assert!(value_confirms_insert(
            &s("начало поля"),
            &Some(shown),
            &long
        ));
        // перенос строк в приложении вместо пробелов — не мешает
        assert!(value_confirms_insert(
            &None,
            &s("привет\nбольшой\nмир"),
            "привет большой мир"
        ));
    }

    #[test]
    fn empty_inserted_never_confirms() {
        assert!(!value_confirms_insert(&None, &s("что-то"), "   "));
    }

    #[test]
    fn preexisting_text_does_not_confirm_an_unrelated_edit() {
        assert!(!value_confirms_insert(
            &s("диктовка"),
            &s("другой текст диктовка"),
            "диктовка"
        ));
        assert!(value_confirms_insert(
            &s("диктовка"),
            &s("диктовка диктовка"),
            "диктовка"
        ));
    }

    #[test]
    fn confirmation_requires_the_same_editable_field_and_process() {
        let before = FocusSnapshot {
            process_id: 42,
            element_id: 7,
            role: "AXTextArea".into(),
            editable: true,
            value: s("до"),
        };
        let after = FocusSnapshot {
            value: s("до текст"),
            ..before.clone()
        };
        assert!(snapshot_confirms_insert(&before, &after, "текст"));
        assert!(!snapshot_confirms_insert(
            &before,
            &FocusSnapshot {
                process_id: 43,
                ..after.clone()
            },
            "текст"
        ));
        assert!(!snapshot_confirms_insert(
            &before,
            &FocusSnapshot {
                element_id: 8,
                ..after.clone()
            },
            "текст"
        ));
        assert!(!snapshot_confirms_insert(
            &FocusSnapshot {
                editable: false,
                ..before
            },
            &after,
            "текст"
        ));
    }

    #[test]
    fn focus_snapshot_is_none_in_tests() {
        assert_eq!(focus_snapshot(), None);
    }
}
