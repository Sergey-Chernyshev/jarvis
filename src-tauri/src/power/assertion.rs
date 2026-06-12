//! Power assertion через IOKit — программный родственник caffeinate.
//!
//! IOPMAssertion живёт в процессе демона: краш = автоснятие, «застрявший»
//! запрет сна невозможен (в отличие от detached caffeinate у Raycast Coffee).
//! Проверка живьём: pmset -g assertions | grep -i jarvis

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};

type IOPMAssertionID = u32;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        level: u32,
        name: CFStringRef,
        id: *mut IOPMAssertionID,
    ) -> i32;
    fn IOPMAssertionRelease(id: IOPMAssertionID) -> i32;
}

const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;

/// Абстракция блокера — движок тестируется с фейком, продакшен ходит в IOKit.
pub trait Blocker: Send {
    /// true — не гасить и экран (PreventUserIdleDisplaySleep),
    /// false — только сон системы (PreventUserIdleSystemSleep).
    fn start(&mut self, keep_display_on: bool) -> u32;
    fn stop(&mut self, id: u32);
}

pub struct IopmBlocker;

impl Blocker for IopmBlocker {
    fn start(&mut self, keep_display_on: bool) -> u32 {
        let assertion_type = CFString::new(if keep_display_on {
            "PreventUserIdleDisplaySleep"
        } else {
            "PreventUserIdleSystemSleep"
        });
        let name = CFString::new("Jarvis: не спать");
        let mut id: IOPMAssertionID = 0;
        let rc = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.as_concrete_TypeRef(),
                K_IOPM_ASSERTION_LEVEL_ON,
                name.as_concrete_TypeRef(),
                &mut id,
            )
        };
        if rc != 0 {
            eprintln!("[jarvis:keep-awake] IOPMAssertionCreateWithName rc={rc}");
        }
        id
    }

    fn stop(&mut self, id: u32) {
        unsafe { IOPMAssertionRelease(id) };
    }
}
