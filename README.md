# ClipVim

A clipboard history popup you drive with vim motions. Summon it with a global
shortcut, move with `j`/`k`, hit `⏎`, and the entry is pasted into whatever app
you were already typing in.

Built on Tauri v2 — Rust core, web UI, ~15 MB binary.

```
⌘⇧V          summon / dismiss the popup   (⌃⇧V on Windows and Linux)
j k ↓ ↑ ⌃n ⌃p   move
gg G         first / last
⌃d ⌃u        half page
1–9          jump to entry
⏎            paste into the app underneath
y            copy without pasting
d x          delete entry
/            search (esc clears)
⇥ ⇧⇥         cycle filter: all → text → images
?            help
esc          close
```

## Running it

```sh
npm install
npm run tauri dev
```

The app has no Dock icon — it lives in the menu bar. Clicking the menu bar icon
lists your 10 most recent text entries and 5 most recent images (with
thumbnails); click one to paste it. **Show Clipboard…**, **Clear History** and
**Quit** are there too. The window starts hidden; press the
shortcut.

To build a release `.app` and `.dmg`:

```sh
npm run tauri build
```

## macOS: grant Accessibility permission

Pasting into another app means synthesising a `⌘V` keystroke, which macOS gates
behind **System Settings → Privacy & Security → Accessibility**. Without it the
popup still works and still copies — the keystroke is just silently swallowed.

The app detects this and shows a banner with a button that opens the right pane.
In development you grant the permission to your **terminal**, because that is the
process tree sending the events; a bundled `.app` asks for itself.

## Tests

```sh
cd src-tauri && cargo test
```

11 tests cover the store: dedup and move-to-front, cap eviction, id stability
across `touch`, text truncation, thumbnail downscaling, and rejection of
degenerate or oversized images.

Not covered by tests, because they need a real keypress on a real desktop:
the global hotkey firing, the panel showing without stealing focus, and the
paste keystroke landing. Verify those by hand.

## How it works

Four platform problems, and where each is solved:

| Problem | Where |
|---|---|
| Global hotkey | `tauri-plugin-global-shortcut`, registered in `lib.rs` |
| A window that doesn't steal focus | `src-tauri/src/panel.rs` |
| Watching the clipboard | `src-tauri/src/clipboard.rs` |
| Pasting back | `src-tauri/src/paste.rs` |

**The non-activating panel is the load-bearing trick.** On macOS the window is
swizzled into an `NSPanel` with `NSWindowStyleMaskNonactivatingPanel`
(`panel.rs`). That lets it receive keystrokes *without activating ClipVim*, so
the app you were typing in never loses focus and is still there to receive the
paste. A normal window would steal focus and there would be nothing to paste
into. It also joins all Spaces and floats over full-screen apps.

Because the app is never active, a natively focused `<input>` is not a reliable
place to collect keystrokes. So `src/main.ts` owns the entire keymap in one
global `keydown` listener — search typing included, with the search box readonly
and used only for display.


**The paste keystroke must be synthesised on the main thread.** `enigo` resolves
`'v'` to a physical keycode through HIToolbox's Text Services Manager (so ⌘V
works on Dvorak and AZERTY, not just QWERTY), and TSM asserts it is on the main
dispatch queue — calling it from a spawned thread traps with SIGTRAP and kills
the process. `paste_entry` therefore waits out the settle delay on a spawned
thread but marshals the keystroke itself back via `run_on_main_thread`. Note this
only bites once Accessibility permission is granted; without it `Enigo::new()`
fails first and you never reach the trap.

**There is no clipboard-changed event on any platform**, so `clipboard.rs`
polls every 400 ms. On macOS the poll is gated on `NSPasteboard.changeCount`, a
single cheap message send that allocates nothing; the clipboard is only actually
read when that counter moves. Other platforms compare a content hash.

Entries are deduplicated by an FNV-1a signature, cached per entry. Writes the app
makes itself are tagged so pasting does not re-record the same entry.

**History is in-memory only and dies with the process** — nothing is written to
disk. Capped at 200 entries and 256 MB, oldest evicted first. Images are stored
as PNG rather than raw RGBA, since one 4K screenshot is ~33 MB uncompressed, with
a 160 px thumbnail cached as a `data:` URL for the list.

## Cross-platform status

The Rust core (`arboard`, `enigo`, `global-shortcut`) is cross-platform, and
non-macOS builds fall back to a plain always-on-top window in `panel.rs`.

- **macOS** — the tested path.
- **Windows** — should work; an always-on-top window does take focus, so
  paste-back needs testing and may need a `SetForegroundWindow` dance.
- **Linux/X11** — should work.
- **Linux/Wayland** — expect trouble. Wayland deliberately restricts clipboard
  reads by unfocused apps and global hotkey registration. Also note `arboard` on
  X11 only owns the clipboard while the process lives.

## Layout

```
src/                    frontend (vanilla TS + Vite, no framework)
  main.ts               vim keymap, rendering, IPC
  types.ts              mirrors the Rust DTOs
  style.css
src-tauri/src/
  lib.rs                commands, shortcut registration, setup
  tray.rs               menu bar icon and its menu of recent entries
  panel.rs              NSPanel swizzling, positioning, show/hide
  clipboard.rs          change watcher, clipboard read/write
  history.rs            in-memory store, dedup, thumbnails
  paste.rs              keystroke synthesis, Accessibility check
```

## Things left undone

- The shortcut is hardcoded in `popup_shortcut()` — no settings UI.
- Text preview shows the first 280 chars; the full body is never sent to the
  frontend.
- No pinned/favourite entries, no persistence, no excluded-app list (a password
  manager's clipboard *will* land in the history).
