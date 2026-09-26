//! ClipVim — a vim-navigable clipboard history popup.

mod clipboard;
mod history;
mod panel;
mod paste;
mod tray;

use history::{EntryView, HistoryState};
use serde::Serialize;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Locks the history, mapping poisoning to a reportable error.
macro_rules! history {
    ($state:expr) => {
        $state
            .0
            .lock()
            .map_err(|err| format!("history lock poisoned: {err}"))
    };
}

/// Tell every view of the history that it changed: the popup's webview and the
/// tray menu.
///
/// Never call this while holding the history lock. On the main thread,
/// `run_on_main_thread` runs the menu rebuild inline instead of queueing it, and
/// the rebuild takes that same lock — a std `Mutex` is not reentrant, so it would
/// deadlock.
fn notify_history_changed(app: &AppHandle) {
    let _ = app.emit("history-changed", ());
    let handle = app.clone();
    if let Err(err) = app.run_on_main_thread(move || tray::refresh(&handle)) {
        log::error!("could not schedule a tray menu refresh: {err}");
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionStatus {
    /// macOS Accessibility permission. Without it, paste-back silently fails.
    accessibility: bool,
    /// Whether this platform needs that permission at all.
    required: bool,
}

#[tauri::command]
fn list_history(state: State<HistoryState>) -> Result<Vec<EntryView>, String> {
    Ok(history!(state)?.views())
}

/// Paste an entry chosen in the popup.
#[tauri::command]
fn paste_entry(app: AppHandle, id: u64) -> Result<(), String> {
    paste_entry_by_id(&app, id, paste::FOCUS_SETTLE)
}

/// Put an entry on the clipboard, dismiss the popup, and paste into the app
/// underneath. Shared by the popup and the tray menu, which differ only in how
/// long they wait before sending the keystroke.
fn paste_entry_by_id(app: &AppHandle, id: u64, settle: Duration) -> Result<(), String> {
    let state = app.state::<HistoryState>();
    let content = history!(state)?
        .content_of(id)
        .ok_or("that entry is no longer in the history")?;

    clipboard::write_content(&content)?;
    history!(state)?.touch(id);

    // Dismiss first: the keystroke must land in the app underneath, not here.
    panel::hide(app)?;
    notify_history_changed(app);

    // The wait happens off-thread so the popup closes immediately, but the
    // keystroke itself must be synthesised ON the main thread: enigo's macOS
    // backend resolves 'v' to a physical keycode through HIToolbox's Text
    // Services Manager, which asserts it is on the main dispatch queue and
    // traps with SIGTRAP if it is not — taking the whole process down.
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(settle);
        let dispatched = app.run_on_main_thread(|| {
            if let Err(err) = paste::send_paste_shortcut() {
                log::error!("paste keystroke failed: {err}");
            }
        });
        if let Err(err) = dispatched {
            log::error!("could not reach the main thread to paste: {err}");
        }
    });

    Ok(())
}

/// Copy to the clipboard without pasting — vim's `y`.
#[tauri::command]
fn copy_entry(app: AppHandle, state: State<HistoryState>, id: u64) -> Result<(), String> {
    let content = history!(state)?
        .content_of(id)
        .ok_or("that entry is no longer in the history")?;

    clipboard::write_content(&content)?;
    history!(state)?.touch(id);
    panel::hide(&app)?;
    notify_history_changed(&app);
    Ok(())
}

#[tauri::command]
fn delete_entry(app: AppHandle, state: State<HistoryState>, id: u64) -> Result<bool, String> {
    let removed = history!(state)?.remove(id);
    if removed {
        notify_history_changed(&app);
    }
    Ok(removed)
}

#[tauri::command]
fn clear_history(app: AppHandle, state: State<HistoryState>) -> Result<(), String> {
    history!(state)?.clear();
    notify_history_changed(&app);
    Ok(())
}

#[tauri::command]
fn hide_popup(app: AppHandle) -> Result<(), String> {
    panel::hide(&app)
}

#[tauri::command]
fn permission_status() -> PermissionStatus {
    PermissionStatus {
        accessibility: paste::accessibility_granted(),
        required: cfg!(target_os = "macos"),
    }
}

#[tauri::command]
fn open_accessibility_settings() -> Result<(), String> {
    paste::open_accessibility_settings()
}

/// `Cmd+Shift+V` on macOS, `Ctrl+Shift+V` elsewhere.
fn popup_shortcut() -> Shortcut {
    #[cfg(target_os = "macos")]
    let modifiers = Modifiers::SUPER | Modifiers::SHIFT;
    #[cfg(not(target_os = "macos"))]
    let modifiers = Modifiers::CONTROL | Modifiers::SHIFT;

    Shortcut::new(Some(modifiers), Code::KeyV)
}

pub fn run() {
    let builder = tauri::Builder::default();

    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    builder
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(HistoryState::default())
        .invoke_handler(tauri::generate_handler![
            list_history,
            paste_entry,
            copy_entry,
            delete_entry,
            clear_history,
            hide_popup,
            permission_status,
            open_accessibility_settings,
        ])
        .setup(|app| {
            // Agent app: no Dock icon, no menu bar. Reinforces Info.plist's
            // LSUIElement and also covers `tauri dev`.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();

            panel::init(&handle)?;
            tray::init(&handle)?;
            clipboard::spawn_watcher(handle.clone());

            let shortcut = popup_shortcut();
            app.global_shortcut()
                .on_shortcut(shortcut, move |app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        if let Err(err) = panel::toggle(app) {
                            log::error!("toggling the popup failed: {err}");
                        }
                    }
                })?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running ClipVim");
}
