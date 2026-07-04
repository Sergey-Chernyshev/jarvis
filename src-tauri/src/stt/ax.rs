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
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
    }

    /// Скопировать строковый AX-атрибут элемента (None — нет/не строка).
    unsafe fn copy_string_attr(el: AXUIElementRef, name: &str) -> Option<String> {
        let attr = CFString::new(name);
        let mut out: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(el, attr.as_concrete_TypeRef(), &mut out);
        if err != K_AX_ERROR_SUCCESS || out.is_null() {
            return None;
        }
        if CFGetTypeID(out) != CFStringGetTypeID() {
            CFRelease(out);
            return None;
        }
        let s = CFString::wrap_under_create_rule(out as CFStringRef).to_string();
        Some(s)
    }

    /// Снимок сфокусированного элемента системы. None — AX не отдал фокус.
    pub fn focus_snapshot() -> Option<FocusSnapshot> {
        unsafe {
            let sys = AXUIElementCreateSystemWide();
            if sys.is_null() {
                return None;
            }
            let attr = CFString::new("AXFocusedUIElement");
            let mut focused: CFTypeRef = std::ptr::null();
            let err =
                AXUIElementCopyAttributeValue(sys, attr.as_concrete_TypeRef(), &mut focused);
            CFRelease(sys);
            if err != K_AX_ERROR_SUCCESS || focused.is_null() {
                return None;
            }

            let role = copy_string_attr(focused, "AXRole").unwrap_or_default();
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
        assert!(value_confirms_insert(&None, &s("надиктовано"), "надиктовано"));
    }

    #[test]
    fn rejects_unchanged_or_missing_value() {
        assert!(!value_confirms_insert(&s("такой же"), &s("такой же"), "текст"));
        assert!(!value_confirms_insert(&s("что-то"), &None, "текст"));
        assert!(!value_confirms_insert(&None, &None, "текст"));
    }

    #[test]
    fn rejects_change_without_inserted_tail() {
        // значение изменилось, но вставленного там нет (например, элемент сам обновился)
        assert!(!value_confirms_insert(&s("до"), &s("после"), "надиктованный текст"));
    }

    #[test]
    fn normalizes_whitespace_and_checks_tail_of_long_text() {
        let long = "слово ".repeat(50); // 300 символов
        let shown = format!("начало поля {}", long.split_whitespace().collect::<Vec<_>>().join(" "));
        assert!(value_confirms_insert(&s("начало поля"), &Some(shown), &long));
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
