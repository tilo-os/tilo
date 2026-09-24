//! First-page rasterisation of a PDF (feature `pdf`) through Tilo's PDF
//! engine, which parses the document in its sandboxed helper process. When no
//! engine is available on the host the caller draws a badge instead.

use tilo_pdf::{Engine, RenderOptions, Rotation};

use crate::generate::{RgbaThumb, rgba_to_thumb};

/// Whether PDF previews are available on this host (so a caller can decide
/// up front whether PDF previews are available or badge-only).
pub fn available() -> bool {
    Engine::shared().is_ok()
}

/// Rasterise the first page of `pdf` to an RGBA thumbnail whose longest edge
/// is at most `max_edge`. `None` when no engine is available or the document
/// is unreadable.
pub fn pdf_thumb(pdf: &[u8], max_edge: u32) -> Option<RgbaThumb> {
    let doc = Engine::shared().ok()?.open_bytes(pdf).ok()?;
    let size = doc.page_size(0)?;
    let longest = size.width.max(size.height);
    if !(longest.is_finite() && longest > 0.0) {
        return None;
    }
    let options = RenderOptions {
        #[allow(
            clippy::cast_precision_loss,
            reason = "thumbnail edges are a few hundred pixels"
        )]
        scale: max_edge as f32 / longest,
        rotation: Rotation::None,
        region: None,
        annotations: true,
        reader_markup: true,
    };
    let bitmap = doc.render(0, &options).ok()?;
    rgba_to_thumb(
        bitmap.pixels().to_vec(),
        bitmap.width(),
        bitmap.height(),
        max_edge,
    )
}
