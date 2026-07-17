//! Best-effort проверка вставки диктовки через Accessibility API: был ли фокус
//! на редактируемом элементе и изменилось ли его значение после ⌘V.
//!
//! Всё строго best-effort: AX может быть недоступен (нет разрешения — хотя для
//! синтеза ⌘V оно то же самое), элемент может не отдавать AXValue (secure-поля,
//! кастомные редакторы) — любой сбой означает «вставка не подтверждена», это
//! НЕ ошибка. Никаких паник, никаких блокировок пайплайна диктовки.

#![allow(non_upper_case_globals)]

/// Снимок сфокусированного элемента до/после вставки.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusSnapshot {
    /// AX-роль элемента (для диагностики в логе).
    pub role: String,
    /// Похож ли элемент на поле ввода (роль или settable AXValue).
    pub editable: bool,
    /// Текстовое значение элемента (None — не отдаёт/не строка).
    pub value: Option<String>,
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
    after_n.contains(&tail)
}

// ── AX FFI (только вне тестов: в CI/юнитах системного AX нет) ───────────────

#[cfg(not(test))]
mod ffi {
    use super::FocusSnapshot;
    use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
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
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: CFTypeRef,
        ) -> AXError;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
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
                        // дать приложению построить дерево
                        std::thread::sleep(std::time::Duration::from_millis(150));
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
            CFRelease(focused);
            Some(FocusSnapshot {
                role,
                editable: editable_role || settable,
                value,
            })
        }
    }
}

/// Снимок сфокусированного элемента (best-effort; в тестах всегда None).
pub fn focus_snapshot() -> Option<FocusSnapshot> {
    #[cfg(not(test))]
    {
        // AX-вызовы уходят в чужие процессы — защищаемся от любых сюрпризов.
        std::panic::catch_unwind(ffi::focus_snapshot).unwrap_or(None)
    }
    #[cfg(test)]
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
    fn focus_snapshot_is_none_in_tests() {
        assert_eq!(focus_snapshot(), None);
    }
}
