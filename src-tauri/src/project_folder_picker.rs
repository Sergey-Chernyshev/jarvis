use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use tauri::AppHandle;
use tokio::sync::oneshot;

const NS_MODAL_RESPONSE_OK: isize = 1;
const PICKER_WINDOW_LEVEL: isize = 1_001;
static ACTIVE_PICKERS: AtomicUsize = AtomicUsize::new(0);

type PickResult = Result<Option<PathBuf>, String>;
type PickResponder = Arc<Mutex<Option<oneshot::Sender<PickResult>>>>;

pub async fn pick(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    let _activity = PickerActivity::enter();
    let (sender, receiver) = oneshot::channel();
    let responder = Arc::new(Mutex::new(Some(sender)));
    let main_thread_responder = Arc::clone(&responder);
    app.run_on_main_thread(move || {
        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            begin_pick_on_main_thread(Arc::clone(&main_thread_responder))
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => complete_pick(&main_thread_responder, Err(error)),
            Err(_) => complete_pick(
                &main_thread_responder,
                Err("Нативный выбор папки аварийно завершился".into()),
            ),
        }
    })
    .map_err(|_| "Не удалось открыть нативный выбор папки".to_string())?;
    receiver
        .await
        .map_err(|_| "Нативный выбор папки не вернул результат".to_string())?
}

pub fn is_active() -> bool {
    ACTIVE_PICKERS.load(Ordering::Acquire) > 0
}

struct PickerActivity;

impl PickerActivity {
    fn enter() -> Self {
        ACTIVE_PICKERS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for PickerActivity {
    fn drop(&mut self) {
        let previous = ACTIVE_PICKERS.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "picker activity counter underflow");
    }
}

fn complete_pick(responder: &PickResponder, result: PickResult) {
    let mut sender = responder
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(sender) = sender.take() {
        let _ = sender.send(result);
    }
}

unsafe fn begin_pick_on_main_thread(responder: PickResponder) -> Result<(), String> {
    let application: *mut AnyObject =
        unsafe { msg_send![class!(NSApplication), sharedApplication] };
    if application.is_null() {
        return Err("NSApplication недоступен".into());
    }
    let _: () = unsafe { msg_send![application, activateIgnoringOtherApps: true] };

    let panel: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSOpenPanel), openPanel] };
    let panel = panel.ok_or_else(|| "NSOpenPanel недоступен".to_string())?;
    let _: () = unsafe { msg_send![&*panel, setCanChooseDirectories: true] };
    let _: () = unsafe { msg_send![&*panel, setCanChooseFiles: false] };
    let _: () = unsafe { msg_send![&*panel, setAllowsMultipleSelection: false] };
    let _: () = unsafe { msg_send![&*panel, setResolvesAliases: true] };
    let _: () = unsafe { msg_send![&*panel, setLevel: PICKER_WINDOW_LEVEL] };

    let callback_panel = panel.clone();
    let callback_responder = Arc::clone(&responder);
    let completion: RcBlock<dyn Fn(isize)> = RcBlock::new(move |response: isize| {
        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            selected_folder(&callback_panel, response)
        }))
        .unwrap_or_else(|_| Err("Не удалось обработать выбранную папку".into()));
        complete_pick(&callback_responder, result);
    });
    let _: () = unsafe { msg_send![&*panel, beginWithCompletionHandler: &*completion] };
    Ok(())
}

unsafe fn selected_folder(panel: &AnyObject, response: isize) -> PickResult {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_activity_is_cleared_on_every_scope_exit() {
        assert!(!is_active());
        {
            let _activity = PickerActivity::enter();
            assert!(is_active());
        }
        assert!(!is_active());
    }
}
