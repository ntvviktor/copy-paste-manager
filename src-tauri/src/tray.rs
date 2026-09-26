//! The menu bar (tray) icon, and its menu of recent clipboard entries.
//!
//! The menu is rebuilt from scratch whenever the history changes rather than
//! patched in place: it is at most a couple of dozen rows, and rebuilding keeps
//! it trivially consistent with the store.

use crate::history::HistoryState;
use crate::panel;
use tauri::image::Image;
use tauri::menu::{IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

const TRAY_ID: &str = "clipvim";
/// Rows per section. The popup still holds the full history.
const MENU_TEXT_ITEMS: usize = 10;
const MENU_IMAGE_ITEMS: usize = 5;
/// Visible characters in a text row before it is cut with an ellipsis.
const LABEL_CHARS: usize = 48;
/// Menu ids for history rows are `entry:<history id>`.
const ENTRY_PREFIX: &str = "entry:";

pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("ClipVim")
        .menu(&menu)
        .on_menu_event(on_menu_event);

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;
    Ok(())
}

/// Replace the tray menu with one built from the current history.
///
/// Must run on the main thread; `crate::notify_history_changed` arranges that.
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    match build_menu(app) {
        Ok(menu) => {
            if let Err(err) = tray.set_menu(Some(menu)) {
                log::error!("replacing the tray menu failed: {err}");
            }
        }
        Err(err) => log::error!("building the tray menu failed: {err}"),
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    // Copy out what the rows need, then release the lock before touching any
    // menu API — those calls hop to the main thread and must never wait while
    // the history is locked.
    let (texts, images) = match app.state::<HistoryState>().0.lock() {
        Ok(history) => history.recent_for_menu(MENU_TEXT_ITEMS, MENU_IMAGE_ITEMS),
        Err(_) => (Vec::new(), Vec::new()),
    };
    let has_entries = !texts.is_empty() || !images.is_empty();

    let menu = Menu::new(app)?;

    if !has_entries {
        menu.append(&MenuItem::with_id(
            app,
            "empty",
            "Copy something to start a history",
            false,
            None::<&str>,
        )?)?;
    }

    if !texts.is_empty() {
        menu.append(&MenuItem::with_id(app, "header-text", "Text", false, None::<&str>)?)?;
        for entry in &texts {
            menu.append(&MenuItem::with_id(
                app,
                format!("{ENTRY_PREFIX}{}", entry.id),
                text_label(&entry.head),
                true,
                None::<&str>,
            )?)?;
        }
    }

    if !images.is_empty() {
        if !texts.is_empty() {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
        }
        menu.append(&MenuItem::with_id(app, "header-images", "Images", false, None::<&str>)?)?;
        for entry in &images {
            // A thumbnail that fails to decode still gets a clickable row.
            let icon = entry
                .thumb_png
                .as_deref()
                .and_then(|png| Image::from_bytes(png).ok());
            menu.append(&IconMenuItem::with_id(
                app,
                format!("{ENTRY_PREFIX}{}", entry.id),
                format!("{} × {}", entry.width, entry.height),
                true,
                icon,
                None::<&str>,
            )?)?;
        }
    }

    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "show", "Show Clipboard…", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "clear", "Clear History", has_entries, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit ClipVim", true, None::<&str>)?)?;

    Ok(menu)
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let id = event.id.as_ref();

    if let Some(entry_id) = id
        .strip_prefix(ENTRY_PREFIX)
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        if let Err(err) = crate::paste_entry_by_id(app, entry_id, crate::paste::MENU_SETTLE) {
            log::error!("pasting from the tray menu failed: {err}");
        }
        return;
    }

    match id {
        "show" => {
            if let Err(err) = panel::show(app) {
                log::error!("showing the popup failed: {err}");
            }
        }
        "clear" => {
            if let Ok(mut history) = app.state::<HistoryState>().0.lock() {
                history.clear();
            }
            // The guard is gone by here — notifying rebuilds this menu, which
            // locks the history again.
            crate::notify_history_changed(app);
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

/// One line, bounded, with `&` doubled: muda treats a lone `&` as a keyboard
/// mnemonic marker and deletes it, so "R&D" would otherwise show as "RD".
fn text_label(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut label: String = flat.chars().take(LABEL_CHARS).collect();
    if flat.chars().nth(LABEL_CHARS).is_some() {
        label.push('…');
    }
    label.replace('&', "&&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_doubles_ampersands_so_they_survive() {
        assert_eq!(text_label("R&D budget"), "R&&D budget");
    }

    #[test]
    fn label_flattens_whitespace_onto_one_line() {
        assert_eq!(text_label("  first\n\tsecond   third "), "first second third");
    }

    #[test]
    fn long_label_is_cut_with_an_ellipsis() {
        let label = text_label(&"z".repeat(LABEL_CHARS + 5));
        assert_eq!(label.chars().count(), LABEL_CHARS + 1);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn short_label_has_no_ellipsis() {
        assert_eq!(text_label("brief"), "brief");
    }
}
