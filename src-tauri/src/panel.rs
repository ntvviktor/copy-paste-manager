//! Popup window behaviour.
//!
//! On macOS the window is swizzled into a non-activating `NSPanel`. That is the
//! whole trick behind this kind of app: the panel can take keystrokes (so vim
//! navigation works) without activating ClipVim, so the app you were typing in
//! never loses focus and is still there to receive the paste.
//!
//! Other platforms get a plain always-on-top window, which behaves well enough
//! on Windows and on X11.

use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

pub const MAIN_WINDOW: &str = "main";

pub fn main_window(app: &AppHandle) -> Result<WebviewWindow, String> {
    app.get_webview_window(MAIN_WINDOW)
        .ok_or_else(|| format!("window `{MAIN_WINDOW}` not found"))
}

/// Place the popup horizontally centred and in the upper fifth of whichever
/// screen the cursor is on — the screen the user is currently looking at.
///
/// macOS has its own version in `platform`; tao's cursor/monitor APIs cannot be
/// trusted there. See the note on `platform::reposition`.
#[cfg(not(target_os = "macos"))]
fn reposition(window: &WebviewWindow) -> Result<(), String> {
    use tauri::PhysicalPosition;

    let size = window.outer_size().map_err(|e| e.to_string())?;

    let monitor = window
        .cursor_position()
        .ok()
        .and_then(|point| window.monitor_from_point(point.x, point.y).ok().flatten())
        .or_else(|| window.current_monitor().ok().flatten());

    let Some(monitor) = monitor else {
        return window.center().map_err(|e| e.to_string());
    };

    let origin = monitor.position();
    let screen = monitor.size();
    let x = origin.x + (screen.width as i32 - size.width as i32) / 2;
    let y = origin.y + (screen.height as i32 / 5);

    window
        .set_position(PhysicalPosition { x, y })
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
// `tauri-nspanel` re-exports the deprecated `cocoa` crate; it does the same
// internally. Nothing to fix on our side until the plugin moves to objc2.
#[allow(deprecated)]
mod platform {
    use super::{main_window, MAIN_WINDOW};
    use tauri::AppHandle;
    use tauri_nspanel::{
        cocoa::{
            appkit::{NSEvent, NSScreen, NSWindow, NSWindowCollectionBehavior},
            base::{id, nil},
            foundation::{NSArray, NSPoint},
        },
        panel_delegate, ManagerExt, Panel, WebviewWindowExt,
    };

    /// `NSWindowStyleMaskNonactivatingPanel` — accepts key events without
    /// activating the owning app.
    #[allow(non_upper_case_globals)]
    const NSWindowStyleMaskNonactivatingPanel: i32 = 1 << 7;

    /// `NSStatusWindowLevel`: above normal windows, the Dock and the menu bar.
    #[allow(non_upper_case_globals)]
    const NSStatusWindowLevel: i32 = 25;

    pub fn init(app: &AppHandle) -> Result<(), String> {
        let window = main_window(app)?;
        let panel = window.to_panel().map_err(|e| e.to_string())?;

        panel.set_level(NSStatusWindowLevel);
        panel.set_style_mask(NSWindowStyleMaskNonactivatingPanel);

        // Show on every Space, and float over full-screen apps instead of
        // forcing a Space switch.
        panel.set_collection_behaviour(
            NSWindowCollectionBehavior::NSWindowCollectionBehaviorCanJoinAllSpaces
                | NSWindowCollectionBehavior::NSWindowCollectionBehaviorStationary
                | NSWindowCollectionBehavior::NSWindowCollectionBehaviorFullScreenAuxiliary,
        );

        // We manage visibility ourselves; let the panel survive app deactivation.
        panel.set_hides_on_deactivate(false);

        // Dismiss when focus goes elsewhere, the way Spotlight does.
        let delegate = panel_delegate!(ClipVimPanelDelegate {
            window_did_resign_key
        });
        let handle = app.clone();
        delegate.set_listener(Box::new(move |event: String| {
            if event == "window_did_resign_key" {
                let _ = hide(&handle);
            }
        }));
        panel.set_delegate(delegate);

        Ok(())
    }

    fn panel(app: &AppHandle) -> Result<Panel, String> {
        app.get_webview_panel(MAIN_WINDOW)
            .map_err(|err| format!("panel not initialised: {err:?}"))
    }

    /// The screen the mouse is on, in Cocoa's coordinate space.
    ///
    /// Everything here stays in Cocoa points with a bottom-left origin, because
    /// tao's portable helpers cannot survive the round trip. `cursor_position()`
    /// returns points multiplied by the *primary* display's backing scale
    /// factor, while `monitor_from_point()` matches against `CGDisplayBounds`,
    /// which is in unscaled points. On a Retina laptop the cursor therefore
    /// arrives doubled: no monitor's bounds contain it, the lookup returns
    /// `None`, and we used to fall back to `current_monitor()` — the screen the
    /// panel was last on. That is why the popup kept appearing on the built-in
    /// display no matter which external screen was in use.
    unsafe fn screen_with_cursor() -> Option<id> {
        let cursor = NSEvent::mouseLocation(nil);
        let screens = NSScreen::screens(nil);

        (0..NSArray::count(screens))
            .map(|i| NSArray::objectAtIndex(screens, i))
            .find(|&screen| {
                let f = NSScreen::frame(screen);
                cursor.x >= f.origin.x
                    && cursor.x < f.origin.x + f.size.width
                    && cursor.y >= f.origin.y
                    && cursor.y < f.origin.y + f.size.height
            })
            // Between screens (or the cursor is on a display that just went
            // away): the key window's screen is the best remaining guess.
            .or_else(|| {
                let main = NSScreen::mainScreen(nil);
                (main != nil).then_some(main)
            })
    }

    /// Centre the panel horizontally and put its top edge a fifth of the way
    /// down the screen the cursor is on.
    ///
    /// `visibleFrame` rather than `frame`, so the panel clears the menu bar and
    /// the Dock on whichever screen currently owns them.
    fn reposition(app: &AppHandle) -> Result<(), String> {
        let ns_window = main_window(app)?
            .ns_window()
            .map_err(|e| e.to_string())? as id;

        unsafe {
            let Some(screen) = screen_with_cursor() else {
                return Ok(());
            };

            let area = NSScreen::visibleFrame(screen);
            let panel_size = NSWindow::frame(ns_window).size;

            let x = area.origin.x + (area.size.width - panel_size.width) / 2.0;
            // Cocoa positions a window by its bottom-left corner, so convert the
            // top edge we actually care about into a baseline.
            let top = area.origin.y + area.size.height * 4.0 / 5.0;
            ns_window.setFrameOrigin_(NSPoint::new(x, top - panel_size.height));
        }

        Ok(())
    }

    pub fn show(app: &AppHandle) -> Result<(), String> {
        // `NSScreen` and window geometry are main-thread-only. The global
        // shortcut already fires there, and `run_on_main_thread` runs inline in
        // that case, so the move still happens before the panel is ordered in.
        let handle = app.clone();
        app.run_on_main_thread(move || {
            if let Err(err) = reposition(&handle) {
                log::error!("could not place the popup: {err}");
            }
            match panel(&handle) {
                Ok(panel) => panel.show(),
                Err(err) => log::error!("could not show the popup: {err}"),
            }
        })
        .map_err(|e| e.to_string())
    }

    pub fn hide(app: &AppHandle) -> Result<(), String> {
        panel(app)?.order_out(None);
        Ok(())
    }

    pub fn is_visible(app: &AppHandle) -> Result<bool, String> {
        Ok(panel(app)?.is_visible())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{main_window, reposition};
    use tauri::AppHandle;

    pub fn init(_app: &AppHandle) -> Result<(), String> {
        Ok(())
    }

    pub fn show(app: &AppHandle) -> Result<(), String> {
        let window = main_window(app)?;
        reposition(&window)?;
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())
    }

    pub fn hide(app: &AppHandle) -> Result<(), String> {
        main_window(app)?.hide().map_err(|e| e.to_string())
    }

    pub fn is_visible(app: &AppHandle) -> Result<bool, String> {
        main_window(app)?.is_visible().map_err(|e| e.to_string())
    }
}

pub use platform::{hide, init, is_visible};

/// Show the popup and tell the UI, so it can reset its selection and search box
/// rather than reappearing wherever the user left off.
pub fn show(app: &AppHandle) -> Result<(), String> {
    platform::show(app)?;
    let _ = app.emit("popup-shown", ());
    Ok(())
}

pub fn toggle(app: &AppHandle) -> Result<(), String> {
    if is_visible(app)? {
        hide(app)
    } else {
        show(app)
    }
}
