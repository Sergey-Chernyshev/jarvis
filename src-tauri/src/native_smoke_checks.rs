//! Native observations and synthetic fixtures for the validated debug profile.
//! Optional insertion targets only the explicitly launched scratch editor.
//! No audio capture or permission requests.

use serde_json::Value;

#[cfg(all(debug_assertions, target_os = "macos"))]
static EDITOR: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

pub fn stop_editor() {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    if let Some(mut child) = EDITOR.lock().unwrap().take() {
        if let Some(root) = crate::native_smoke::fixture_root() { let _ = std::fs::write(root.join("editor-command"), "stop"); }
        // Let the owned helper complete its native fullscreen Space exit.
        for _ in 0..75 {
            if child.try_wait().ok().flatten().is_some() { return; }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let _ = child.kill(); let _ = child.wait();
    }
}

#[cfg(all(debug_assertions, target_os = "macos"))]
mod macos {
    use super::Value;
    use crate::stt::ax::FocusIdentity;
    use serde_json::json;
    use std::sync::Mutex;
    use tauri::Manager;

    static ORIGINAL_FOCUS: Mutex<Option<FocusIdentity>> = Mutex::new(None);
    static ACTIVATION_EVENTS: Mutex<Vec<Value>> = Mutex::new(Vec::new());
    static ACTIVATION_OBSERVERS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    static ACTIVATION_MONITORING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    unsafe fn record_activation(event: &'static str) {
        use objc2::{class, msg_send};
        use objc2::runtime::AnyObject;
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let foreground: *mut AnyObject = msg_send![workspace, frontmostApplication];
        let pid: i32 = if foreground.is_null() { 0 } else { msg_send![foreground, processIdentifier] };
        let active: bool = msg_send![application, isActive];
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let mut events = ACTIVATION_EVENTS.lock().unwrap();
        if events.len() < 200 {
            events.push(json!({"event":event,"timestampMs":timestamp,"foregroundPid":pid,
                "ownsForeground":pid == std::process::id() as i32,"active":active,
                "monitoring":ACTIVATION_MONITORING.load(std::sync::atomic::Ordering::SeqCst)}));
        }
    }

    pub async fn activation_observation(app: tauri::AppHandle, action: String) -> Result<Value, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || unsafe {
            use objc2::{class, msg_send};
            use objc2::runtime::AnyObject;
            extern "C" {
                static NSApplicationDidBecomeActiveNotification: *mut AnyObject;
                static NSApplicationDidResignActiveNotification: *mut AnyObject;
            }
            let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
            let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let result = (|| -> Result<Value, String> {
                match action.as_str() {
                    "app_activation_observe" => {
                        let mut tokens = ACTIVATION_OBSERVERS.lock().unwrap();
                        if !tokens.is_empty() { return Err("activation observation already started".into()); }
                        ACTIVATION_EVENTS.lock().unwrap().clear();
                        ACTIVATION_MONITORING.store(false, std::sync::atomic::Ordering::SeqCst);
                        for (name, event) in [(NSApplicationDidBecomeActiveNotification, "did-become-active"),
                            (NSApplicationDidResignActiveNotification, "did-resign-active")] {
                            let callback = block2::RcBlock::new(move |_notification: *mut AnyObject| record_activation(event));
                            let token: *mut AnyObject = msg_send![center, addObserverForName: name, object: application,
                                queue: std::ptr::null_mut::<AnyObject>(), usingBlock: &*callback];
                            if token.is_null() { return Err("native activation observer was not installed".into()); }
                            // The observation is debug-only and explicitly removed before fixture cleanup.
                            let retained = objc2::rc::Retained::retain(token).ok_or("native activation observer is missing")?;
                            tokens.push(objc2::rc::Retained::into_raw(retained) as usize);
                        }
                        record_activation("observation-started");
                    },
                    "app_activation_checkpoint" => {
                        ACTIVATION_MONITORING.store(true, std::sync::atomic::Ordering::SeqCst);
                        record_activation("monitoring-started");
                    },
                    "app_activation_stop" => {
                        ACTIVATION_MONITORING.store(false, std::sync::atomic::Ordering::SeqCst);
                        record_activation("monitoring-stopped");
                        for raw in ACTIVATION_OBSERVERS.lock().unwrap().drain(..) {
                            let token = objc2::rc::Retained::from_raw(raw as *mut AnyObject).expect("retained observer token");
                            let _: () = msg_send![center, removeObserver: &*token];
                        }
                    },
                    "app_activation_events" => {},
                    _ => return Err("unknown activation observation".into()),
                }
                Ok(json!({"events":ACTIVATION_EVENTS.lock().unwrap().clone(),
                    "monitoring":ACTIVATION_MONITORING.load(std::sync::atomic::Ordering::SeqCst),
                    "observing":!ACTIVATION_OBSERVERS.lock().unwrap().is_empty(),"pid":std::process::id(),
                    "provenance":"AppKit activation notifications for this owned debug app; only foreground PID, booleans and timestamps"}))
            })();
            let _ = tx.send(result);
        }).map_err(|error| error.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(2), rx).await
            .map_err(|_| "native activation observation timed out".to_string())?
            .map_err(|_| "native activation observation callback dropped".to_string())?
    }

    pub async fn activation_policy(app: tauri::AppHandle, accessory: bool) -> Result<Value, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || unsafe {
            use objc2::{class, msg_send};
            use objc2::runtime::AnyObject;
            let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let expected = if accessory { 1isize } else { 0isize };
            let accepted: bool = msg_send![application, setActivationPolicy: expected];
            let observed: isize = msg_send![application, activationPolicy];
            let _ = tx.send(json!({ "accepted":accepted, "nativeValue":observed,
                "policy":if observed == 1 { "accessory" } else if observed == 0 { "regular" } else { "prohibited" } }));
        }).map_err(|error| error.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(2), rx).await
            .map_err(|_| "native activation policy change timed out".to_string())?
            .map_err(|_| "native activation policy callback dropped".to_string())
    }

    pub async fn paste_into_editor(app: tauri::AppHandle, pid: i32) -> Result<Value, String> {
        tauri::async_runtime::spawn_blocking(move || {
            if crate::stt::ax::frontmost_pid_main(&app) != Some(pid) { return Err("scratch editor no longer owns focus".into()); }
            let focus = ORIGINAL_FOCUS.lock().unwrap().clone().ok_or("no captured scratch field")?;
            if focus.process_id != pid { return Err("captured field is not the scratch editor".into()); }
            let target = crate::stt::insert::InsertionTarget { process_id: Some(pid), focus: Some(focus) };
            let outcome = crate::stt::insert::insert_text("Jarvis native paste QA 👋", Some(&app), &target);
            Ok(json!({"copied":outcome.copied,"pasteSent":outcome.paste_sent,
                "confirmed":outcome.verdict == crate::stt::insert::InsertVerdict::Confirmed,"error":outcome.error}))
        }).await.map_err(|error| error.to_string())?
    }

    pub async fn toast_render_state(app: tauri::AppHandle) -> Result<Value, String> {
        let window = app.get_webview_window("toast").ok_or("smoke toast is missing")?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        window.with_webview(move |view| {
            use core_foundation::base::TCFType;
            use core_foundation::string::CFString;
            use objc2::runtime::AnyObject;
            use objc2::msg_send;
            let script = CFString::new(r#"JSON.stringify((()=>{
                const card = document.querySelector('.card.voice[data-phase]');
                return { phase:card?.dataset.phase, opacity:card ? Number(getComputedStyle(card).opacity) : 0,
                    rect:card?.getBoundingClientRect().toJSON(),
                    finiteAnimations:document.getAnimations().filter(a => a.playState === 'running' &&
                        Number.isFinite(a.effect?.getComputedTiming().endTime)).length };
            })())"#);
            let sender = Mutex::new(Some(tx));
            let callback = block2::RcBlock::new(move |value: *mut AnyObject, error: *mut AnyObject| {
                let result = (|| unsafe {
                    if value.is_null() || !error.is_null() { return Err("toast DOM observation failed".to_string()); }
                    let text: *const std::ffi::c_char = msg_send![value, UTF8String];
                    if text.is_null() { return Err("toast DOM observation returned no text".to_string()); }
                    serde_json::from_slice(std::ffi::CStr::from_ptr(text).to_bytes()).map_err(|error| error.to_string())
                })();
                if let Some(tx) = sender.lock().unwrap().take() { let _ = tx.send(result); }
            });
            unsafe {
                let native = view.inner() as *mut AnyObject;
                let _: () = msg_send![native, evaluateJavaScript: script.as_concrete_TypeRef() as *mut AnyObject, completionHandler: &*callback];
            }
        }).map_err(|error| error.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(2), rx).await
            .map_err(|_| "toast DOM observation timed out".to_string())?
            .map_err(|_| "toast DOM observation callback dropped".to_string())?
    }

    pub async fn observe_focus(app: tauri::AppHandle, capture: bool) -> Result<Value, String> {
        tauri::async_runtime::spawn_blocking(move || {
            let pid = super::EDITOR.lock().unwrap().as_ref().map(|child| child.id()).unwrap_or_else(std::process::id) as i32;
            let trusted = crate::stt::ax::input_permission_granted();
            let foreground = crate::stt::ax::frontmost_pid_main(&app);
            // Only this process or its explicitly launched scratch child may
            // supply an AX field; unrelated foreground applications are refused.
            let current = if trusted && foreground == Some(pid) {
                crate::stt::ax::focus_identity_main(&app).filter(|identity| identity.process_id == pid)
            } else {
                None
            };
            if capture {
                let available = current.is_some();
                *ORIGINAL_FOCUS.lock().unwrap() = current;
                return json!({ "available": available, "trusted": trusted,
                    "ownsForeground": foreground == Some(pid), "operation": "capture",
                    "provenance": "production AXUIElementCopyAttributeValue; scratch app only" });
            }
            let original = ORIGINAL_FOCUS.lock().unwrap().clone();
            let same = original.as_ref().zip(current.as_ref())
                .map(|(before, after)| before.same_element(after));
            json!({ "available": same.is_some(), "sameElement": same,
                "trusted": trusted, "ownsForeground": foreground == Some(pid),
                "operation": "compare", "provenance": "production retained AXUIElement CFEqual; scratch app only" })
        }).await.map_err(|error| error.to_string())
    }

    pub async fn window_properties(app: tauri::AppHandle) -> Result<Value, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let owner = app.clone();
        app.run_on_main_thread(move || {
            use objc2::runtime::AnyObject;
            use objc2::msg_send;
            let mut values = serde_json::Map::new();
            for label in ["main", "toast", "onboarding"] {
                let Some(window) = owner.get_webview_window(label) else { continue };
                let Ok(raw) = window.ns_window() else { continue };
                let value = unsafe {
                    let native = if label == "toast" {
                        match crate::platform::toast_native_window(raw as *mut AnyObject) {
                            Ok(panel) => panel,
                            Err(error) => { values.insert(label.into(), json!({"error": error})); continue; }
                        }
                    } else { raw as *mut AnyObject };
                    let native_class = (*native).class().name().to_string_lossy().into_owned();
                    let style_mask: usize = msg_send![native, styleMask];
                    let is_panel: bool = msg_send![native, isKindOfClass: objc2::class!(NSPanel)];
                    let behavior: usize = msg_send![native, collectionBehavior];
                    let level: isize = msg_send![native, level];
                    let visible: bool = msg_send![native, isVisible];
                    let key: bool = msg_send![native, isKeyWindow];
                    let can_key: bool = msg_send![native, canBecomeKeyWindow];
                    let active_space: bool = msg_send![native, isOnActiveSpace];
                    let number: isize = msg_send![native, windowNumber];
                    let frame: crate::platform::CGRect = msg_send![native, frame];
                    let screen: *mut AnyObject = msg_send![native, screen];
                    let work_area = if screen.is_null() { Value::Null } else {
                        let area: crate::platform::CGRect = msg_send![screen, visibleFrame];
                        json!({"x": area.origin.x, "y": area.origin.y,
                            "width": area.size.width, "height": area.size.height})
                    };
                    json!({"visible": visible, "key": key, "canBecomeKey": can_key, "windowNumber": number,
                        "nativeClass":native_class,"isPanel":is_panel,"styleMask":style_mask,
                        "nonactivatingPanel":style_mask & (1 << 7) != 0,
                        "onActiveSpace": active_space, "level": level,
                        "collectionBehavior": behavior,
                        "joinsAllSpaces": behavior & 1 != 0,
                        "fullScreenAuxiliary": behavior & (1 << 8) != 0,
                        "joinsAllApplications": behavior & (1 << 18) != 0,
                        "ignoresCycle": behavior & (1 << 6) != 0,
                        "frame": {"x": frame.origin.x, "y": frame.origin.y,
                            "width": frame.size.width, "height": frame.size.height},
                        "screenWorkArea": work_area})
                };
                values.insert(label.into(), value);
            }
            let _ = tx.send(Value::Object(values));
        }).map_err(|error| error.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(2), rx).await
            .map_err(|_| "native window observation timed out".to_string())?
            .map_err(|_| "native window observation callback dropped".to_string())
    }
}

#[tauri::command]
pub async fn native_smoke_probe(window: tauri::WebviewWindow, operation: String) -> Result<Value, String> {
    if !crate::native_smoke::enabled() || window.label() != "main" {
        return Err("native smoke probe requires the isolated debug main window".into());
    }
    #[cfg(all(debug_assertions, target_os = "macos"))]
    {
        use tauri::Manager;
        let app = window.app_handle().clone();
        match operation.as_str() {
            "codex_observe" => {
                // Read actual registered transcripts with the production
                // importer, writing cursors only in this disposable profile.
                static MONITOR: std::sync::OnceLock<std::sync::Mutex<crate::codex_live::Monitor>> = std::sync::OnceLock::new();
                let updates = tokio::task::spawn_blocking(|| {
                    let registry = crate::session_identity::registry()?;
                    let mut monitor = MONITOR.get_or_init(|| std::sync::Mutex::new(crate::codex_live::Monitor::new(&crate::util::jarvis_dir())))
                        .lock().map_err(|e| e.to_string())?;
                    let updates = monitor.scan(&registry, crate::util::now_ms());
                    monitor.persist()?;
                    Ok::<_, String>(updates)
                }).await.map_err(|e| e.to_string())??;
                let count = updates.len(); let d = crate::daemon::Daemon::get(&app);
                for update in updates { crate::codex_live::apply_update(&d, update); }
                Ok(serde_json::json!({"updates":count,"sessions":d.snapshot().len()}))
            },
            "app_activation_observe" | "app_activation_checkpoint" | "app_activation_events" | "app_activation_stop" => {
                if operation == "app_activation_observe" && EDITOR.lock().unwrap().is_some() {
                    return Err("start activation observation before the owned editor".into());
                }
                macos::activation_observation(app, operation).await
            },
            "app_policy_accessory" | "app_policy_regular" => {
                if EDITOR.lock().unwrap().is_some() { return Err("set fixture activation policy before starting its editor".into()); }
                macos::activation_policy(app, operation == "app_policy_accessory").await
            },
            "editor_start" => {
                let root = crate::native_smoke::fixture_root().ok_or("missing profile")?;
                let mut slot = EDITOR.lock().unwrap();
                if slot.is_some() { return Err("scratch editor already running".into()); }
                let child = std::process::Command::new(root.join("qa-editor")).arg(&root)
                    .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                    .spawn().map_err(|error| error.to_string())?;
                let pid = child.id(); *slot = Some(child);
                Ok(serde_json::json!({"pid":pid}))
            },
            "editor_focus_a" | "editor_focus_b" | "editor_enter_fullscreen" | "editor_exit_fullscreen" | "editor_monitor_on" | "editor_monitor_off" => {
                if EDITOR.lock().unwrap().is_none() { return Err("scratch editor is absent".into()); }
                let root = crate::native_smoke::fixture_root().ok_or("missing profile")?;
                let command = match operation.as_str() {
                    "editor_focus_a" => "focus-a", "editor_focus_b" => "focus-b",
                    "editor_enter_fullscreen" => "enter-fullscreen", "editor_exit_fullscreen" => "exit-fullscreen",
                    "editor_monitor_on" => "monitor-on", "editor_monitor_off" => "monitor-off",
                    _ => unreachable!(),
                };
                std::fs::write(root.join("editor-command"), command).map_err(|error| error.to_string())?;
                Ok(serde_json::json!({"ok":true}))
            },
            "editor_state" => {
                let root = crate::native_smoke::fixture_root().ok_or("missing profile")?;
                let bytes = tokio::fs::read(root.join("editor-state.json")).await.map_err(|error| error.to_string())?;
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())
            },
            "editor_paste" => {
                let pid = EDITOR.lock().unwrap().as_ref().ok_or("scratch editor is absent")?.id() as i32;
                macos::paste_into_editor(app, pid).await
            },
            "editor_stop" => {
                tauri::async_runtime::spawn_blocking(stop_editor).await.map_err(|error| error.to_string())?;
                Ok(serde_json::json!({"ok":true}))
            },
            "permissions" => Ok(serde_json::json!({
                "microphone": crate::stt::mic_permission::status().as_str(),
                "accessibility": crate::stt::ax::input_permission_granted(),
                "provenance": "native authorization status; no request made"
            })),
            "focus_capture" => macos::observe_focus(app, true).await,
            "focus_compare" => macos::observe_focus(app, false).await,
            "window_properties" => macos::window_properties(app).await,
            "toast_render_state" => macos::toast_render_state(app).await,
            "seed_transcript" => {
                let daemon = app.state::<std::sync::Arc<crate::daemon::Daemon>>().inner().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let (id, saved) = daemon.transcripts.push_styled_with_status(
                        "Синтетическая запись для проверки сохранения", "dictation", Some("clean"), false);
                    if !saved { return Err("synthetic transcript fixture was not persisted".into()); }
                    Ok(serde_json::json!({"id": id, "provenance": "fixed synthetic text in isolated temporary profile"}))
                }).await.map_err(|error| error.to_string())?
            },
            "transcript_disk" => {
                let bytes = tokio::fs::read(crate::stt::transcripts::Transcripts::default_path()).await
                    .map_err(|error| error.to_string())?;
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())
            },
            "toast_present" => {
                let daemon = app.state::<std::sync::Arc<crate::daemon::Daemon>>();
                crate::route::hud::emit(&daemon, crate::route::hud::Phase::Error {
                    msg: "Изолированная проверка уведомления. Запись не запускалась.".into(),
                });
                Ok(serde_json::json!({"fixture": "synthetic HUD error; production event and layout"}))
            },
            "toast_listening" | "toast_analyzing" | "toast_empty" => {
                let daemon = app.state::<std::sync::Arc<crate::daemon::Daemon>>();
                let phase = match operation.as_str() {
                    "toast_listening" => crate::route::hud::Phase::Listening { secs: 30 },
                    "toast_analyzing" => crate::route::hud::Phase::Analyzing,
                    "toast_empty" => crate::route::hud::Phase::Empty,
                    _ => unreachable!(),
                };
                crate::route::hud::emit(&daemon, phase);
                Ok(serde_json::json!({"fixture":"synthetic phase through production HUD event and layout; no audio capture"}))
            },
            "toast_dismiss" => {
                let daemon = app.state::<std::sync::Arc<crate::daemon::Daemon>>();
                crate::route::hud::emit(&daemon, crate::route::hud::Phase::Dismiss);
                Ok(serde_json::json!({"dismissed": true}))
            },
            _ => Err("unknown native smoke observation".into()),
        }
    }
    #[cfg(not(all(debug_assertions, target_os = "macos")))]
    {
        let _ = operation;
        Err("native observations require a macOS debug build".into())
    }
}
