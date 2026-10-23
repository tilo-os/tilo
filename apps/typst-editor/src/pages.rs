//! A compiled document's pages as a page source: the preview asks the shared
//! page renderer ([`tilo_page_raster`]) for the pages and tiles it shows, and
//! the renderer rasterises them from here, below the UI thread.

use std::hash::{Hash, Hasher};

use typst::foundations::Content;
use typst::layout::{Abs, Frame, FrameItem, Point, Sides, Size};
use typst::utils::Scalar;
use typst_layout::Page;
use vello::peniko::ImageData;

use tilo_page_raster::PageSource;

use crate::page_text::PageText;
use tilo_page_raster::tiles::{self, TileKey};

/// A frame hashed by what it draws. Spans and introspection tags are left
/// out: a compile numbers spans afresh, so they differ for a page that looks
/// the same.
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
pub struct TypstPages {
    pages: Vec<Page>,
    /// Page sizes in points.
    sizes: Vec<(f64, f64)>,
    /// What each page looks like, as a hash of what it draws: equal for a
    /// page an edit left as it was.
    looks: Vec<u128>,
    /// Each page's selectable text.
    text: Vec<PageText>,
}

impl TypstPages {
    pub fn new(pages: Vec<Page>) -> Self {
        let sizes = pages
            .iter()
            .map(|page| {
                let size = page.frame.size();
                (size.x.to_pt(), size.y.to_pt())
            })
            .collect();
        let looks = pages
            .iter()
            .map(|page| typst::utils::hash128(&(Looks(&page.frame), &page.fill)))
            .collect();
        let text = pages
            .iter()
            .map(|page| PageText::of_frame(&page.frame))
            .collect();
        Self {
            pages,
            sizes,
            looks,
            text,
        }
    }

    /// These pages with `text` as their selectable text.
    #[cfg(test)]
    #[must_use]
    pub fn with_text(mut self, text: Vec<PageText>) -> Self {
        self.text = text;
        self
    }

    /// Blank pages of `sizes` (in points).
    #[cfg(test)]
    pub fn blank(sizes: &[(f64, f64)]) -> Self {
        Self::new(
            sizes
                .iter()
                .map(|(w, h)| Page {
                    frame: Frame::hard(Size::new(Abs::pt(*w), Abs::pt(*h))),
                    bleed: Sides::default(),
                    fill: typst::foundations::Smart::Auto,
                    numbering: None,
                    supplement: Content::empty(),
                    number: 1,
                })
                .collect(),
        )
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn sizes(&self) -> &[(f64, f64)] {
        &self.sizes
    }

    pub fn looks(&self) -> &[u128] {
        &self.looks
    }

    /// The selectable text of `page`; none past the last page.
    pub fn text(&self, page: usize) -> &PageText {
        static NONE: PageText = PageText::EMPTY;
        self.text.get(page).unwrap_or(&NONE)
    }
}

/// `page` rasterised at `pixel_per_pt`, composited over white so it reads
/// as paper whatever the document's page fill.
fn rasterise(page: &Page, pixel_per_pt: f64) -> Option<ImageData> {
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
    Some(ui::raster::rgba8(rgba, width, height))
}

impl PageSource for TypstPages {
    fn render_page(&self, page: usize, width: u32) -> Option<ImageData> {
        let (page, (width_pt, _)) = (self.pages.get(page)?, *self.sizes.get(page)?);
        rasterise(page, f64::from(width) / width_pt)
    }

    fn render_tile(&self, tile: TileKey) -> Option<ImageData> {
        let page = self.pages.get(tile.page)?;
        let size = *self.sizes.get(tile.page)?;
        let region = tiles::tile_region(size, tile.level, tile.col, tile.row)?;
        let scale = f64::from(tiles::level_scale(tile.level));
        let pt = |px: u32| Abs::pt(f64::from(px) / scale);
        let mut frame = Frame::hard(Size::new(pt(region.width), pt(region.height)));
        frame.push_frame(Point::new(-pt(region.x), -pt(region.y)), page.frame.clone());
        let cut = Page {
            frame,
            bleed: Sides::default(),
            fill: page.fill.clone(),
            numbering: None,
            supplement: Content::empty(),
            number: page.number,
        };
        rasterise(&cut, scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{WorldParts, compile_source};

    #[test]
    fn a_tile_shows_its_region_of_the_page() {
        let out = compile_source(
            &WorldParts::new(),
            "#set page(width: 300pt, height: 400pt)\n= Tiles\n#lorem(120)\n#rect(width: 100%, height: 80pt, fill: blue)\n",
        );
        let document = out.document.expect("laid out");
        let level = 8;
        let (pw, ph) = tiles::page_pixels(document.sizes()[0], level);
        let page = document.render_page(0, pw).expect("the page renders");
        assert_eq!((page.width, page.height), (pw, ph));
        let key = TileKey {
            page: 0,
            level,
            col: 1,
            row: 1,
        };
        let tile = document.render_tile(key).expect("the tile renders");
        let region = tiles::tile_region(document.sizes()[0], level, 1, 1).unwrap();
        assert_eq!((tile.width, tile.height), (region.width, region.height));
        let (page_px, tile_px) = (page.data.data(), tile.data.data());
        let mut diff = 0u64;
        let mut inked = 0u64;
        for y in 0..region.height {
            for x in 0..region.width {
                let p = (((region.y + y) * pw + region.x + x) * 4) as usize;
                let t = ((y * region.width + x) * 4) as usize;
                for c in 0..3 {
                    diff += u64::from(page_px[p + c].abs_diff(tile_px[t + c]));
                }
                inked += u64::from(tile_px[t] < 128);
            }
        }
        let mean = diff as f64 / f64::from(region.width * region.height * 3);
        assert!(inked > 100, "the tile carries the page's text and figure");
        assert!(
            mean < 2.0,
            "the tile matches the page's pixels (mean difference {mean})"
        );
    }

    #[test]
    fn an_edit_changes_only_the_looks_of_the_pages_it_touches() {
        let parts = WorldParts::new();
        let source = |middle: &str| {
            format!(
                "= One\n#lorem(30)\n#pagebreak()\n= Two\n{middle}\n#pagebreak()\n= Three\n#lorem(30)\n"
            )
        };
        let before = compile_source(&parts, &source("#lorem(30)"))
            .document
            .unwrap();
        let after = compile_source(&parts, &source("Edited. #lorem(30)"))
            .document
            .unwrap();
        assert_eq!(before.len(), 3);
        assert_eq!(before.looks()[0], after.looks()[0]);
        assert_ne!(before.looks()[1], after.looks()[1]);
        assert_eq!(before.looks()[2], after.looks()[2]);
    }
}
