//! Блокер сна: не даём машине уснуть, пока работают агенты.
//!
//! Ассёршн живёт внутри процесса демона, поэтому «застрявший» запрет сна
//! невозможен: упал Jarvis — снялся и запрет (в отличие от detached caffeinate).
//!
//! macOS: IOPMAssertion через IOKit. Проверка живьём: `pmset -g assertions | grep -i jarvis`.
//! Linux: дочерний `systemd-inhibit`, который держит lock, пока жив.
//! Проверка живьём: `systemd-inhibit --list | grep -i jarvis`.

/// Абстракция блокера — движок тестируется с фейком, продакшен ходит в систему.
pub trait Blocker: Send {
    /// true — не гасить и экран (idle+display), false — только сон системы.
    fn start(&mut self, keep_display_on: bool) -> u32;
    fn stop(&mut self, id: u32);
}

/* ================= macOS: IOPMAssertion ================= */

#[cfg(target_os = "macos")]
mod imp {
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

    pub(super) fn start(keep_display_on: bool) -> u32 {
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

    pub(super) fn stop(id: u32) {
        unsafe { IOPMAssertionRelease(id) };
    }
}

/* ================= Linux: systemd-inhibit ================= */

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::collections::HashMap;
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;

    /// Запущенные inhibit-процессы по выданному id. systemd снимает lock, когда
    /// процесс умирает, поэтому убить ребёнка = снять запрет; аварийное падение
    /// демона тоже снимает его само.
    static HOLDS: Mutex<Option<HashMap<u32, Child>>> = Mutex::new(None);
    static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

    pub(super) fn start(keep_display_on: bool) -> u32 {
        // idle — не уходить в простой; sleep — не засыпать по таймеру.
        // Экран дополнительно держим через idle-lock: отдельного «display» у
        // logind нет, гасить экран запрещает именно idle.
        let what = if keep_display_on { "idle:sleep" } else { "sleep" };
        let child = Command::new("systemd-inhibit")
            .arg(format!("--what={what}"))
            .args(["--who=Jarvis", "--why=Работают агенты", "--mode=block", "sleep", "infinity"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(child) = child else {
            eprintln!("[jarvis:keep-awake] systemd-inhibit недоступен — сон не блокируется");
            return 0;
        };
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut guard = HOLDS.lock().unwrap();
        guard.get_or_insert_with(HashMap::new).insert(id, child);
        id
    }

    pub(super) fn stop(id: u32) {
        if id == 0 {
            return; // старт не удался — снимать нечего
        }
        let mut guard = HOLDS.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            if let Some(mut child) = map.remove(&id) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

/// Продакшен-блокер: на macOS IOPMAssertion, на Linux systemd-inhibit.
/// Имя историческое — менять его пришлось бы во всех точках использования.
pub struct IopmBlocker;

impl Blocker for IopmBlocker {
    fn start(&mut self, keep_display_on: bool) -> u32 {
        imp::start(keep_display_on)
    }

    fn stop(&mut self, id: u32) {
        imp::stop(id)
    }
}
