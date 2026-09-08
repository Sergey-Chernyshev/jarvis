//! A real nonactivating NSPanel hosts the notification WebView. The hidden Tao
//! NSWindow remains alive for Tauri's IPC/lifetime ownership. Never change its
//! Objective-C class: Tao has private ivars that an NSPanel does not share.

use super::{CGRect, configure_overlay};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use std::cell::RefCell;
use std::sync::OnceLock;

thread_local! {
    // This cell is accessed only by AppKit's main thread. Retained objects never
    // cross threads, and the panel lives as long as the application's UI loop.
    static PANEL: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

fn panel_class() -> &'static AnyClass {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| {
        extern "C" fn cannot_focus(_: &AnyObject, _: Sel) -> Bool { Bool::NO }
        let mut builder = ClassBuilder::new(c"JarvisNotificationPanel", class!(NSPanel))
            .expect("notification panel class is registered once");
        unsafe {
            builder.add_method(sel!(canBecomeKeyWindow), cannot_focus as extern "C" fn(_, _) -> _);
            builder.add_method(sel!(canBecomeMainWindow), cannot_focus as extern "C" fn(_, _) -> _);
        }
        builder.register()
    })
}

/// Must be called on AppKit's main thread. The panel is created while the toast
/// is still hidden at startup, never by constructing a WebView during dictation.
pub unsafe fn native_window(carrier: *mut AnyObject) -> Result<*mut AnyObject, String> {
    PANEL.with(|slot| {
        let mut owner = slot.borrow_mut();
        if let Some(panel) = owner.as_ref() {
            return Ok(Retained::as_ptr(panel) as *mut AnyObject);
        }
        let view: *mut AnyObject = msg_send![carrier, contentView];
        let view = Retained::retain(view).ok_or("notification content view is missing")?;
        let frame: CGRect = msg_send![carrier, frame];
        let allocated: *mut AnyObject = msg_send![panel_class(), alloc];
        // Borderless | NonactivatingPanel. The nonactivation tag must be set at
        // initialization, not patched onto an already-created ordinary window.
        let raw: *mut AnyObject = msg_send![allocated,
            initWithContentRect: frame, styleMask: (1usize << 7), backing: 2usize, defer: false];
        let panel = Retained::from_raw(raw).ok_or("notification panel allocation failed")?;
        let _: () = msg_send![raw, setReleasedWhenClosed: false];
        let _: () = msg_send![raw, setOpaque: false];
        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![raw, setBackgroundColor: clear];
        let _: () = msg_send![raw, setHasShadow: false];
        let _: () = msg_send![raw, setFloatingPanel: true];
        let _: () = msg_send![raw, setWorksWhenModal: true];
        let _: () = msg_send![raw, setBecomesKeyOnlyIfNeeded: true];
        let _: () = msg_send![raw, setAnimationBehavior: 2isize]; // None; content owns motion.
        configure_overlay(raw);
        // NSStatusWindowLevel: above normal/fullscreen content, at the level
        // intended for status UI. The notification does not need the main
        // launcher's screen-saver level.
        let _: () = msg_send![raw, setLevel: 25isize];
        let _: () = msg_send![carrier, orderOut: std::ptr::null::<AnyObject>()];
        // Tao's raw-window-handle implementation still expects its carrier to
        // have a contentView. Leave an empty native view there; it must never
        // be nil even though the actual Wry hierarchy moves into the panel.
        let placeholder: *mut AnyObject = msg_send![class!(NSView), alloc];
        let bounds: CGRect = msg_send![&*view, bounds];
        let placeholder: *mut AnyObject = msg_send![placeholder, initWithFrame: bounds];
        let placeholder = Retained::from_raw(placeholder).ok_or("notification carrier view allocation failed")?;
        let _: () = msg_send![carrier, setContentView: &*placeholder];
        // Retain the entire Wry parent view while detaching, preserving its
        // autoresizing/observers and the WKWebView's registered IPC handlers.
        let _: () = msg_send![raw, setContentView: &*view];
        *owner = Some(panel);
        Ok(raw)
    })
}
