//! `tilo-doc-pdf` — render a [`tilo_doc::DocSnapshot`] to a paginated PDF.
//!
//! Takes the materialized, read-only document view and lays it out onto
//! US-Letter pages, returning the bytes of a valid PDF 1.7 file. This
//! crate only exposes [`render_pdf`].
//!
//! ## Approach
//!
//! Written by a small hand-rolled writer (`pdf`) — no `printpdf`
//! dependency — for lockfile stability and full control of pagination
//! and image embedding. Text uses the base-14 Helvetica/Helvetica-Bold
//! fonts (viewer-shipped); AFM advance widths (`metrics`) drive
//! accurate word-wrap. Images are decoded with the `image` crate and
//! embedded as `/DeviceRGB` `Image` `XObjects`.
//!
//! ## Known limitations
//!
//! Fonts: base-14 Helvetica only — glyphs outside `WinAnsiEncoding` (CJK,
//! most non-Latin) render as `?`; italic isn't distinguished from
//! regular, only bold is. Images: re-encoded to raw RGB and
//! Flate-compressed (fine for export, not size-optimal); failures fall
//! back to a labelled placeholder. Tables: a simple fixed-grid, no
//! merging/per-cell wrapping beyond one clipped line/row spanning.
//! Marks: inline bold/italic spans aren't yet applied per-span — bold
//! comes from block kind/level, not `strong` marks.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod metrics;
mod pdf;

use std::fmt;
use tilo_doc::export::{BlockKind, ExportBlock, TablePlan};
use tilo_doc::snapshot::AttrVal;
use tilo_doc::{BlockView, DocSnapshot};

use pdf::{ObjId, PdfWriter, escape_pdf_text};
use tilo_content::ContentSource;

/// Errors that can arise while rendering a snapshot to PDF.
#[derive(Debug)]
pub enum DocPdfError {
    /// An image block referenced a blob that could not be decoded. Carries
    /// the blob hash for diagnostics; non-fatal in [`render_pdf`] (a
    /// placeholder is drawn instead).
    ImageDecode(String),
}

/// One raster page for a searchable scanned-document PDF.
pub struct ScannedPdfPage {
    pub width: u32,
    pub height: u32,
    /// Packed RGB8 pixels, row-major.
    pub rgb: Vec<u8>,
    /// OCR text associated with this page. It is embedded invisibly so copy,
    /// search, and indexing work without drawing over the original scan.
    pub recognized_text: String,
}

/// Export ordered scanned pages as a self-contained searchable A4 PDF.
#[must_use]
pub fn render_scanned_pdf(pages: &[ScannedPdfPage]) -> Vec<u8> {
    use std::io::Write as _;

    const W: f64 = 595.0;
    const H: f64 = 842.0;
    const MARGIN: f64 = 18.0;
    let mut writer = PdfWriter::new();
    let font = writer.add(font_dict("Helvetica").into_bytes());
    let pages_obj = writer.reserve();
    let mut page_objects = Vec::with_capacity(pages.len());

    for (index, page) in pages.iter().enumerate() {
        if page.width == 0 || page.height == 0 {
            continue;
        }
        let expected = usize::try_from(page.width)
            .ok()
            .and_then(|w| {
                usize::try_from(page.height)
                    .ok()
                    .and_then(|h| w.checked_mul(h))
            })
            .and_then(|pixels| pixels.checked_mul(3));
        if expected != Some(page.rgb.len()) {
            continue;
        }
        let image_data = pdf::flate(&page.rgb);
        let image = writer.add_stream(
            &format!(
                " /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
                page.width, page.height
            ),
            &image_data,
        );
        let available_w = W - MARGIN * 2.0;
        let available_h = H - MARGIN * 2.0;
        let scale = (available_w / f64::from(page.width)).min(available_h / f64::from(page.height));
        let draw_w = f64::from(page.width) * scale;
        let draw_h = f64::from(page.height) * scale;
        let x = (W - draw_w) * 0.5;
        let y = (H - draw_h) * 0.5;
        let mut content = Vec::new();
        let _ = writeln!(
            content,
            "q {draw_w} 0 0 {draw_h} {x} {y} cm /Im{index} Do Q"
        );
        if !page.recognized_text.trim().is_empty() {
            let escaped = pdf::escape_pdf_text(&page.recognized_text);
            let _ = write!(content, "BT /F1 10 Tf 3 Tr 18 18 Td (");
            content.extend_from_slice(&escaped);
            content.extend_from_slice(b") Tj ET\n");
        }
        // Content commands are tiny compared with the compressed raster and
        // remain uncompressed so document indexers can recover the invisible
        // OCR layer even when they implement only the core PDF filters.
        let contents = writer.add_stream("", &content);
        let resources = format!(
            "<< /Font << /F1 {} 0 R >> /XObject << /Im{index} {} 0 R >> >>",
            font.0, image.0
        );
        let body = format!(
            "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 595 842] /Resources {resources} /Contents {} 0 R >>",
            pages_obj.0, contents.0
        );
        page_objects.push(writer.add(body.into_bytes()));
    }
    let kids = page_objects
        .iter()
        .map(|object| format!("{} 0 R", object.0))
        .collect::<Vec<_>>()
        .join(" ");
    writer.set(
        pages_obj,
        format!(
            "<< /Type /Pages /Count {} /Kids [{kids}] >>",
            page_objects.len()
        )
        .into_bytes(),
    );
    let catalog =
        writer.add(format!("<< /Type /Catalog /Pages {} 0 R >>", pages_obj.0).into_bytes());
    writer.finish(catalog)
}

impl fmt::Display for DocPdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocPdfError::ImageDecode(h) => write!(f, "failed to decode image blob {h}"),
        }
    }
}

impl std::error::Error for DocPdfError {}

// ---- page geometry (US-Letter) ---------------------------------------------

const PAGE_W: f32 = 612.0; // 8.5in * 72
const PAGE_H: f32 = 792.0; // 11in * 72
const MARGIN: f32 = 72.0; // 1in margins
const TEXT_W: f32 = PAGE_W - 2.0 * MARGIN;
const CONTENT_TOP: f32 = PAGE_H - MARGIN;
const CONTENT_BOTTOM: f32 = MARGIN;

const BODY_SIZE: f32 = 11.0;
const BODY_LEADING: f32 = 14.0; // line height for body text
const PARA_GAP: f32 = 6.0; // extra space after a paragraph/heading
const LIST_INDENT: f32 = 18.0; // per-level indent for lists
const TABLE_PAD: f32 = 4.0; // cell text padding

// ---- low-level drawing model ------------------------------------------------

/// One drawing op accumulated into a page's content stream, collected
/// per page and serialized at the end so the writer stays a single pass.
enum Op {
    /// Show `text` at baseline `(x, y)` in font `bold?` at `size` pt.
    Text {
        x: f32,
        y: f32,
        size: f32,
        bold: bool,
        text: String,
    },
    /// Stroke a 1pt line from `(x1,y1)` to `(x2,y2)`.
    Line { x1: f32, y1: f32, x2: f32, y2: f32 },
    /// Stroke a rectangle outline at `(x,y)` (lower-left) sized `w`×`h`.
    Rect { x: f32, y: f32, w: f32, h: f32 },
    /// Paint image `XObject` `name` into the box `(x,y,w,h)` (lower-left origin).
    Image {
        name: String,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
}

/// One page under construction: a cursor descending from the top margin
/// and the list of ops drawn so far.
struct Page {
    ops: Vec<Op>,
    /// Image `XObjects` referenced on this page: `(resource_name, obj_id)`.
    images: Vec<(String, ObjId)>,
}

impl Page {
    fn new() -> Self {
        Self {
            ops: Vec::new(),
            images: Vec::new(),
        }
    }
}

/// The pagination/rendering cursor. Owns the writer (so images can be
/// added as `XObjects` on demand) and the running list of pages.
///
/// `first_page` is split out from `later_pages` (rather than one `Vec`
/// with a never-empty invariant) so "there is always a current page" is a
/// fact of the type, not a runtime check [`Layout::cur`] would otherwise
/// have to assert.
struct Layout {
    writer: PdfWriter,
    first_page: Page,
    later_pages: Vec<Page>,
    /// Y cursor on the current page (PDF coords: larger = higher). Text
    /// draws at the baseline, so the cursor tracks the *top* of the next
    /// line and subtracts ascent when placing.
    y: f32,
    next_img: usize,
}

impl Layout {
    fn new() -> Self {
        Layout {
            writer: PdfWriter::new(),
            first_page: Page::new(),
            later_pages: Vec::new(),
            y: CONTENT_TOP,
            next_img: 0,
        }
    }

    fn cur(&mut self) -> &mut Page {
        self.later_pages.last_mut().unwrap_or(&mut self.first_page)
    }

    fn new_page(&mut self) {
        self.later_pages.push(Page::new());
        self.y = CONTENT_TOP;
    }

    /// Ensure `need` points of vertical space remain; otherwise start a page.
    fn ensure(&mut self, need: f32) {
        if self.y - need < CONTENT_BOTTOM {
            self.new_page();
        }
    }

    /// Draw a single already-wrapped line at the current cursor, then
    /// advance by `leading`. `size`/`bold` pick the font.
    fn line(&mut self, text: &str, x: f32, size: f32, leading: f32, bold: bool) {
        self.ensure(leading);
        // Baseline sits ~`size` below the cursor's top (approx ascent).
        let baseline = self.y - size * 0.8;
        self.cur().ops.push(Op::Text {
            x,
            y: baseline,
            size,
            bold,
            text: text.to_string(),
        });
        self.y -= leading;
    }

    fn gap(&mut self, amount: f32) {
        self.y -= amount;
    }
}

// ---- public entry point -----------------------------------------------------

/// Render a materialized qdoc snapshot to PDF bytes. Always succeeds for
/// a well-formed snapshot — undecodable image blobs degrade to a
/// placeholder box rather than erroring; an empty document still yields
/// a valid, single-page PDF.
///
/// # Errors
///
/// Never in the current implementation: every failure mode (an undecodable
/// image blob, a missing blob hash) degrades to a placeholder box instead of
/// propagating. The `Result` is kept so a future encoding failure has
/// somewhere to surface without a signature change.
pub fn render_pdf(snap: &DocSnapshot, content: &dyn ContentSource) -> Result<Vec<u8>, DocPdfError> {
    let mut layout = Layout::new();
    // Kept inline elements without visible text (drawings, formulas) have no text to lay out.
    let snap = tilo_doc::export::without_kept_placeholders(snap);

    for block in &snap.blocks {
        render_block(&mut layout, content, block);
    }

    Ok(finish(layout))
}

// ---- block dispatch ---------------------------------------------------------

fn render_block(layout: &mut Layout, content: &dyn ContentSource, block: &BlockView) {
    match ExportBlock::new(block).kind() {
        BlockKind::Heading => render_heading(layout, block),
        BlockKind::ListItem => render_list_item(layout, block),
        BlockKind::Image => render_image(layout, content, block),
        BlockKind::Table => render_table(layout, block),
        BlockKind::Paragraph | BlockKind::Quote | BlockKind::Code => {
            render_paragraph(layout, block);
        }
    }
}

/// Point size for a heading level: H1 24 … H6 12, monotonically decreasing.
fn heading_size(level: u8) -> f32 {
    match level {
        1 => 24.0,
        2 => 20.0,
        3 => 16.0,
        4 => 14.0,
        5 => 12.5,
        _ => 11.5,
    }
}

fn render_heading(layout: &mut Layout, block: &BlockView) {
    let level = ExportBlock::new(block).heading_level();
    let size = heading_size(level);
    let leading = size * 1.25;
    layout.gap(PARA_GAP);
    layout.ensure(leading);
    for line in wrap(&block.text, TEXT_W, size, true) {
        layout.line(&line, MARGIN, size, leading, true);
    }
    layout.gap(PARA_GAP);
}

fn render_paragraph(layout: &mut Layout, block: &BlockView) {
    if block.text.is_empty() {
        // Blank paragraph → vertical space (matches an empty line in the doc).
        layout.gap(BODY_LEADING);
        return;
    }
    for line in wrap(&block.text, TEXT_W, BODY_SIZE, false) {
        layout.line(&line, MARGIN, BODY_SIZE, BODY_LEADING, false);
    }
    layout.gap(PARA_GAP);
}

fn render_list_item(layout: &mut Layout, block: &BlockView) {
    let level = match block.attrs.get("level") {
        // `level` comes from the document; clamp to a depth no real list
        // nests past, so a corrupt/hostile attribute can't push the
        // indent (and this cast) to an absurd value.
        #[allow(
            clippy::cast_precision_loss,
            reason = "clamped to a small range just above; precision loss is impossible in that range"
        )]
        Some(AttrVal::Int(n)) => (*n).clamp(0, 20) as f32,
        _ => 0.0,
    };
    let ordered = matches!(block.attrs.get("ordered"), Some(AttrVal::Bool(true)));
    let indent = MARGIN + level * LIST_INDENT;
    // Marker: ordered lists use the block's `index` attr if present, else "1.".
    let marker = if ordered {
        let n = match block.attrs.get("index") {
            Some(AttrVal::Int(i)) => *i,
            _ => 1,
        };
        format!("{n}.")
    } else {
        "\u{2022}".to_string() // bullet
    };
    let marker_w =
        metrics::text_width(&metrics::HELVETICA, &pdf::to_winansi(&marker), BODY_SIZE) + 4.0;
    let text_indent = indent + marker_w;
    let avail = (MARGIN + TEXT_W) - text_indent;
    let lines = wrap(&block.text, avail.max(40.0), BODY_SIZE, false);

    // Marker on the first line, hung to the left of the wrapped text.
    layout.ensure(BODY_LEADING);
    let first_y_top = layout.y;
    layout.line(
        lines.first().map_or("", String::as_str),
        text_indent,
        BODY_SIZE,
        BODY_LEADING,
        false,
    );
    // Place the marker at the same baseline as the first text line.
    let baseline = first_y_top - BODY_SIZE * 0.8;
    layout.cur().ops.push(Op::Text {
        x: indent,
        y: baseline,
        size: BODY_SIZE,
        bold: false,
        text: marker,
    });
    for line in lines.iter().skip(1) {
        layout.line(line, text_indent, BODY_SIZE, BODY_LEADING, false);
    }
}

fn render_image(layout: &mut Layout, content: &dyn ContentSource, block: &BlockView) {
    let hash = if let Some(AttrVal::Str(s)) = block.attrs.get("blob") {
        s.clone()
    } else {
        placeholder(layout, "image (no blob)");
        return;
    };
    let Some(bytes) = content.content(&hash) else {
        placeholder(layout, "image (missing blob)");
        return;
    };
    match decode_image(&bytes) {
        Ok(img) => place_image(layout, &img),
        Err(_) => placeholder(layout, "image (undecodable)"),
    }
}

fn render_table(layout: &mut Layout, block: &BlockView) {
    // Rows are child blocks, each row's children its cells. Tolerates
    // the alternate flat-cells-with-`cols`-attr shape.
    let plan = TablePlan::new(block);
    if plan.is_empty() {
        return;
    }
    let ncols = plan
        .rows()
        .iter()
        .map(std::vec::Vec::len)
        .max()
        .unwrap_or(1)
        .max(1);
    // A column count feeding a display-only width ratio; precision loss
    // is invisible well before a table could plausibly have this many
    // columns.
    #[allow(
        clippy::cast_precision_loss,
        reason = "ncols is a column count feeding a display ratio; no real table approaches f32's precision limit"
    )]
    let col_w = TEXT_W / ncols as f32;
    let row_h = BODY_LEADING + 2.0 * TABLE_PAD;

    layout.gap(PARA_GAP);
    for row in plan.rows() {
        // Keep a whole row together where possible.
        layout.ensure(row_h);
        let top = layout.y;
        let bottom = top - row_h;
        // Cell grid + text.
        for (ci, cell) in row.iter().enumerate() {
            // A column index feeding a display-only x-offset; precision
            // loss is invisible well before a table could plausibly have
            // this many columns.
            #[allow(
                clippy::cast_precision_loss,
                reason = "ci is a column index feeding a display offset; no real table approaches f32's precision limit"
            )]
            let x = MARGIN + ci as f32 * col_w;
            layout.cur().ops.push(Op::Rect {
                x,
                y: bottom,
                w: col_w,
                h: row_h,
            });
            let text = clip_to_width(&cell.text, col_w - 2.0 * TABLE_PAD, BODY_SIZE);
            let baseline = top - TABLE_PAD - BODY_SIZE * 0.8;
            layout.cur().ops.push(Op::Text {
                x: x + TABLE_PAD,
                y: baseline,
                size: BODY_SIZE,
                bold: false,
                text,
            });
        }
        // Outline any missing trailing cells so the grid stays rectangular.
        for ci in row.len()..ncols {
            // A column index feeding a display-only x-offset; precision
            // loss is invisible well before a table could plausibly have
            // this many columns.
            #[allow(
                clippy::cast_precision_loss,
                reason = "ci is a column index feeding a display offset; no real table approaches f32's precision limit"
            )]
            let x = MARGIN + ci as f32 * col_w;
            layout.cur().ops.push(Op::Rect {
                x,
                y: bottom,
                w: col_w,
                h: row_h,
            });
        }
        layout.y -= row_h;
    }
    layout.gap(PARA_GAP);
}

// ---- images -----------------------------------------------------------------

struct DecodedImage {
    width: u32,
    height: u32,
    /// Raw RGB8, row-major, top-to-bottom.
    rgb: Vec<u8>,
}

fn decode_image(bytes: &[u8]) -> Result<DecodedImage, DocPdfError> {
    let img =
        image::load_from_memory(bytes).map_err(|_| DocPdfError::ImageDecode(String::new()))?;
    let rgb = img.to_rgb8();
    Ok(DecodedImage {
        width: rgb.width(),
        height: rgb.height(),
        rgb: rgb.into_raw(),
    })
}

fn place_image(layout: &mut Layout, img: &DecodedImage) {
    // Scale to text width, preserving aspect; cap height to one page's content.
    // A display-only aspect ratio; no real decoded image dimension
    // approaches f32's precision limit.
    #[allow(
        clippy::cast_precision_loss,
        reason = "aspect ratio is display-only; no real image dimension approaches f32's precision limit"
    )]
    let aspect = img.height as f32 / img.width.max(1) as f32;
    let w = TEXT_W;
    let mut h = w * aspect;
    let max_h = CONTENT_TOP - CONTENT_BOTTOM;
    if h > max_h {
        h = max_h;
    }
    layout.gap(PARA_GAP);
    layout.ensure(h);
    let top = layout.y;
    let bottom = top - h;

    // Register the XObject in the writer and reference it on this page.
    let name = format!("Im{}", layout.next_img);
    // Bounded by the number of images in one document — nowhere near `usize::MAX`.
    layout.next_img = layout.next_img.saturating_add(1);
    let compressed = pdf::flate(&img.rgb);
    let dict = format!(
        " /Type /XObject /Subtype /Image /Width {} /Height {} \
         /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
        img.width, img.height
    );
    let obj = layout.writer.add_stream(&dict, &compressed);
    layout.cur().images.push((name.clone(), obj));
    layout.cur().ops.push(Op::Image {
        name,
        x: MARGIN,
        y: bottom,
        w,
        h,
    });
    layout.y -= h;
    layout.gap(PARA_GAP);
}

/// Draw a labelled placeholder box where an image couldn't be embedded.
fn placeholder(layout: &mut Layout, label: &str) {
    let h = 120.0;
    layout.gap(PARA_GAP);
    layout.ensure(h);
    let top = layout.y;
    let bottom = top - h;
    layout.cur().ops.push(Op::Rect {
        x: MARGIN,
        y: bottom,
        w: TEXT_W,
        h,
    });
    // Diagonal cross to read clearly as a placeholder.
    layout.cur().ops.push(Op::Line {
        x1: MARGIN,
        y1: bottom,
        x2: MARGIN + TEXT_W,
        y2: top,
    });
    layout.cur().ops.push(Op::Line {
        x1: MARGIN,
        y1: top,
        x2: MARGIN + TEXT_W,
        y2: bottom,
    });
    let baseline = top - h / 2.0;
    let label = clip_to_width(label, TEXT_W - 8.0, BODY_SIZE);
    layout.cur().ops.push(Op::Text {
        x: MARGIN + 6.0,
        y: baseline,
        size: BODY_SIZE,
        bold: false,
        text: label,
    });
    layout.y -= h;
    layout.gap(PARA_GAP);
}

// ---- text wrapping ----------------------------------------------------------

/// Word-wrap `text` to `max_w` points at `size`pt. Hard newlines in the
/// source split lines; over-long single words are hard-broken so they
/// never overflow the text column.
fn wrap(text: &str, max_w: f32, size: f32, bold: bool) -> Vec<String> {
    let table = if bold {
        &metrics::HELVETICA_BOLD
    } else {
        &metrics::HELVETICA
    };
    let mut out = Vec::new();
    for src_line in text.split('\n') {
        if src_line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in src_line.split(' ') {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            let w = metrics::text_width(table, &pdf::to_winansi(&candidate), size);
            if w <= max_w || line.is_empty() {
                if w <= max_w {
                    line = candidate;
                } else {
                    // Single word longer than the column: hard-break it.
                    for chunk in hard_break(word, max_w, size, table) {
                        out.push(chunk);
                    }
                    line.clear();
                }
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
                // The new word alone might still overflow.
                if metrics::text_width(table, &pdf::to_winansi(&line), size) > max_w {
                    for chunk in hard_break(&line, max_w, size, table) {
                        out.push(chunk);
                    }
                    line.clear();
                }
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Break a single overlong word into column-width chunks (char granularity).
fn hard_break(word: &str, max_w: f32, size: f32, table: &[u16; 256]) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    for ch in word.chars() {
        let candidate = format!("{cur}{ch}");
        if metrics::text_width(table, &pdf::to_winansi(&candidate), size) > max_w && !cur.is_empty()
        {
            chunks.push(std::mem::take(&mut cur));
            cur.push(ch);
        } else {
            cur = candidate;
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// Truncate `text` to fit `max_w` at `size`pt, appending "…" if clipped
/// — used for table cells and single-line placeholder labels.
fn clip_to_width(text: &str, max_w: f32, size: f32) -> String {
    let table = &metrics::HELVETICA;
    if metrics::text_width(table, &pdf::to_winansi(text), size) <= max_w {
        return text.replace('\n', " ");
    }
    let flat = text.replace('\n', " ");
    let ell = "\u{2026}";
    let ell_w = metrics::text_width(table, &pdf::to_winansi(ell), size);
    let mut cur = String::new();
    for ch in flat.chars() {
        let candidate = format!("{cur}{ch}");
        if metrics::text_width(table, &pdf::to_winansi(&candidate), size) + ell_w > max_w {
            break;
        }
        cur = candidate;
    }
    cur.push_str(ell);
    cur
}

// ---- serialization ----------------------------------------------------------

/// Turn the accumulated pages into the final PDF byte stream.
fn finish(mut layout: Layout) -> Vec<u8> {
    // Font objects (shared across pages).
    let helv = layout.writer.add(font_dict("Helvetica").into_bytes());
    let helv_bold = layout.writer.add(font_dict("Helvetica-Bold").into_bytes());

    // Reserve the pages-tree object so each page can reference its parent.
    let pages_obj = layout.writer.reserve();

    // Take ownership of the page list so we can borrow the writer mutably.
    let mut pages = vec![std::mem::replace(&mut layout.first_page, Page::new())];
    pages.append(&mut layout.later_pages);
    let mut page_objs: Vec<ObjId> = Vec::with_capacity(pages.len());

    for page in pages {
        let content = serialize_ops(&page.ops);
        let content_data = pdf::flate(&content);
        let content_obj = layout
            .writer
            .add_stream(" /Filter /FlateDecode", &content_data);

        // Per-page resources: fonts always; XObjects when images were placed.
        let mut xobjects = String::new();
        if !page.images.is_empty() {
            use std::fmt::Write as _;
            xobjects.push_str(" /XObject << ");
            for (name, obj) in &page.images {
                // Writing to a `String` cannot fail; discard the always-`Ok`
                // result rather than allocating a temporary just to push it.
                let _ = write!(xobjects, "/{} {} 0 R ", name, obj.0);
            }
            xobjects.push_str(">>");
        }
        let resources = format!(
            "<< /Font << /F1 {} 0 R /F2 {} 0 R >>{} >>",
            helv.0, helv_bold.0, xobjects
        );
        // `PAGE_W`/`PAGE_H` are fixed page-geometry constants (612.0,
        // 792.0), not derived from any document input, so this truncation
        // is provably exact.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "PAGE_W/PAGE_H are fixed whole-number constants, not document-derived"
        )]
        let page_body = format!(
            "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 {} {}] \
             /Resources {} /Contents {} 0 R >>",
            pages_obj.0, PAGE_W as i32, PAGE_H as i32, resources, content_obj.0
        );
        page_objs.push(layout.writer.add(page_body.into_bytes()));
    }

    // Fill in the pages tree now that we have the kids.
    let kids = page_objs
        .iter()
        .map(|o| format!("{} 0 R", o.0))
        .collect::<Vec<_>>()
        .join(" ");
    let pages_body = format!(
        "<< /Type /Pages /Count {} /Kids [{}] >>",
        page_objs.len(),
        kids
    );
    layout.writer.set(pages_obj, pages_body.into_bytes());

    let catalog = layout
        .writer
        .add(format!("<< /Type /Catalog /Pages {} 0 R >>", pages_obj.0).into_bytes());
    layout.writer.finish(catalog)
}

fn font_dict(base: &str) -> String {
    format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding >>")
}

/// Serialize a page's ops into a PDF content stream.
fn serialize_ops(ops: &[Op]) -> Vec<u8> {
    use std::io::Write as _;
    let mut s = Vec::new();
    for op in ops {
        match op {
            Op::Text {
                x,
                y,
                size,
                bold,
                text,
            } => {
                let font = if *bold { "F2" } else { "F1" };
                let _ = write!(s, "BT /{font} {size} Tf {x:.2} {y:.2} Td (");
                s.extend_from_slice(&escape_pdf_text(text));
                let _ = writeln!(s, ") Tj ET");
            }
            Op::Line { x1, y1, x2, y2 } => {
                let _ = writeln!(s, "{x1:.2} {y1:.2} m {x2:.2} {y2:.2} l S");
            }
            Op::Rect { x, y, w, h } => {
                let _ = writeln!(s, "{x:.2} {y:.2} {w:.2} {h:.2} re S");
            }
            Op::Image { name, x, y, w, h } => {
                // Image space is a unit square mapped by the CTM; q/Q isolates it.
                let _ = writeln!(s, "q {w:.2} 0 0 {h:.2} {x:.2} {y:.2} cm /{name} Do Q");
            }
        }
    }
    s
}

#[cfg(test)]
mod tests;
