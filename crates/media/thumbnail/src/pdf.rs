//! First-page rasterisation of a PDF (feature `pdf`) through Tilo's PDF
//! engine, which parses the document in its sandboxed helper process. When no
//! engine is available on the host the caller draws a badge instead.

use tilo_pdf::{Engine, RenderOptions, Rotation};

use crate::generate::{RgbaThumb, ThumbSize, rgba_to_thumb};

/// Whether PDF previews are available on this host (so a caller can decide
/// up front whether PDF previews are available or badge-only).
pub fn available() -> bool {
    Engine::shared().is_ok()
}

/// Rasterise the first page of `pdf` to an RGBA thumbnail that fits `size`.
/// `None` when no engine is available or the document is unreadable.
pub fn pdf_thumb(pdf: &[u8], size: impl Into<ThumbSize>) -> Option<RgbaThumb> {
    let size = size.into();
    let doc = Engine::shared().ok()?.open_bytes(pdf).ok()?;
    let page = doc.page_size(0)?;
    if !(page.width.is_finite() && page.height.is_finite() && page.width > 0.0 && page.height > 0.0)
    {
        return None;
    }
    // Render straight at the size that fits the box, so the helper never
    // rasterises more pixels than the preview keeps.
    #[allow(
        clippy::cast_precision_loss,
        reason = "thumbnail boxes are a few hundred pixels"
    )]
    let scale =
        (size.width.max(1) as f32 / page.width).min(size.height.max(1) as f32 / page.height);
    let options = RenderOptions {
        scale,
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
        size,
    )
}
