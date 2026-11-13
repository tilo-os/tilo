//! `tilo-doc-pdf` — render a [`tilo_doc::DocSnapshot`] to a paginated PDF: the
//! read-only document view laid out onto US-Letter pages as the bytes of a
//! valid PDF 1.7 file. The crate exposes only [`render_pdf`].
//!
//! The writer (`pdf`) is hand-rolled, so pagination and image embedding stay
//! under control and the lockfile stable. Text uses the base-14 Helvetica
//! family with AFM advance widths (`metrics`) for word-wrap; images are decoded
//! with the `image` crate and embedded as `/DeviceRGB` `Image` `XObjects`.

mod metrics;
mod pdf;

use std::fmt;
use tilo_doc::export::{BlockKind, ExportBlock, TablePlan};
use tilo_doc::paging::{Block as PageBlock, Fragmentation, PageFlow, Placed};
use tilo_doc::snapshot::AttrVal;
use tilo_doc::{BlockView, DocSnapshot};

use pdf::{ObjId, PdfWriter, escape_pdf_text};
use tilo_content::ContentSource;

#[derive(Debug)]
pub enum DocPdfError {
    /// An image block referenced a blob that could not be decoded, with the blob hash
    /// for diagnostics; non-fatal, a placeholder is drawn instead.
    ImageDecode(String),
}

pub struct ScannedPdfPage {
    pub width: u32,
    pub height: u32,
    /// Packed RGB8 pixels, row-major.
    pub rgb: Vec<u8>,
    /// OCR text, embedded invisibly, so copy, search and indexing work without
    /// drawing over the scan.
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
        // Content commands are tiny beside the compressed raster, so they stay
        // uncompressed and an indexer that implements only core filters can still
        // recover the invisible OCR layer.
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
}

impl Style {
    const BOLD: Self = Self {
        bold: true,
        italic: false,
    };

    /// The advance widths of this face; an oblique face is its upright face slanted
    /// and shares its widths.
    fn widths(self) -> &'static [u16; 256] {
        if self.bold {
            &metrics::HELVETICA_BOLD
        } else {
            &metrics::HELVETICA
        }
    }

    fn font(self) -> &'static str {
        match (self.bold, self.italic) {
            (false, false) => "F1",
            (true, false) => "F2",
            (false, true) => "F3",
            (true, true) => "F4",
        }
    }

    fn width(self, text: &str, size: f32) -> f32 {
        metrics::text_width(self.widths(), &pdf::to_winansi(text), size)
    }
}

/// The faces a document can use, in resource order: `F1` … `F4`.
const FACES: [&str; 4] = [
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
];

/// One drawing op, collected per page and serialized at the end, so the writer
/// stays a single pass.
enum Op {
    Text {
        x: f32,
        y: f32,
        size: f32,
        style: Style,
        text: String,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    Image {
        name: String,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
}

impl Op {
    fn raise(&mut self, dy: f32) {
        match self {
            Op::Text { y, .. } | Op::Rect { y, .. } | Op::Image { y, .. } => *y += dy,
            Op::Line { y1, y2, .. } => {
                *y1 += dy;
                *y2 += dy;
            }
        }
    }
}

struct Page {
    ops: Vec<Op>,
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

/// The pagination and rendering cursor: the writer, so images can be added as
/// `XObjects` on demand, and the pages. `first_page` is split out of
/// `later_pages` so "there is always a page" is a fact of the type rather than a
/// runtime check.
struct Layout {
    writer: PdfWriter,
    first_page: Page,
    later_pages: Vec<Page>,
    flow: PageFlow,
    /// Each placed block's page and first op there, in placement order, so a
    /// heading carried to the next page with its text can follow it.
    block_ops: Vec<(usize, usize)>,
    next_img: usize,
}

impl Layout {
    fn new() -> Self {
        Layout {
            writer: PdfWriter::new(),
            first_page: Page::new(),
            later_pages: Vec::new(),
            flow: PageFlow::new(f64::from(CONTENT_TOP - CONTENT_BOTTOM)),
            block_ops: Vec::new(),
            next_img: 0,
        }
    }

    fn page(&mut self, index: usize) -> &mut Page {
        if index > self.later_pages.len() {
            self.later_pages.resize_with(index, Page::new);
        }
        match index.checked_sub(1) {
            None => &mut self.first_page,
            Some(later) => self
                .later_pages
                .get_mut(later)
                .unwrap_or(&mut self.first_page),
        }
    }

    /// Place a block that splits between `lines` (each line's top and
    /// height below the block's top), or that cannot split when `lines` is
    /// empty.
    fn place(&mut self, block: &PageBlock, lines: &[(f64, f64)]) -> Placed {
        let placed = if lines.is_empty() {
            self.flow.place(block)
        } else {
            self.flow.place_lines(block, || lines.to_vec())
        };
        if let Some(carry) = placed.carry
            && let Some(&(page, first_op)) = self.block_ops.get(carry.block)
        {
            let from = self.page(page);
            let ops = from.ops.split_off(first_op.min(from.ops.len()));
            let (images, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut from.images)
                .into_iter()
                .partition(|(name, _)| {
                    ops.iter()
                        .any(|op| matches!(op, Op::Image { name: n, .. } if n == name))
                });
            from.images = kept;
            let dy = CONTENT_TOP - pdf_y(carry.from_top);
            let to = self.page(carry.from_page.saturating_add(1));
            for mut op in ops {
                op.raise(dy);
                to.ops.push(op);
            }
            to.images.extend(images);
        }
        let first_op = self.page(placed.page).ops.len();
        self.block_ops.push((placed.page, first_op));
        placed
    }

    fn lines(&mut self, block: &PageBlock, lines: &[Vec<Run>], text: TextSet) -> Placed {
        let TextSet { x, size, leading } = text;
        let boxes = line_boxes(lines.len(), f64::from(leading));
        let placed = self.place(block, &boxes);
        for (index, runs) in lines.iter().enumerate() {
            let (page, top) = placed.locate(&boxes, index);
            let baseline = pdf_y(top) - size * 0.8;
            let mut x = x;
            for (text, style) in runs {
                self.page(page).ops.push(Op::Text {
                    x,
                    y: baseline,
                    size,
                    style: *style,
                    text: text.clone(),
                });
                x += style.width(text, size);
            }
        }
        placed
    }

    fn gap(&mut self, amount: f32) {
        self.flow.skip(f64::from(amount));
    }
}

#[derive(Clone, Copy)]
struct TextSet {
    x: f32,
    size: f32,
    leading: f32,
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "an offset within one page's content box, far inside f32's range"
)]
fn pdf_y(offset: f64) -> f32 {
    CONTENT_TOP - offset as f32
}

fn line_boxes(count: usize, height: f64) -> Vec<(f64, f64)> {
    (0..count)
        .scan(0.0, |top, _| {
            let line = (*top, height);
            *top += height;
            Some(line)
        })
        .collect()
}

fn text_block(block: &BlockView, lines: usize, leading: f32) -> PageBlock {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a line count feeding a height; no real block approaches f64's precision limit"
    )]
    let height = lines as f64 * f64::from(leading);
    PageBlock::new(height).breaking_before(breaks_before(block))
}

fn breaks_before(block: &BlockView) -> bool {
    block.flag("page_break_before")
}

/// Render a materialized tilo-doc snapshot to PDF bytes, a valid single page even
/// when the document is empty. An undecodable image blob or a missing blob hash
/// degrades to a placeholder box.
///
/// # Errors
///
/// Never: the `Result` is kept so a future encoding failure has somewhere to
/// surface.
pub fn render_pdf(snap: &DocSnapshot, content: &dyn ContentSource) -> Result<Vec<u8>, DocPdfError> {
    let mut layout = Layout::new();
    let snap = tilo_doc::export::without_kept_placeholders(snap);

    for block in &snap.blocks {
        render_block(&mut layout, content, block);
    }

    Ok(finish(layout))
}

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
    let lines = wrap(&styled(block, Style::BOLD), TEXT_W, size);
    let page_block = text_block(block, lines.len(), leading)
        .with_lead(f64::from(PARA_GAP))
        .keeping_with_next(true);
    layout.lines(
        &page_block,
        &lines,
        TextSet {
            x: MARGIN,
            size,
            leading,
        },
    );
    layout.gap(PARA_GAP);
}

fn render_paragraph(layout: &mut Layout, block: &BlockView) {
    if block.text.is_empty() {
        // Blank paragraph → one empty line (as in the doc).
        layout.place(&text_block(block, 1, BODY_LEADING), &[]);
        return;
    }
    let lines = wrap(&styled(block, Style::default()), TEXT_W, BODY_SIZE);
    let page_block = text_block(block, lines.len(), BODY_LEADING);
    layout.lines(
        &page_block,
        &lines,
        TextSet {
            x: MARGIN,
            size: BODY_SIZE,
            leading: BODY_LEADING,
        },
    );
    layout.gap(PARA_GAP);
}

fn render_list_item(layout: &mut Layout, block: &BlockView) {
    let level = match block.attrs.get("level") {
        #[allow(
            clippy::cast_precision_loss,
            reason = "clamped to a small range just above; precision loss is impossible in that range"
        )]
        Some(AttrVal::Int(n)) => (*n).clamp(0, 20) as f32,
        _ => 0.0,
    };
    let ordered = block.flag("ordered");
    let indent = MARGIN + level * LIST_INDENT;
    // Marker: ordered lists use the block's `index` attr if present, else "1.".
    let marker = if ordered {
        let n = block.int_attr("index").unwrap_or(1);
        format!("{n}.")
    } else {
        "\u{2022}".to_string() // bullet
    };
    let marker_w =
        metrics::text_width(&metrics::HELVETICA, &pdf::to_winansi(&marker), BODY_SIZE) + 4.0;
    let text_indent = indent + marker_w;
    let avail = (MARGIN + TEXT_W) - text_indent;
    let lines = wrap(&styled(block, Style::default()), avail.max(40.0), BODY_SIZE);

    // Even an empty item takes a line for its marker.
    let page_block = text_block(block, lines.len().max(1), BODY_LEADING);
    let placed = layout.lines(
        &page_block,
        &lines,
        TextSet {
            x: text_indent,
            size: BODY_SIZE,
            leading: BODY_LEADING,
        },
    );
    let (page, top) = placed.locate(&line_boxes(1, f64::from(BODY_LEADING)), 0);
    layout.page(page).ops.push(Op::Text {
        x: indent,
        y: pdf_y(top) - BODY_SIZE * 0.8,
        size: BODY_SIZE,
        style: Style::default(),
        text: marker,
    });
}

fn render_image(layout: &mut Layout, content: &dyn ContentSource, block: &BlockView) {
    let hash = if let Some(AttrVal::Str(s)) = block.attrs.get("blob") {
        s.clone()
    } else {
        placeholder(layout, block, "image (no blob)");
        return;
    };
    let Some(bytes) = content.content(&hash) else {
        placeholder(layout, block, "image (missing blob)");
        return;
    };
    match decode_image(&bytes) {
        Ok(img) => place_image(layout, block, &img),
        Err(_) => placeholder(layout, block, "image (undecodable)"),
    }
}

fn render_table(layout: &mut Layout, block: &BlockView) {
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
    #[allow(
        clippy::cast_precision_loss,
        reason = "ncols is a column count feeding a display ratio; no real table approaches f32's precision limit"
    )]
    let col_w = TEXT_W / ncols as f32;
    let row_h = BODY_LEADING + 2.0 * TABLE_PAD;

    // The table breaks between rows, never inside one; a manual page break
    // on the table or its first cell breaks before it.
    let boxes = line_boxes(plan.rows().len(), f64::from(row_h));
    #[allow(
        clippy::cast_precision_loss,
        reason = "a row count feeding a height; no real table approaches f64's precision limit"
    )]
    let page_block = PageBlock::new(plan.rows().len() as f64 * f64::from(row_h))
        .with_lead(f64::from(PARA_GAP))
        .breaking_before(
            breaks_before(block)
                || plan
                    .rows()
                    .first()
                    .and_then(|row| row.first())
                    .is_some_and(|cell| breaks_before(cell)),
        )
        .fragmented(Fragmentation::ANYWHERE);
    let placed = layout.place(&page_block, &boxes);
    for (ri, row) in plan.rows().iter().enumerate() {
        let (page, offset) = placed.locate(&boxes, ri);
        let top = pdf_y(offset);
        let bottom = top - row_h;
        let page = layout.page(page);
        for (ci, cell) in row.iter().enumerate() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "ci is a column index feeding a display offset; no real table approaches f32's precision limit"
            )]
            let x = MARGIN + ci as f32 * col_w;
            page.ops.push(Op::Rect {
                x,
                y: bottom,
                w: col_w,
                h: row_h,
            });
            let text = clip_to_width(&cell.text, col_w - 2.0 * TABLE_PAD, BODY_SIZE);
            let baseline = top - TABLE_PAD - BODY_SIZE * 0.8;
            page.ops.push(Op::Text {
                x: x + TABLE_PAD,
                y: baseline,
                size: BODY_SIZE,
                style: Style::default(),
                text,
            });
        }
        for ci in row.len()..ncols {
            #[allow(
                clippy::cast_precision_loss,
                reason = "ci is a column index feeding a display offset; no real table approaches f32's precision limit"
            )]
            let x = MARGIN + ci as f32 * col_w;
            page.ops.push(Op::Rect {
                x,
                y: bottom,
                w: col_w,
                h: row_h,
            });
        }
    }
    layout.gap(PARA_GAP);
}

struct DecodedImage {
    width: u32,
    height: u32,
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

fn place_image(layout: &mut Layout, block: &BlockView, img: &DecodedImage) {
    // Scale to the text width, keeping the aspect, and cap the height to one page's
    // content.
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
    let placed = layout.place(&figure_block(block, h), &[]);
    let top = pdf_y(placed.top);
    let bottom = top - h;

    let name = format!("Im{}", layout.next_img);
    layout.next_img = layout.next_img.saturating_add(1);
    let compressed = pdf::flate(&img.rgb);
    let dict = format!(
        " /Type /XObject /Subtype /Image /Width {} /Height {} \
         /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
        img.width, img.height
    );
    let obj = layout.writer.add_stream(&dict, &compressed);
    let page = layout.page(placed.page);
    page.images.push((name.clone(), obj));
    page.ops.push(Op::Image {
        name,
        x: MARGIN,
        y: bottom,
        w,
        h,
    });
    layout.gap(PARA_GAP);
}

fn figure_block(block: &BlockView, h: f32) -> PageBlock {
    PageBlock::new(f64::from(h))
        .with_lead(f64::from(PARA_GAP))
        .breaking_before(breaks_before(block))
}

fn placeholder(layout: &mut Layout, block: &BlockView, label: &str) {
    let h = 120.0;
    let placed = layout.place(&figure_block(block, h), &[]);
    let top = pdf_y(placed.top);
    let bottom = top - h;
    let page = layout.page(placed.page);
    page.ops.push(Op::Rect {
        x: MARGIN,
        y: bottom,
        w: TEXT_W,
        h,
    });
    page.ops.push(Op::Line {
        x1: MARGIN,
        y1: bottom,
        x2: MARGIN + TEXT_W,
        y2: top,
    });
    page.ops.push(Op::Line {
        x1: MARGIN,
        y1: top,
        x2: MARGIN + TEXT_W,
        y2: bottom,
    });
    let baseline = top - h / 2.0;
    let label = clip_to_width(label, TEXT_W - 8.0, BODY_SIZE);
    page.ops.push(Op::Text {
        x: MARGIN + 6.0,
        y: baseline,
        size: BODY_SIZE,
        style: Style::default(),
        text: label,
    });
    layout.gap(PARA_GAP);
}

type Run = (String, Style);

type Styled = (char, Style);

fn styled(block: &BlockView, base: Style) -> Vec<Styled> {
    let on = |name: &str, at: usize| {
        block.marks.iter().any(|mark| {
            mark.name == name
                && mark.start <= at
                && at < mark.end
                && !matches!(mark.value, tilo_doc::document::MarkValue::Bool(false))
        })
    };
    block
        .text
        .chars()
        .enumerate()
        .map(|(at, ch)| {
            let style = Style {
                bold: base.bold || on("strong", at),
                italic: base.italic || on("em", at),
            };
            (ch, style)
        })
        .collect()
}

fn styled_width(chars: &[Styled], size: f32) -> f32 {
    let mut buffer = [0; 4];
    chars
        .iter()
        .map(|&(ch, style)| style.width(ch.encode_utf8(&mut buffer), size))
        .sum()
}

fn runs(chars: &[Styled]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for &(ch, style) in chars {
        match runs.last_mut() {
            Some((text, face)) if *face == style => text.push(ch),
            _ => runs.push((ch.to_string(), style)),
        }
    }
    runs
}

/// Word-wrap styled characters to `max_w` points at `size`pt, as lines of
/// runs. Hard newlines in the source split lines; over-long single words
/// are hard-broken so they never overflow the text column.
fn wrap(chars: &[Styled], max_w: f32, size: f32) -> Vec<Vec<Run>> {
    let mut out: Vec<Vec<Styled>> = Vec::new();
    for source in chars.split(|&(ch, _)| ch == '\n') {
        if source.is_empty() {
            out.push(Vec::new());
            continue;
        }
        let mut line: Vec<Styled> = Vec::new();
        for piece in source.split_inclusive(|&(ch, _)| ch == ' ') {
            let word = match piece.split_last() {
                Some((&(' ', _), word)) => word,
                _ => piece,
            };
            if styled_width(&line, size) + styled_width(word, size) <= max_w {
                line.extend_from_slice(piece);
                continue;
            }
            if !line.is_empty() {
                out.push(trimmed(std::mem::take(&mut line)));
            }
            if styled_width(word, size) <= max_w {
                line.extend_from_slice(piece);
            } else {
                out.extend(hard_break(word, max_w, size));
            }
        }
        if !line.is_empty() {
            out.push(trimmed(line));
        }
    }
    if out.is_empty() {
        out.push(Vec::new());
    }
    out.iter().map(|line| runs(line)).collect()
}

fn trimmed(mut line: Vec<Styled>) -> Vec<Styled> {
    while line.last().is_some_and(|&(ch, _)| ch == ' ') {
        line.pop();
    }
    line
}

fn hard_break(word: &[Styled], max_w: f32, size: f32) -> Vec<Vec<Styled>> {
    let mut chunks = Vec::new();
    let mut cur: Vec<Styled> = Vec::new();
    for &styled in word {
        cur.push(styled);
        if styled_width(&cur, size) > max_w && cur.len() > 1 {
            cur.pop();
            chunks.push(std::mem::replace(&mut cur, vec![styled]));
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

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

fn finish(mut layout: Layout) -> Vec<u8> {
    let fonts = {
        use std::fmt::Write as _;
        let mut fonts = String::new();
        for (index, face) in FACES.iter().enumerate() {
            let font = layout.writer.add(font_dict(face).into_bytes());
            // Writing to a `String` cannot fail.
            let _ = write!(fonts, "/F{} {} 0 R ", index.saturating_add(1), font.0);
        }
        fonts
    };

    // Reserve the pages-tree object so each page can reference its parent.
    let pages_obj = layout.writer.reserve();

    let mut pages = vec![std::mem::replace(&mut layout.first_page, Page::new())];
    pages.append(&mut layout.later_pages);
    let mut page_objs: Vec<ObjId> = Vec::with_capacity(pages.len());

    for page in pages {
        let content = serialize_ops(&page.ops);
        let content_data = pdf::flate(&content);
        let content_obj = layout
            .writer
            .add_stream(" /Filter /FlateDecode", &content_data);

        let mut xobjects = String::new();
        if !page.images.is_empty() {
            use std::fmt::Write as _;
            xobjects.push_str(" /XObject << ");
            for (name, obj) in &page.images {
                let _ = write!(xobjects, "/{} {} 0 R ", name, obj.0);
            }
            xobjects.push_str(">>");
        }
        let resources = format!("<< /Font << {fonts}>>{xobjects} >>");
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

fn serialize_ops(ops: &[Op]) -> Vec<u8> {
    use std::io::Write as _;
    let mut s = Vec::new();
    for op in ops {
        match op {
            Op::Text {
                x,
                y,
                size,
                style,
                text,
            } => {
                let font = style.font();
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
                let _ = writeln!(s, "q {w:.2} 0 0 {h:.2} {x:.2} {y:.2} cm /{name} Do Q");
            }
        }
    }
    s
}

#[cfg(test)]
mod tests;
