//! Synthesising the paste keystroke into whichever app was focused.
//!
//! Because the popup is a non-activating panel, the previously focused app never
//! lost focus — so there is nobody to restore. Hide the panel, put the entry on
//! the clipboard, and send the platform paste chord.
//!
//! On macOS this requires the user to grant Accessibility permission; without it
//! the keystroke is silently swallowed, which is why `accessibility_granted()`
//! exists and is surfaced in the UI.

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::time::Duration;

/// Grace period after hiding the panel before the keystroke is sent, so the
/// target app is settled and ready to receive it.
pub const FOCUS_SETTLE: Duration = Duration::from_millis(60);

/// Settle time when pasting from the tray menu. Deliberately more generous than
/// the popup's: the click arrives while the status-bar menu is being dismissed,
/// and that hand-back of focus has not been timed.
pub const MENU_SETTLE: Duration = Duration::from_millis(150);

pub fn send_paste_shortcut() -> Result<(), String> {
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|err| format!("could not open an input device: {err}"))?;

    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    enigo
        .key(modifier, Direction::Press)
        .map_err(|err| format!("pressing the modifier failed: {err}"))?;
    let typed = enigo.key(Key::Unicode('v'), Direction::Click);
    // Always release the modifier, even if the keypress failed, so the user is
    // not left with a stuck Command key.
    let released = enigo.key(modifier, Direction::Release);

    typed.map_err(|err| format!("sending the paste key failed: {err}"))?;
    released.map_err(|err| format!("releasing the modifier failed: {err}"))?;
    Ok(())
}

/// Whether this process may synthesise input events.
#[cfg(target_os = "macos")]
pub fn accessibility_granted() -> bool {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }
    unsafe { AXIsProcessTrusted() != 0 }
}

#[cfg(not(target_os = "macos"))]
pub fn accessibility_granted() -> bool {
    true
}

/// Open the settings pane where the user grants that permission.
#[cfg(target_os = "macos")]
pub fn open_accessibility_settings() -> Result<(), String> {
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("could not open System Settings: {err}"))
}

#[cfg(not(target_os = "macos"))]
pub fn open_accessibility_settings() -> Result<(), String> {
    Ok(())
}
