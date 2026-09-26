//! In-memory, non-persistent clipboard history.
//!
//! Nothing here touches disk: the history lives for the lifetime of the process
//! and disappears on quit. Images are kept as PNG (compressed) rather than raw
//! RGBA, because a single 4K screenshot is ~33 MB uncompressed.

use base64::{engine::general_purpose::STANDARD, Engine};
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Hard cap on entries.
pub const MAX_ITEMS: usize = 200;
/// Soft cap on retained image bytes; oldest images are evicted past this.
const MAX_IMAGE_BYTES: usize = 256 * 1024 * 1024;
/// Images larger than this are ignored rather than retained.
const MAX_PIXELS: u64 = 40_000_000;
/// How much text the frontend list receives per entry.
const PREVIEW_CHARS: usize = 280;
/// Longest edge of the list thumbnail.
const THUMB_MAX_EDGE: u32 = 160;
/// Prefix of every stored thumbnail URL.
const DATA_URL_PREFIX: &str = "data:image/png;base64,";
/// How much of a text entry the tray menu receives; it shortens it further.
const MENU_TEXT_HEAD_CHARS: usize = 200;

#[derive(Clone)]
pub struct ImageEntry {
    pub width: u32,
    pub height: u32,
    /// Full-resolution PNG, used for paste-back.
    pub png: Vec<u8>,
    /// Downscaled PNG as a `data:` URL, built once at capture time because the
    /// frontend re-reads the whole list on every clipboard change.
    pub thumb_data_url: String,
}

#[derive(Clone)]
pub enum Content {
    Text(String),
    Image(ImageEntry),
}

impl Content {
    /// Cheap content fingerprint (FNV-1a) used for dedup and for recognising
    /// clipboard writes this app made itself.
    pub fn signature(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |bytes: &[u8]| {
            for b in bytes {
                hash ^= *b as u64;
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        };
        match self {
            Content::Text(t) => {
                feed(&[0x01]);
                feed(t.as_bytes());
            }
            Content::Image(img) => {
                feed(&[0x02]);
                feed(&img.png);
            }
        }
        hash
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Content::Text(t) => t.len(),
            Content::Image(img) => img.png.len() + img.thumb_data_url.len(),
        }
    }
}

pub struct Entry {
    pub id: u64,
    pub created_at: u128,
    /// Cached `content.signature()`. Recomputing it per entry on every insert
    /// would mean rehashing every retained image on every copy.
    signature: u64,
    pub content: Content,
}

/// What the frontend actually receives. Deliberately excludes full-resolution
/// image bytes and full text bodies so that listing 200 entries stays cheap.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryView {
    pub id: u64,
    pub created_at: u128,
    pub kind: &'static str,
    pub preview: String,
    pub truncated: bool,
    pub line_count: usize,
    pub char_count: usize,
    /// `data:image/png;base64,…` thumbnail, images only.
    pub thumb: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub byte_len: usize,
}

/// One text row in the tray menu.
pub struct MenuText {
    pub id: u64,
    /// The start of the text, bounded to `MENU_TEXT_HEAD_CHARS`.
    pub head: String,
}

/// One image row in the tray menu.
pub struct MenuImage {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    /// The list thumbnail as PNG bytes, used as the menu item's icon.
    pub thumb_png: Option<Vec<u8>>,
}

#[derive(Default)]
pub struct History {
    items: VecDeque<Entry>,
    next_id: u64,
}

impl History {
    /// Record new clipboard content at the front.
    ///
    /// If identical content is already present it is moved to the front instead
    /// of duplicated. Returns the entry id, and whether it was newly created.
    pub fn insert(&mut self, content: Content) -> (u64, bool) {
        let sig = content.signature();
        if let Some(pos) = self.items.iter().position(|e| e.signature == sig) {
            let mut existing = self.items.remove(pos).expect("position just found");
            existing.created_at = now_ms();
            let id = existing.id;
            self.items.push_front(existing);
            return (id, false);
        }

        self.next_id += 1;
        let id = self.next_id;
        self.items.push_front(Entry {
            id,
            created_at: now_ms(),
            signature: sig,
            content,
        });
        self.enforce_limits();
        (id, true)
    }

    fn enforce_limits(&mut self) {
        while self.items.len() > MAX_ITEMS {
            self.items.pop_back();
        }
        let mut total: usize = self.items.iter().map(|e| e.content.retained_bytes()).sum();
        while total > MAX_IMAGE_BYTES && self.items.len() > 1 {
            if let Some(dropped) = self.items.pop_back() {
                total -= dropped.content.retained_bytes();
            }
        }
    }

    /// The newest entries of each kind, for the tray menu. Copies out only what
    /// a menu row needs, so callers hold the lock as briefly as possible.
    pub fn recent_for_menu(
        &self,
        text_limit: usize,
        image_limit: usize,
    ) -> (Vec<MenuText>, Vec<MenuImage>) {
        let mut texts = Vec::new();
        let mut images = Vec::new();

        for entry in &self.items {
            if texts.len() >= text_limit && images.len() >= image_limit {
                break;
            }
            match &entry.content {
                Content::Text(text) if texts.len() < text_limit => texts.push(MenuText {
                    id: entry.id,
                    head: text.trim().chars().take(MENU_TEXT_HEAD_CHARS).collect(),
                }),
                Content::Image(img) if images.len() < image_limit => images.push(MenuImage {
                    id: entry.id,
                    width: img.width,
                    height: img.height,
                    thumb_png: img
                        .thumb_data_url
                        .strip_prefix(DATA_URL_PREFIX)
                        .and_then(|encoded| STANDARD.decode(encoded).ok()),
                }),
                _ => {}
            }
        }

        (texts, images)
    }

    pub fn views(&self) -> Vec<EntryView> {
        self.items.iter().map(view_of).collect()
    }

    pub fn content_of(&self, id: u64) -> Option<Content> {
        self.items
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.content.clone())
    }

    /// Move an entry to the front without changing its id — used after pasting,
    /// so the most recently used entry is the first one next time.
    pub fn touch(&mut self, id: u64) {
        if let Some(pos) = self.items.iter().position(|e| e.id == id) {
            if pos == 0 {
                return;
            }
            if let Some(mut entry) = self.items.remove(pos) {
                entry.created_at = now_ms();
                self.items.push_front(entry);
            }
        }
    }

    pub fn remove(&mut self, id: u64) -> bool {
        if let Some(pos) = self.items.iter().position(|e| e.id == id) {
            self.items.remove(pos);
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// Tauri managed state wrapper.
pub struct HistoryState(pub Mutex<History>);

impl Default for HistoryState {
    fn default() -> Self {
        Self(Mutex::new(History::default()))
    }
}

fn view_of(entry: &Entry) -> EntryView {
    match &entry.content {
        Content::Text(text) => {
            let trimmed = text.trim();
            let preview: String = trimmed.chars().take(PREVIEW_CHARS).collect();
            let truncated = trimmed.chars().nth(PREVIEW_CHARS).is_some();
            EntryView {
                id: entry.id,
                created_at: entry.created_at,
                kind: "text",
                preview,
                truncated,
                line_count: text.lines().count().max(1),
                char_count: text.chars().count(),
                thumb: None,
                width: None,
                height: None,
                byte_len: text.len(),
            }
        }
        Content::Image(img) => EntryView {
            id: entry.id,
            created_at: entry.created_at,
            kind: "image",
            preview: format!("{} × {}", img.width, img.height),
            truncated: false,
            line_count: 1,
            char_count: 0,
            thumb: Some(img.thumb_data_url.clone()),
            width: Some(img.width),
            height: Some(img.height),
            byte_len: img.png.len(),
        },
    }
}

/// Build a storable image entry from raw RGBA, as handed over by the clipboard.
/// Returns `None` for images that are empty, malformed, or absurdly large.
pub fn image_entry_from_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<ImageEntry> {
    if width == 0 || height == 0 {
        return None;
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let full = RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let png = encode_png(&full)?;

    let scale = f64::from(THUMB_MAX_EDGE) / f64::from(width.max(height));
    let thumb_png = if scale >= 1.0 {
        png.clone()
    } else {
        let tw = ((f64::from(width) * scale).round() as u32).max(1);
        let th = ((f64::from(height) * scale).round() as u32).max(1);
        encode_png(&image::imageops::thumbnail(&full, tw, th))?
    };

    Some(ImageEntry {
        width,
        height,
        png,
        thumb_data_url: format!("{DATA_URL_PREFIX}{}", STANDARD.encode(&thumb_png)),
    })
}

fn encode_png(img: &RgbaImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(img.as_raw(), img.width(), img.height(), ExtendedColorType::Rgba8)
        .ok()?;
    Some(out)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(body: &str) -> Content {
        Content::Text(body.to_string())
    }

    /// Solid-colour RGBA buffer, so tests do not depend on fixture files.
    fn rgba(width: u32, height: u32, value: u8) -> Vec<u8> {
        vec![value; (width as usize) * (height as usize) * 4]
    }

    #[test]
    fn records_entries_newest_first() {
        let mut history = History::default();
        history.insert(text("first"));
        history.insert(text("second"));

        let views = history.views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].preview, "second");
        assert_eq!(views[1].preview, "first");
    }

    #[test]
    fn recopying_moves_to_front_without_duplicating() {
        let mut history = History::default();
        let (first_id, created) = history.insert(text("alpha"));
        assert!(created);
        history.insert(text("beta"));

        let (again_id, created) = history.insert(text("alpha"));
        assert!(!created, "an identical copy must not create a second entry");
        assert_eq!(again_id, first_id, "the original entry id is reused");

        let views = history.views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].preview, "alpha");
    }

    #[test]
    fn text_and_image_of_the_same_bytes_are_distinct() {
        // The signature is discriminated by kind, so a text entry cannot collide
        // with an image entry.
        let image = image_entry_from_rgba(2, 2, &rgba(2, 2, 0x41)).expect("valid image");
        let as_text = Content::Text(String::from_utf8_lossy(&image.png).to_string());
        assert_ne!(Content::Image(image).signature(), as_text.signature());
    }

    #[test]
    fn evicts_oldest_past_the_cap() {
        let mut history = History::default();
        for index in 0..(MAX_ITEMS + 10) {
            history.insert(text(&format!("entry {index}")));
        }

        let views = history.views();
        assert_eq!(views.len(), MAX_ITEMS);
        assert_eq!(views[0].preview, format!("entry {}", MAX_ITEMS + 9));
        assert_eq!(views[MAX_ITEMS - 1].preview, "entry 10");
    }

    #[test]
    fn touch_promotes_without_changing_the_id() {
        let mut history = History::default();
        let (target, _) = history.insert(text("wanted"));
        history.insert(text("noise"));

        history.touch(target);
        let views = history.views();
        assert_eq!(views[0].id, target);
        assert_eq!(views[0].preview, "wanted");
    }

    #[test]
    fn remove_reports_whether_anything_went() {
        let mut history = History::default();
        let (id, _) = history.insert(text("doomed"));
        assert!(history.remove(id));
        assert!(!history.remove(id), "removing twice is not an error, just false");
        assert!(history.views().is_empty());
    }

    #[test]
    fn long_text_is_truncated_for_the_list_but_counted_in_full() {
        let body = "x".repeat(PREVIEW_CHARS + 50);
        let mut history = History::default();
        history.insert(text(&body));

        let view = &history.views()[0];
        assert!(view.truncated);
        assert_eq!(view.preview.chars().count(), PREVIEW_CHARS);
        assert_eq!(view.char_count, PREVIEW_CHARS + 50);
    }

    #[test]
    fn short_text_is_not_marked_truncated() {
        let mut history = History::default();
        history.insert(text("brief"));
        assert!(!history.views()[0].truncated);
    }

    #[test]
    fn image_entry_keeps_dimensions_and_builds_a_data_url() {
        let entry = image_entry_from_rgba(8, 4, &rgba(8, 4, 0x7f)).expect("valid image");
        assert_eq!((entry.width, entry.height), (8, 4));
        assert!(entry.thumb_data_url.starts_with("data:image/png;base64,"));
        assert!(!entry.png.is_empty());

        let mut history = History::default();
        history.insert(Content::Image(entry));
        let view = &history.views()[0];
        assert_eq!(view.kind, "image");
        assert_eq!(view.preview, "8 × 4");
        assert_eq!((view.width, view.height), (Some(8), Some(4)));
    }

    #[test]
    fn large_images_are_downscaled_for_the_thumbnail() {
        let edge = THUMB_MAX_EDGE * 3;
        let entry = image_entry_from_rgba(edge, edge, &rgba(edge, edge, 0x10)).expect("valid");
        // A real downscale happened, so the thumbnail is not just the full PNG.
        let full_as_url_len = STANDARD.encode(&entry.png).len();
        assert!(entry.thumb_data_url.len() < full_as_url_len);
    }

    #[test]
    fn rejects_degenerate_and_oversized_images() {
        assert!(image_entry_from_rgba(0, 10, &[]).is_none());
        assert!(image_entry_from_rgba(10, 0, &[]).is_none());
        // Claims more pixels than the buffer holds.
        assert!(image_entry_from_rgba(10, 10, &rgba(2, 2, 0)).is_none());
        // Past MAX_PIXELS, rejected before any allocation.
        assert!(image_entry_from_rgba(40_000, 40_000, &[]).is_none());
    }

    #[test]
    fn menu_takes_the_newest_of_each_kind_up_to_its_limit() {
        let mut history = History::default();
        for index in 0..5 {
            history.insert(text(&format!("text {index}")));
        }
        for shade in 0..3u8 {
            let image = image_entry_from_rgba(4, 4, &rgba(4, 4, shade)).expect("valid image");
            history.insert(Content::Image(image));
        }

        let (texts, images) = history.recent_for_menu(2, 10);
        let heads: Vec<&str> = texts.iter().map(|t| t.head.as_str()).collect();
        assert_eq!(heads, ["text 4", "text 3"]);
        assert_eq!(images.len(), 3, "fewer images than the limit means all of them");
    }

    #[test]
    fn menu_images_carry_a_decodable_png_thumbnail() {
        let mut history = History::default();
        let image = image_entry_from_rgba(8, 4, &rgba(8, 4, 9)).expect("valid image");
        history.insert(Content::Image(image));

        let (_, images) = history.recent_for_menu(0, 1);
        let png = images[0].thumb_png.as_deref().expect("thumbnail decodes from its data URL");
        assert!(png.starts_with(b"\x89PNG"));
        assert_eq!((images[0].width, images[0].height), (8, 4));
    }

    #[test]
    fn menu_text_head_is_bounded() {
        let mut history = History::default();
        history.insert(text(&"y".repeat(10_000)));

        let (texts, _) = history.recent_for_menu(1, 0);
        assert_eq!(texts[0].head.chars().count(), MENU_TEXT_HEAD_CHARS);
    }
}
