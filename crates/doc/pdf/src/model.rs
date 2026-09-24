//! Engine-independent document facts: metadata, page geometry, outline,
//! links, form fields, annotations, and render requests.
//!
//! Everything read from a document (links, annotations, form widgets, text)
//! is in page **display** space; edits written back ([`crate::edits`]) are in
//! PDF **user** space so they stay exact however the page is displayed.
//! [`PageGeometry`] converts between the two.

use serde::{Deserialize, Serialize};

use crate::geom::{PageGeometry, PageSize, Point, Rect, Rotation};

/// Facts about an opened document, fetched once at open.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DocumentInfo {
    /// Geometry of every page: its visible box in user space and its own
    /// `/Rotate`. [`PageGeometry::size`] is the display size.
    pub pages: Vec<PageGeometry>,
    pub metadata: Metadata,
    /// Whether the document has an interactive form (`AcroForm`).
    pub has_form: bool,
}

/// Document information dictionary entries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
    pub creator: String,
    pub producer: String,
}

/// Where a link, bookmark or destination points.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Destination {
    /// A page of this document (0-based), optionally at a point on it.
    Page { page: usize, at: Option<Point> },
    /// An external URI.
    Uri(String),
    /// Another file (a `/Launch` or remote go-to action), by its path.
    File(String),
}

/// One outline (bookmark) entry and its children.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlineEntry {
    pub title: String,
    pub destination: Option<Destination>,
    pub children: Vec<OutlineEntry>,
}

impl OutlineEntry {
    /// Depth-first flattening as `(depth, entry)` pairs.
    #[must_use]
    pub fn flatten(entries: &[Self]) -> Vec<(usize, &Self)> {
        let mut out = Vec::new();
        let mut stack: Vec<(usize, &Self)> = entries.iter().rev().map(|e| (0, e)).collect();
        while let Some((depth, entry)) = stack.pop() {
            out.push((depth, entry));
            stack.extend(
                entry
                    .children
                    .iter()
                    .rev()
                    .map(|c| (depth.saturating_add(1), c)),
            );
        }
        out
    }
}

/// A link area on a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub rect: Rect,
    pub destination: Destination,
}

/// An RGBA colour, 8 bits per channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// The subtype of an existing annotation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnotationKind {
    Text,
    Link,
    FreeText,
    Line,
    Square,
    Circle,
    Polygon,
    Polyline,
    Highlight,
    Underline,
    Squiggly,
    StrikeOut,
    Stamp,
    Caret,
    Ink,
    Popup,
    FileAttachment,
    Widget,
    Redact,
    /// Any other subtype, by name.
    Other(String),
}

/// An annotation present in the document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationInfo {
    pub kind: AnnotationKind,
    pub rect: Rect,
    pub color: Option<Rgba>,
    /// `/Contents`.
    pub contents: String,
    /// `/T` — the author.
    pub author: String,
    /// `/NM` — the annotation's unique name, when set.
    pub name: String,
    /// Text-markup quadrilaterals as rectangles (highlight, underline, …).
    pub quads: Vec<Rect>,
    /// Ink strokes.
    pub ink: Vec<Vec<Point>>,
}

/// The type of a form field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldKind {
    Text,
    CheckBox,
    RadioButton,
    ComboBox,
    ListBox,
    PushButton,
    Signature,
    Unknown,
}

/// One widget (on-page appearance) of a form field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldWidget {
    pub page: usize,
    pub rect: Rect,
    /// For check boxes and radio buttons: the export value this widget sets.
    pub export_value: String,
    /// For check boxes and radio buttons: whether this widget is on.
    pub checked: bool,
}

/// A form field and its current value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormField {
    /// Fully qualified field name — the key a form value record uses.
    pub name: String,
    pub kind: FieldKind,
    /// The current value (`/V`) as text.
    pub value: String,
    /// Choice options for combo and list boxes.
    pub options: Vec<String>,
    pub read_only: bool,
    pub required: bool,
    pub multiline: bool,
    pub widgets: Vec<FieldWidget>,
}

/// A rectangle of device pixels within a rendered page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// What to render.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderOptions {
    /// Device pixels per point.
    pub scale: f32,
    /// Extra clockwise rotation on top of the page's own.
    pub rotation: Rotation,
    /// Render only this pixel region of the page at `scale`; `None` renders
    /// the whole page.
    pub region: Option<PixelRect>,
    /// Draw annotation appearances (including form widgets).
    pub annotations: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            rotation: Rotation::None,
            region: None,
            annotations: true,
        }
    }
}

impl RenderOptions {
    /// Options rendering the whole page `width_px` pixels wide.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "pixel widths are far below f32's exact range"
    )]
    pub fn fit_width(page: PageSize, width_px: u32) -> Self {
        let scale = if page.width > 0.0 {
            width_px as f32 / page.width
        } else {
            1.0
        };
        Self {
            scale,
            ..Self::default()
        }
    }

    /// Pixel size of the whole page at these options.
    #[must_use]
    pub fn page_pixels(&self, page: PageSize) -> (u32, u32) {
        let size = self.rotation.apply_size(page);
        (px(size.width * self.scale), px(size.height * self.scale))
    }

    /// Pixel size of the output bitmap.
    #[must_use]
    pub fn output_pixels(&self, page: PageSize) -> (u32, u32) {
        match self.region {
            Some(r) => (r.width, r.height),
            None => self.page_pixels(page),
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the value is finite, positive and clamped to u32's range first"
)]
fn px(v: f32) -> u32 {
    if v.is_finite() && v > 0.0 {
        v.round().min(u32::MAX as f32) as u32
    } else {
        0
    }
}

/// Upper bound on the pixels one render may produce (32 Mpx, 128 MiB).
pub const MAX_RENDER_PIXELS: u64 = 32 * 1024 * 1024;

/// A fixed tiling of a page's pixels at one scale, for rendering only the
/// visible part of a zoomed page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileGrid {
    /// Edge length of a square tile in device pixels.
    pub tile: u32,
}

/// One tile of a [`TileGrid`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tile {
    pub col: u32,
    pub row: u32,
    pub region: PixelRect,
}

impl Default for TileGrid {
    fn default() -> Self {
        Self { tile: 512 }
    }
}

impl TileGrid {
    /// Tiles intersecting `visible` (in page pixels at `options.scale`),
    /// row-major. Edge tiles are clipped to the page.
    #[must_use]
    pub fn tiles(&self, page: PageSize, options: &RenderOptions, visible: PixelRect) -> Vec<Tile> {
        let (page_w, page_h) = options.page_pixels(page);
        let t = self.tile.max(1);
        if page_w == 0 || page_h == 0 {
            return Vec::new();
        }
        let x_end = visible.x.saturating_add(visible.width).min(page_w);
        let y_end = visible.y.saturating_add(visible.height).min(page_h);
        let mut out = Vec::new();
        let mut row = visible.y.checked_div(t).unwrap_or(0);
        while row.saturating_mul(t) < y_end {
            let mut col = visible.x.checked_div(t).unwrap_or(0);
            while col.saturating_mul(t) < x_end {
                let left = col.saturating_mul(t);
                let top = row.saturating_mul(t);
                out.push(Tile {
                    col,
                    row,
                    region: PixelRect {
                        x: left,
                        y: top,
                        width: t.min(page_w.saturating_sub(left)),
                        height: t.min(page_h.saturating_sub(top)),
                    },
                });
                col = col.saturating_add(1);
            }
            row = row.saturating_add(1);
        }
        out
    }

    /// Snap a continuous zoom scale to the discrete scale tiles are cached
    /// at: steps of 2^(1/4), so a pinch reuses tiles until the scale moves
    /// by about 19%.
    #[must_use]
    pub fn snap_scale(scale: f32) -> f32 {
        if !(scale.is_finite() && scale > 0.0) {
            return 1.0;
        }
        let steps = (scale.log2() * 4.0).round();
        (steps / 4.0).exp2()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cover_the_visible_region_and_clip_at_page_edges() {
        let page = PageSize {
            width: 600.0,
            height: 800.0,
        };
        let options = RenderOptions {
            scale: 2.0,
            ..RenderOptions::default()
        };
        let grid = TileGrid { tile: 512 };
        let all = grid.tiles(
            page,
            &options,
            PixelRect {
                x: 0,
                y: 0,
                width: u32::MAX,
                height: u32::MAX,
            },
        );
        // 1200 x 1600 px → 3 x 4 tiles.
        assert_eq!(all.len(), 12);
        let covered: u64 = all
            .iter()
            .map(|t| u64::from(t.region.width) * u64::from(t.region.height))
            .sum();
        assert_eq!(covered, 1200 * 1600);
        let some = grid.tiles(
            page,
            &options,
            PixelRect {
                x: 600,
                y: 600,
                width: 10,
                height: 10,
            },
        );
        assert_eq!(some.len(), 1);
        assert_eq!((some[0].col, some[0].row), (1, 1));
    }

    #[test]
    fn scale_snapping_is_stable() {
        assert_eq!(TileGrid::snap_scale(1.0), 1.0);
        assert_eq!(TileGrid::snap_scale(2.05), 2.0);
        assert_eq!(
            TileGrid::snap_scale(TileGrid::snap_scale(3.3)),
            TileGrid::snap_scale(3.3)
        );
    }
}
