//! Разрешение микрофона (macOS TCC). Безопасная проверка статуса БЕЗ промпта и
//! БЕЗ краша — даже если `NSMicrophoneUsageDescription` отсутствует (читает только
//! состояние TCC через `AVCaptureDevice.authorizationStatusForMediaType:`).
//!
//! Контракт (см. дизайн §1, ресёрч macos-mic-tcc):
//!  - `status()` — не триггерит диалог, не падает; вызывать ДО открытия захвата.
//!  - На не-macOS и в тестах — всегда `Authorized` (нет TCC).
//!
//! Запрос доступа (`request()`) показывает системный диалог ТОЛЬКО при
//! `NotDetermined`; на остальных статусах возвращает текущее решение.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicAuth {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
}

impl MicAuth {
    pub fn as_str(self) -> &'static str {
        match self {
            MicAuth::NotDetermined => "not-determined",
            MicAuth::Restricted => "restricted",
            MicAuth::Denied => "denied",
            MicAuth::Authorized => "authorized",
        }
    }
    /// AudioUnits must not open while the permission dialog is undecided.
    pub fn may_capture(self) -> bool {
        matches!(self, MicAuth::Authorized)
    }

    pub fn capture_error(self) -> Option<&'static str> {
        match self {
            MicAuth::Authorized => None,
            MicAuth::NotDetermined => Some("Разрешите Jarvis доступ к микрофону в системном диалоге, затем начните запись ещё раз."),
            MicAuth::Denied | MicAuth::Restricted => Some("Нет доступа к микрофону. Разрешите Jarvis доступ в Системных настройках → Конфиденциальность → Микрофон."),
        }
    }
}

#[cfg(all(target_os = "macos", not(test)))]
mod imp {
    use super::MicAuth;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use std::sync::atomic::{AtomicBool, Ordering};

    static REQUEST_PENDING: AtomicBool = AtomicBool::new(false);

    // AVMediaTypeAudio — NSString-константа из AVFoundation (@"soun").
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeAudio: *const AnyObject;
    }

    /// Безопасная проверка статуса: не триггерит промпт, не падает.
    pub fn status() -> MicAuth {
        unsafe {
            let cls = class!(AVCaptureDevice);
            let media: *const AnyObject = AVMediaTypeAudio;
            let s: i64 = msg_send![cls, authorizationStatusForMediaType: media];
            match s {
                1 => MicAuth::Restricted,
                2 => MicAuth::Denied,
                3 => MicAuth::Authorized,
                _ => MicAuth::NotDetermined, // 0 = NotDetermined
            }
        }
    }

    /// Показать системный диалог запроса доступа к микрофону. Реальный промпт
    /// появляется только при `NotDetermined` (и только если есть встроенный
    /// `NSMicrophoneUsageDescription` — в .app или в dev-бинаре с встроенным
    /// Info.plist); иначе коллбэк просто отдаёт текущее решение. Fire-and-forget:
    /// коллбэк игнорируем — захват откроется по факту разрешения.
    pub fn request() {
        use block2::RcBlock;
        use objc2::runtime::Bool;
        if status() != MicAuth::NotDetermined || REQUEST_PENDING.swap(true, Ordering::SeqCst) {
            return;
        }
        unsafe {
            let cls = class!(AVCaptureDevice);
            let media: *const AnyObject = AVMediaTypeAudio;
            let handler = RcBlock::new(|_granted: Bool| {
                REQUEST_PENDING.store(false, Ordering::SeqCst);
            });
            let _: () =
                msg_send![cls, requestAccessForMediaType: media, completionHandler: &*handler];
        }
    }
}

#[cfg(not(all(target_os = "macos", not(test))))]
mod imp {
    use super::MicAuth;
    /// Не-macOS / тесты: TCC нет — считаем доступ открытым.
    pub fn status() -> MicAuth {
        MicAuth::Authorized
    }
    /// Не-macOS / тесты: промпта нет — no-op.
    pub fn request() {}
}

/// Текущий статус разрешения микрофона.
pub fn status() -> MicAuth {
    imp::status()
}

/// Запросить доступ к микрофону (системный диалог при `NotDetermined`).
pub fn request() {
    if crate::native_smoke::enabled() {
        return;
    }
    imp::request()
}

/// Explicit recording actions can request access, but must be retried after the
/// decision. They never open a delayed recording after the user released PTT.
pub fn require_authorized() -> Result<(), String> {
    if crate::native_smoke::enabled() {
        return Err("Запись живого аудио отключена в изолированном режиме проверки".into());
    }
    let auth = status();
    if auth == MicAuth::NotDetermined {
        request();
    }
    auth.capture_error()
        .map_or(Ok(()), |error| Err(error.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_authorized_in_tests() {
        // В тестовой сборке TCC не дёргаем — всегда Authorized.
        assert_eq!(status(), MicAuth::Authorized);
    }

    #[test]
    fn may_capture_semantics() {
        assert!(MicAuth::Authorized.may_capture());
        assert!(!MicAuth::NotDetermined.may_capture());
        assert!(!MicAuth::Denied.may_capture());
        assert!(!MicAuth::Restricted.may_capture());
    }

    #[test]
    fn pending_permission_has_actionable_error_without_capture_permission() {
        assert!(MicAuth::NotDetermined
            .capture_error()
            .unwrap()
            .contains("ещё раз"));
        assert!(MicAuth::Denied
            .capture_error()
            .unwrap()
            .contains("Системных настройках"));
        assert!(MicAuth::Authorized.capture_error().is_none());
    }
}
