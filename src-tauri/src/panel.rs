//! macOS-only: convert the tray popover's stock NSWindow into a
//! non-activating NSPanel so it behaves like a menu-bar popover —
//! shows + accepts input WITHOUT activating the application.
//!
//! Why the conversion is needed:
//!   - Tauri creates the popover as an NSWindow.
//!   - `window.show()` calls `[orderFront:]`; `window.set_focus()` calls
//!     `[makeKeyAndOrderFront:]`. Neither activates the application
//!     (Apple docs are explicit about this).
//!   - When the app is in the background and the user clicks the tray,
//!     the window appears but the app stays inactive. Clicks on the
//!     webview don't reach React reliably, and the Focused(false)
//!     handler fires spuriously and hides the popover.
//!
//! An NSPanel with `NSWindowStyleMaskNonactivatingPanel` is the
//! AppKit-blessed way out: the panel can be key and accept mouse + key
//! input while the parent app remains in the background. Clicks outside
//! still fire Focused(false) (which the existing handler in lib.rs uses
//! for click-outside-to-dismiss).
//!
//! Implementation note: we swap the NSWindow instance's Obj-C class to
//! NSPanel via `object_setClass`. This is safe because NSPanel is a
//! pure subclass of NSWindow that only adds support for a few extra
//! style-mask bits — the storage layout is identical, all existing
//! pointers (Tauri's, the NSViewController graph, etc.) remain valid.

use objc2::msg_send;
use objc2::runtime::AnyObject;
use tauri::WebviewWindow;

// AppKit constants. Hard-coded rather than imported via objc2-app-kit's
// typed wrappers so this file is stable across objc2-app-kit point
// releases — the bit values themselves are part of the macOS ABI and
// won't change.
const NS_WINDOW_STYLE_MASK_NONACTIVATING_PANEL: usize = 1 << 7;
const NS_WINDOW_COLLECTION_BEHAVIOR_CAN_JOIN_ALL_SPACES: usize = 1 << 0;
const NS_WINDOW_COLLECTION_BEHAVIOR_TRANSIENT: usize = 1 << 3;
const NS_WINDOW_COLLECTION_BEHAVIOR_FULL_SCREEN_AUXILIARY: usize = 1 << 8;
const NS_STATUS_WINDOW_LEVEL: isize = 25;

pub fn make_popover_panel(window: &WebviewWindow) -> Result<(), String> {
    let ptr = window
        .ns_window()
        .map_err(|e| format!("ns_window(): {e}"))?;
    if ptr.is_null() {
        return Err("ns_window() returned null".into());
    }

    // SAFETY: Tauri owns the NSWindow for the app's lifetime; the calls
    // below are standard AppKit setters with no Rust-side side effects.
    unsafe {
        let ns_window: *mut AnyObject = ptr as *mut AnyObject;

        // (1) Re-class as NSPanel. This is the load-bearing step —
        // without it the NonactivatingPanel bit is silently ignored by
        // AppKit (the bit only takes effect on NSPanel instances).
        let panel_class = objc2::class!(NSPanel);
        let _: *const AnyObject =
            msg_send![ns_window, setClass: panel_class];

        // (2) Add NonactivatingPanel to the style mask. Preserve the
        // existing mask (borderless, transparent, etc.).
        let cur_mask: usize = msg_send![ns_window, styleMask];
        let new_mask = cur_mask | NS_WINDOW_STYLE_MASK_NONACTIVATING_PANEL;
        let _: () = msg_send![ns_window, setStyleMask: new_mask];

        // (3) Stay visible across Space switches and when another app is
        // in Full-Screen mode. Canonical collection-behavior for tray
        // popovers.
        let cur_cb: usize = msg_send![ns_window, collectionBehavior];
        let new_cb = cur_cb
            | NS_WINDOW_COLLECTION_BEHAVIOR_CAN_JOIN_ALL_SPACES
            | NS_WINDOW_COLLECTION_BEHAVIOR_TRANSIENT
            | NS_WINDOW_COLLECTION_BEHAVIOR_FULL_SCREEN_AUXILIARY;
        let _: () = msg_send![ns_window, setCollectionBehavior: new_cb];

        // (4) Float above ordinary windows. Status-bar level matches the
        // tray icon's own level so the popover sits naturally beneath it.
        let _: () = msg_send![ns_window, setLevel: NS_STATUS_WINDOW_LEVEL];

        // (5) Don't auto-hide when the app deactivates. NSPanel defaults
        // can be YES for some subclasses; we want the popover to stay up
        // until the user clicks outside it (Focused(false) handler in
        // lib.rs takes care of dismissal).
        let _: () = msg_send![ns_window, setHidesOnDeactivate: false];

        // (6) Standard floating-panel flags. `setBecomesKeyOnlyIfNeeded:
        // false` makes sure typing in form fields works; `setFloatingPanel:
        // true` is the NSPanel-only knob that pairs with NonactivatingPanel.
        let _: () = msg_send![ns_window, setBecomesKeyOnlyIfNeeded: false];
        let _: () = msg_send![ns_window, setFloatingPanel: true];
    }

    Ok(())
}
