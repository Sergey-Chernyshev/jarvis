use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::PathBuf;

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use tauri::AppHandle;

const NS_MODAL_RESPONSE_OK: isize = 1;
const PICKER_WINDOW_LEVEL: isize = 1_001;

pub async fn pick(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let result = std::panic::catch_unwind(|| unsafe { pick_on_main_thread() })
            .unwrap_or_else(|_| Err("Нативный выбор папки аварийно завершился".into()));
        let _ = sender.send(result);
    })
    .map_err(|_| "Не удалось открыть нативный выбор папки".to_string())?;
    receiver
        .await
        .map_err(|_| "Нативный выбор папки не вернул результат".to_string())?
}

unsafe fn pick_on_main_thread() -> Result<Option<PathBuf>, String> {
    let application: *mut AnyObject =
        unsafe { msg_send![class!(NSApplication), sharedApplication] };
    if application.is_null() {
        return Err("NSApplication недоступен".into());
    }
    let _: () = unsafe { msg_send![application, activateIgnoringOtherApps: true] };

    let panel: *mut AnyObject = unsafe { msg_send![class!(NSOpenPanel), openPanel] };
    if panel.is_null() {
        return Err("NSOpenPanel недоступен".into());
    }
    let _: () = unsafe { msg_send![panel, setCanChooseDirectories: true] };
    let _: () = unsafe { msg_send![panel, setCanChooseFiles: false] };
    let _: () = unsafe { msg_send![panel, setAllowsMultipleSelection: false] };
    let _: () = unsafe { msg_send![panel, setResolvesAliases: true] };
    let _: () = unsafe { msg_send![panel, setLevel: PICKER_WINDOW_LEVEL] };

    let response: isize = unsafe { msg_send![panel, runModal] };
    if response != NS_MODAL_RESPONSE_OK {
        return Ok(None);
    }
    let url: *mut AnyObject = unsafe { msg_send![panel, URL] };
    if url.is_null() {
        return Err("NSOpenPanel не вернул выбранную папку".into());
    }
    let representation: *const c_char = unsafe { msg_send![url, fileSystemRepresentation] };
    if representation.is_null() {
        return Err("Не удалось прочитать путь выбранной папки".into());
    }
    let bytes = unsafe { CStr::from_ptr(representation) }.to_bytes();
    if bytes.is_empty() || bytes.contains(&0) {
        return Err("Выбранная папка имеет некорректный путь".into());
    }
    Ok(Some(PathBuf::from(
        String::from_utf8_lossy(bytes).into_owned(),
    )))
}
