//! What the renderer offers a host about the text it paints: whether every glyph run
//! carries the characters it sets. A canvas never reads them — its glyphs are outlines —
//! so they are kept only while a host asks: a page that builds HTML from the frame needs
//! the words to make real text, selectable and readable aloud.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static PRESENT_TEXT: AtomicBool = AtomicBool::new(false);

static PRESENTATION_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Have every glyph run this renderer paints carry the characters it sets
/// (`vello::DrawGlyphs::text`), for a host that presents text as text — a page that
/// builds DOM from the scene — instead of as outlines. Off by default: a GPU host never
/// reads them.
pub fn present_text(on: bool) {
    if PRESENT_TEXT.swap(on, Ordering::Relaxed) != on {
        PRESENTATION_EPOCH.fetch_add(1, Ordering::Relaxed);
    }
}

/// Whether runs keep the characters they set.
pub(super) fn presents_text() -> bool {
    PRESENT_TEXT.load(Ordering::Relaxed)
}

/// The offer as it stands now: a renderer that shaped at a different epoch shaped under
/// the other offer.
pub(super) fn epoch() -> u64 {
    PRESENTATION_EPOCH.load(Ordering::Relaxed)
}
