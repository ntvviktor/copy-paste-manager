//! Clipboard reading, writing, and the background change watcher.
//!
//! There is no cross-platform "clipboard changed" event, so this polls. On macOS
//! the poll is gated on `NSPasteboard.changeCount`, which is a single cheap
//! message send and allocates nothing — the clipboard is only actually read when
//! that counter moves. Other platforms fall back to comparing a content hash.

use crate::history::{self, Content, HistoryState};
use std::borrow::Cow;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// Poll interval. Low enough to feel instant, high enough to stay invisible in
/// Activity Monitor.
const POLL_MS: u64 = 400;

/// Signature of the last value *this app* put on the clipboard. The watcher
/// skips it, so pasting from the popup does not re-record the same entry.
static LAST_SELF_WRITE: Mutex<Option<u64>> = Mutex::new(None);

fn note_self_write(signature: u64) {
    if let Ok(mut guard) = LAST_SELF_WRITE.lock() {
        *guard = Some(signature);
    }
}

/// True if `signature` is the value we just wrote ourselves (consumes the flag).
fn take_self_write(signature: u64) -> bool {
    match LAST_SELF_WRITE.lock() {
        Ok(mut guard) if *guard == Some(signature) => {
            *guard = None;
            true
        }
        _ => false,
    }
}

pub fn spawn_watcher(app: AppHandle) {
    thread::spawn(move || {
        let mut board = match arboard::Clipboard::new() {
            Ok(board) => board,
            Err(err) => {
                log::error!("clipboard unavailable, history disabled: {err}");
                return;
            }
        };

        let mut last_signature: Option<u64> = None;
        #[cfg(target_os = "macos")]
        let mut last_change_count = mac_change_count();

        loop {
            thread::sleep(Duration::from_millis(POLL_MS));

            // Cheap gate: skip the read entirely when nothing has changed.
            #[cfg(target_os = "macos")]
            {
                let count = mac_change_count();
                if count == last_change_count {
                    continue;
                }
                last_change_count = count;
            }

            let Some(content) = read_clipboard(&mut board) else {
                continue;
            };

            let signature = content.signature();
            if last_signature == Some(signature) {
                continue;
            }
            last_signature = Some(signature);

            if take_self_write(signature) {
                continue;
            }

            let state = app.state::<HistoryState>();
            let inserted = match state.0.lock() {
                Ok(mut history) => {
                    history.insert(content);
                    true
                }
                Err(err) => {
                    log::error!("history lock poisoned: {err}");
                    false
                }
            };

            if inserted {
                log::debug!("recorded a new clipboard entry");
                crate::notify_history_changed(&app);
            }
        }
    });
}

/// Read the richest representation currently on the clipboard.
///
/// Images win over text: copying a picture in a browser often puts both the
/// image and its source URL on the clipboard, and the image is what was meant.
fn read_clipboard(board: &mut arboard::Clipboard) -> Option<Content> {
    if let Ok(image) = board.get_image() {
        let width = u32::try_from(image.width).ok()?;
        let height = u32::try_from(image.height).ok()?;
        if let Some(entry) = history::image_entry_from_rgba(width, height, &image.bytes) {
            return Some(Content::Image(entry));
        }
    }

    match board.get_text() {
        Ok(text) if !text.trim().is_empty() => Some(Content::Text(text)),
        _ => None,
    }
}

/// Put a history entry back on the system clipboard.
pub fn write_content(content: &Content) -> Result<(), String> {
    let mut board = arboard::Clipboard::new().map_err(|err| err.to_string())?;

    // Record before writing: the watcher can observe the change the instant the
    // write lands, so the flag has to already be set.
    note_self_write(content.signature());

    let result = match content {
        Content::Text(text) => board.set_text(text.clone()),
        Content::Image(img) => {
            let rgba = image::load_from_memory_with_format(&img.png, image::ImageFormat::Png)
                .map_err(|err| format!("decoding stored image failed: {err}"))?
                .to_rgba8();
            board.set_image(arboard::ImageData {
                width: img.width as usize,
                height: img.height as usize,
                bytes: Cow::Owned(rgba.into_raw()),
            })
        }
    };

    result.map_err(|err| format!("writing to clipboard failed: {err}"))
}

#[cfg(target_os = "macos")]
fn mac_change_count() -> i64 {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let pasteboard: *mut Object = msg_send![class!(NSPasteboard), generalPasteboard];
        if pasteboard.is_null() {
            return -1;
        }
        msg_send![pasteboard, changeCount]
    }
}
