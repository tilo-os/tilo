//! The pages one compile laid out: their sizes, an identity for what each draws, their placed
//! glyphs, and the rasteriser a page's picture comes from.

use std::hash::{Hash, Hasher};

use image::ImageEncoder as _;
use typst::layout::{Frame, FrameItem};
use typst::utils::Scalar;
use typst_layout::Page;

use crate::text::{Cell, PageText};

/// One page as a window lays it out: its width and height in points, and an identity for what
/// it draws, so a window knows which of its pictures still stand after a new compile.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct PageSize {
    pub width: f64,
    pub height: f64,
    pub look: String,
}

/// A frame hashed by what it draws. Spans and introspection tags are left out: a compile
/// numbers spans afresh, so they differ for a page that looks the same.
struct Looks<'a>(&'a Frame);

impl Hash for Looks<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let frame = self.0;
        frame.size().hash(state);
        frame.kind().is_hard().hash(state);
        for (point, item) in frame.items() {
            match item {
                FrameItem::Group(group) => {
                    (0u8, point, group.transform, &group.clip).hash(state);
                    Looks(&group.frame).hash(state);
                }
                FrameItem::Text(text) => {
                    (1u8, point, &text.font, text.size, &text.fill, &text.stroke).hash(state);
                    for glyph in &text.glyphs {
                        (
                            glyph.id,
                            glyph.x_advance,
                            glyph.x_offset,
                            glyph.y_advance,
                            glyph.y_offset,
                        )
                            .hash(state);
                    }
                }
                FrameItem::Shape(shape, _) => (2u8, point, shape).hash(state),
                FrameItem::Image(image, size, _) => (3u8, point, image, size).hash(state),
                FrameItem::Link(..) | FrameItem::Tag(_) => {}
            }
        }
    }
}

/// The laid-out pages of one compile.
pub struct Pages {
    laid: Vec<Page>,
    sizes: Vec<PageSize>,
    text: Vec<PageText>,
}

impl Pages {
    pub fn new(pages: Vec<Page>) -> Self {
        let sizes = pages
            .iter()
            .map(|page| {
                let size = page.frame.size();
                PageSize {
                    width: size.x.to_pt(),
                    height: size.y.to_pt(),
                    look: format!(
                        "{:032x}",
                        typst::utils::hash128(&(Looks(&page.frame), &page.fill))
                    ),
                }
            })
            .collect();
        let text = pages
            .iter()
            .map(|page| PageText::of_frame(&page.frame))
            .collect();
        Self {
            laid: pages,
            sizes,
            text,
        }
    }

    pub fn len(&self) -> usize {
        self.laid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.laid.is_empty()
    }

    pub fn sizes(&self) -> &[PageSize] {
        &self.sizes
    }

    /// The placed glyphs of `page`, counted from 1.
    pub fn cells(&self, page: usize) -> Option<&[Cell]> {
        let page = self.text.get(page.checked_sub(1)?)?;
        Some(page.cells())
    }

    /// `page` drawn as a PNG `width` pixels wide, counted from 1.
    ///
    /// # Errors
    ///
    /// No such page, or the picture could not be encoded.
    pub fn png(&self, page: usize, width: u32) -> Result<Vec<u8>, String> {
        let page = page
            .checked_sub(1)
            .and_then(|page| self.laid.get(page))
            .ok_or_else(|| format!("the document has no page {page}"))?;
        let width = width.clamp(16, 8192);
        let width_pt = page.frame.size().x.to_pt();
        let width_pt = if width_pt > 0.0 { width_pt } else { 1.0 };
        let (pixels, drawn_w, drawn_h) = rasterise(page, f64::from(width) / width_pt)
            .ok_or_else(|| "the page could not be drawn".to_owned())?;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, drawn_w, drawn_h, image::ExtendedColorType::Rgba8)
            .map_err(|why| format!("the page's picture could not be encoded: {why}"))?;
        Ok(png)
    }
}

/// `page` rasterised at `pixel_per_pt`, composited over white so it reads as paper whatever the
/// document's page fill: its RGBA pixels and their size.
fn rasterise(page: &Page, pixel_per_pt: f64) -> Option<(Vec<u8>, u32, u32)> {
    if !(pixel_per_pt.is_finite() && pixel_per_pt > 0.0) {
        return None;
    }
    let options = typst_render::RenderOptions {
        pixel_per_pt: Scalar::new(pixel_per_pt),
        render_bleed: false,
    };
    let pixmap = typst_render::render(page, &options);
    let (width, height) = (pixmap.width(), pixmap.height());
    let mut rgba = pixmap.take();
    for px in rgba.chunks_exact_mut(4) {
        if let [r, g, b, a] = px {
            // Premultiplied colour over white: out = premul + (255 - a).
            let inv = 255 - *a;
            *r = r.saturating_add(inv);
            *g = g.saturating_add(inv);
            *b = b.saturating_add(inv);
            *a = 255;
        }
    }
    Some((rgba, width, height))
}
