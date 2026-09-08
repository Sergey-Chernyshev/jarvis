//! Opt-in debug harness for real WKWebView/IPC journeys in an isolated profile.
//! There is no production environment-variable switch: debug code must validate
//! an explicit launch argument, a temporary-directory marker, and JARVIS_DIR.

use serde_json::Value;
use std::io;
use tauri::Manager;

#[cfg(debug_assertions)]
struct Config {
    root: std::path::PathBuf,
    scenario: String,
}

#[cfg(debug_assertions)]
static CONFIG: std::sync::OnceLock<Config> = std::sync::OnceLock::new();

#[cfg(debug_assertions)]
pub fn fixture_root() -> Option<std::path::PathBuf> { CONFIG.get().map(|config| config.root.clone()) }

pub fn initialize() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    let Some(index) = args.iter().position(|arg| arg == "--native-smoke") else {
        return Ok(());
    };
    #[cfg(not(debug_assertions))]
    {
        let _ = index;
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native smoke requires a debug build",
        ));
    }
    #[cfg(debug_assertions)]
    {
        let fail = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        let root = args
            .get(index + 1)
            .ok_or_else(|| fail("missing native smoke directory"))?;
        let root = std::path::Path::new(root).canonicalize()?;
        let temporary = std::env::temp_dir().canonicalize()?;
        let shared_tmp = std::path::Path::new("/tmp").canonicalize()?;
        if !root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("jarvis-native-smoke-"))
            || (!root.starts_with(&temporary) && !root.starts_with(&shared_tmp))
        {
            return Err(fail(
                "native smoke profile must be a dedicated temporary directory",
            ));
        }
        let marker: Value = serde_json::from_slice(&std::fs::read(root.join("marker.json"))?)?;
        if marker != serde_json::json!({"kind":"jarvis-native-smoke","version":1}) {
            return Err(fail("invalid native smoke profile marker"));
        }
        let data = root.join("data").canonicalize()?;
        if data.parent() != Some(root.as_path())
            || crate::util::jarvis_dir().canonicalize()? != data
        {
            return Err(fail(
                "JARVIS_DIR must refer to this smoke profile's data directory",
            ));
        }
        let scenario_path = root.join("scenario.js").canonicalize()?;
        if scenario_path.parent() != Some(root.as_path()) {
            return Err(fail("scenario must be inside the smoke profile"));
        }
        let scenario = std::fs::read_to_string(scenario_path)?;
        if scenario.len() > 1_000_000 {
            return Err(fail("native smoke scenario is too large"));
        }
        CONFIG
            .set(Config { root, scenario })
            .map_err(|_| fail("native smoke already initialized"))?;
        Ok(())
    }
}

pub fn enabled() -> bool {
    #[cfg(debug_assertions)]
    {
        CONFIG.get().is_some()
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

/// A closed port prevents read-only engine health checks from borrowing the
/// user's sidecar. Smoke mode also suppresses process startup and voice warmup.
pub fn sidecar_port(normal: u16) -> u16 {
    if enabled() {
        0
    } else {
        normal
    }
}

pub fn initialization_script() -> String {
    #[cfg(debug_assertions)]
    if let Some(config) = CONFIG.get() {
        let scenario = &config.scenario;
        return format!(
            r#"
(() => {{
  if (window.top !== window || window.__JARVIS_NATIVE_SMOKE_STARTED__) return;
  window.__JARVIS_NATIVE_SMOKE_STARTED__ = true;
  const errors = [], steps = [], evidence = {{}};
  const describeError = error => String(error?.message || error) + (error?.stack ? '\n' + error.stack : '');
  addEventListener('error', e => errors.push({{type:'error', message:e.message, file:e.filename, line:e.lineno}}));
  addEventListener('unhandledrejection', e => errors.push({{type:'rejection', message:describeError(e.reason)}}));
  const run = async () => {{
    const started = performance.now();
    const t = {{
      assert(condition, message='Assertion failed') {{ if (!condition) throw new Error(message); }},
      async waitFor(check, timeoutMs=10000) {{
        const until = performance.now() + timeoutMs;
        while (performance.now() < until) {{ const value = await check(); if (value) return value; await new Promise(r => setTimeout(r, 40)); }}
        throw new Error('waitFor timed out: ' + check.toString());
      }},
      async step(name, fn) {{
        const start = performance.now();
        try {{ const value = await fn(); steps.push({{name, ok:true, elapsedMs:performance.now()-start}}); return value; }}
        catch (error) {{ steps.push({{name, ok:false, elapsedMs:performance.now()-start, error:describeError(error)}}); throw error; }}
      }},
      invoke(command, args) {{ return window.__TAURI__.core.invoke(command, args); }},
      async screenshot(name, label='main') {{
        // Occluded WKWebViews may suspend animation callbacks. The native
        // snapshot has its own deadline; reaching it must never depend on rAF.
        await Promise.race([
          new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))),
          new Promise(r => setTimeout(r, 250))
        ]);
        const path = await window.__TAURI__.core.invoke('native_smoke_screenshot', {{name,label}});
        (evidence.screenshots ||= []).push({{name,label,path}}); return path;
      }},
      geometry(selector) {{ return Array.from(document.querySelectorAll(selector)).map(e => ({{tag:e.tagName,id:e.id,text:e.innerText?.slice(0,120),rect:e.getBoundingClientRect().toJSON(),display:getComputedStyle(e).display,visibility:getComputedStyle(e).visibility}})); }},
      evidence(key, value) {{ evidence[key] = value; }}
    }};
    let failure = null;
    try {{ await t.waitFor(() => window.jarvis && window.__TAURI__?.core); await (async () => {{ {scenario}
    }})(); }} catch (error) {{ failure = describeError(error); }}
    const report = {{ok:!failure && errors.length===0, failure, errors, steps, evidence, elapsedMs:performance.now()-started,
      runtime:{{userAgent:navigator.userAgent,url:location.href,viewport:{{width:innerWidth,height:innerHeight,dpr:devicePixelRatio}}}},
      geometry:t.geometry('body,main,[role=dialog],.s2'), html:document.documentElement.outerHTML.slice(0,500000)}};
    try {{ await window.__TAURI__.core.invoke('native_smoke_report', {{report}}); }} catch (error) {{ console.error('native smoke report failed', error); }}
  }};
  if (document.readyState === 'loading') addEventListener('DOMContentLoaded', run, {{once:true}}); else run();
}})();
"#
        );
    }
    String::new()
}

#[tauri::command]
pub fn native_smoke_report(window: tauri::WebviewWindow, report: Value) -> Result<(), String> {
    #[cfg(debug_assertions)]
    if let Some(config) = CONFIG.get() {
        if !report.is_object() {
            return Err("smoke report must be an object".into());
        }
        if window.label() != "main" {
            return Err("smoke report requires the main window".into());
        }
        let mut report = report;
        report["native"] = serde_json::json!({
            "pid": std::process::id(), "profile": config.root,
            "visible": window.is_visible().ok(), "focused": window.is_focused().ok(),
            "outerPosition": window.outer_position().ok(), "innerSize": window.inner_size().ok(),
            "scaleFactor": window.scale_factor().ok(),
            "windowLabels": window.app_handle().webview_windows().keys().collect::<Vec<_>>(),
            "excluded": ["hook reconciliation", "sidecar processes", "voice warmup", "remotes", "global hotkeys", "background timers", "updater", "live audio capture"]
        });
        let ok = report.get("ok").and_then(Value::as_bool) == Some(true);
        std::fs::write(
            config.root.join("report.json"),
            serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        crate::native_smoke_checks::stop_editor();
        window.app_handle().exit(if ok { 0 } else { 1 });
        return Ok(());
    }
    let _ = (window, report);
    Err("native smoke is not enabled".into())
}

pub fn page_loaded(window: tauri::WebviewWindow, payload: tauri::webview::PageLoadPayload<'_>) {
    #[cfg(debug_assertions)]
    if let Some(config) = CONFIG.get() {
        use std::io::Write;
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config.root.join("stages.log"))
        {
            let _ = writeln!(log, "page {:?}: {}", payload.event(), payload.url());
        }
        if payload.event() == tauri::webview::PageLoadEvent::Finished {
            // Some WebKit builds run document-start scripts against about:blank;
            // a finished-load injection is a second chance, guarded in JS.
            let script = initialization_script();
            let _ = std::fs::write(config.root.join("injected.js"), &script);
            if let Err(error) = window.eval(script) {
                let _ = std::fs::write(config.root.join("injection-error.txt"), error.to_string());
            }
        }
    }
    let _ = (window, payload);
}

/// Capture only this app's WKWebView via WebKit, not the user's screen. This
/// does not need Screen Recording permission and never captures another app.
#[tauri::command]
pub async fn native_smoke_screenshot(
    window: tauri::WebviewWindow,
    name: String,
    label: Option<String>,
) -> Result<String, String> {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    if let Some(config) = CONFIG.get() {
        if window.label() != "main" {
            return Err("screenshot requires the main smoke window".into());
        }
        let label = label.as_deref().unwrap_or("main");
        if !matches!(label, "main" | "toast" | "onboarding") {
            return Err("screenshot target must be a smoke application window".into());
        }
        let target = window.app_handle().get_webview_window(label)
            .ok_or_else(|| format!("smoke window {label} does not exist"))?;
        let name = name.strip_suffix(".png").unwrap_or(&name);
        if name.is_empty()
            || name.len() > 80
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(
                "screenshot name must contain only letters, digits, hyphens and underscores".into(),
            );
        }
        let path = config.root.join(format!("{name}.png"));
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<Vec<u8>, String>>();
        target.with_webview(move |webview| {
            use block2::RcBlock;
            use objc2::{class, msg_send};
            use objc2::runtime::AnyObject;
            let sender = std::sync::Mutex::new(Some(tx));
            let callback = RcBlock::new(move |image: *mut AnyObject, error: *mut AnyObject| {
                let result = (|| unsafe {
                    if image.is_null() || !error.is_null() { return Err("WKWebView snapshot failed".into()); }
                    let tiff: *mut AnyObject = msg_send![image, TIFFRepresentation];
                    if tiff.is_null() { return Err("snapshot TIFF conversion failed".into()); }
                    let bitmap: *mut AnyObject = msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff];
                    if bitmap.is_null() { return Err("snapshot bitmap conversion failed".into()); }
                    let properties: *mut AnyObject = msg_send![class!(NSDictionary), dictionary];
                    let png: *mut AnyObject = msg_send![bitmap, representationUsingType: 4usize, properties: properties];
                    if png.is_null() { return Err("snapshot PNG conversion failed".into()); }
                    let length: usize = msg_send![png, length];
                    let bytes: *const u8 = msg_send![png, bytes];
                    if bytes.is_null() || length == 0 || length > 100_000_000 { return Err("invalid snapshot PNG".into()); }
                    Ok(std::slice::from_raw_parts(bytes, length).to_vec())
                })();
                if let Some(tx) = sender.lock().unwrap().take() { let _ = tx.send(result); }
            });
            unsafe {
                let native = webview.inner() as *mut AnyObject;
                let _: () = msg_send![native, takeSnapshotWithConfiguration: std::ptr::null::<AnyObject>(), completionHandler: &*callback];
            }
        }).map_err(|error| error.to_string())?;
        let png = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
            .await
            .map_err(|_| "WKWebView snapshot timed out")?
            .map_err(|_| "WKWebView snapshot callback dropped")??;
        tokio::fs::write(&path, png)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(path.to_string_lossy().into_owned());
    }
    let _ = (window, name, label);
    Err("native smoke screenshot is unavailable".into())
}
