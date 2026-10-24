//! Page rendering: lays a [`crate::model::Page`] out with the toolkit's
//! text engine and paints it onto its paper, then overlays annotations and
//! signatures.
//!
//! Coordinate convention: page geometry is in PDF points; `scale` is
//! pixels-per-point (1.0 == 100% zoom). A point `(px, py)` on the page
//! maps to screen `origin + (px, py) * scale`, then the caller's `base`
//! affine is applied (it is identity for the full-window root and the
//! offscreen shot harness, but threading it keeps the renderer correct if
//! the host ever offsets the widget).
//!
//! Everything here is document content. The page *paper* and *ink* do not
//! follow the OS theme — they are the document's own, exactly as a real
//! rasterised PDF renders dark glyphs on white paper regardless of the
//! desktop palette — and markup keeps the colour it is saved with. Text is
//! set at the document's own sizes ([`TextStyle::content`]), scaled by the
//! zoom alone. What the reader adds over the content (find matches, the
//! selection, selection handles) takes its colours from the theme.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Compose, Fill, ImageData as PenikoImage, Mix};

use ui::paint::{PaintCtx, with_alpha};
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;

use crate::model::{
    AnnoKind, Annotation, Block, MARGIN, MarkupStyle, PAGE_W, Page, Signature, SignatureInk, Swatch,
};

/// The document's ink: headings and signatures.
pub fn ink() -> Color {
    Color::from_rgb8(28, 33, 47)
}
/// The document's body ink.
pub fn ink_soft() -> Color {
    Color::from_rgb8(71, 85, 105)
}
/// The document's captions, rules and figure frames.
pub fn ink_faint() -> Color {
    Color::from_rgb8(148, 163, 184)
}
/// The page's own stock.
///
/// A PDF page is *document content*, not chrome: the ink above is the
/// document's ink, fixed at near-black regardless of theme, so the stock it
/// sits on has to be paper for the same reason — a themed page would leave
/// black type on a dark ground the moment the desktop went dark.
pub fn paper() -> Color {
    Color::from_rgb8(255, 255, 255)
}

/// A markup swatch's colour: the one the markup is saved with, so the page
/// on screen and the exported file agree.
pub fn swatch_color(s: Swatch) -> Color {
    let (red, green, blue) = s.rgb();
    Color::from_rgb8(red, green, blue)
}

pub fn alpha(c: Color, a: u8) -> Color {
    with_alpha(c, a)
}

/// How much of a highlighter's colour a highlight lays over the text, as the
/// saved annotation's opacity.
const HIGHLIGHT_ALPHA: u8 = 92;

/// A block's typographic spec, in points.
struct Spec {
    size: f32,
    weight: TextWeight,
    italic: bool,
    color: Color,
    /// Space after the block.
    after: f64,
    /// Indent of the text past a quote's rule.
    indent: f64,
}

impl Spec {
    /// The block's type at `scale` (screen pixels per point).
    fn style(&self, scale: f64) -> TextStyle {
        TextStyle::content(self.size * scale as f32)
            .with_weight(self.weight)
            .with_italic(self.italic)
    }
}

fn body_spec(block: &Block) -> Option<Spec> {
    let spec = |size, weight, color, after| Spec {
        size,
        weight,
        italic: false,
        color,
        after,
        indent: 0.0,
    };
    Some(match block {
        Block::Title(_) => spec(24.0, TextWeight::Bold, ink(), 6.0),
        Block::H1(_) => spec(17.0, TextWeight::Semibold, ink(), 5.0),
        Block::H2(_) => spec(13.5, TextWeight::Semibold, ink(), 3.0),
        Block::Body(_) => spec(10.5, TextWeight::Regular, ink_soft(), 6.0),
        Block::Caption(_) => spec(9.5, TextWeight::Regular, ink_faint(), 4.0),
        Block::Quote(_) => Spec {
            italic: true,
            indent: 14.0,
            ..spec(12.0, TextWeight::Regular, ink(), 8.0)
        },
        _ => return None,
    })
}

/// The type of a figure's caption, in points.
const FIGURE_CAPTION_CONTENT_PX: f32 = 9.5;

/// Break `text` into lines no wider than `max_w` in `style`, at spaces:
/// each line's byte range (trailing spaces included, so the ranges tile the
/// text). Laying lines out here, rather than handing the paragraph to the
/// engine's wrapper, gives the painter and the geometry (find matches, the
/// highlight tool's snap bands) one set of lines.
fn wrap_lines(
    tr: &mut TextRenderer,
    text: &str,
    style: &TextStyle,
    max_w: f64,
) -> Vec<std::ops::Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut end = 0;
    let mut words = text.split_inclusive(' ').peekable();
    while let Some(word) = words.next() {
        let next = end + word.len();
        let candidate = text.get(start..next).unwrap_or("").trim_end();
        let (w, _) = tr.measure_style(candidate, None, style);
        if f64::from(w) > max_w && end > start {
            lines.push(start..end);
            start = end;
        }
        end = next;
        if words.peek().is_none() {
            lines.push(start..end);
        }
    }
    if lines.is_empty() {
        lines.push(0..text.len());
    }
    lines
}

/// The drawn screen rect for the page card at the given page size (points).
pub fn page_card_rect(origin: Point, scale: f64, page_w: f64, page_h: f64) -> Rect {
    Rect::new(
        origin.x,
        origin.y,
        origin.x + page_w * scale,
        origin.y + page_h * scale,
    )
}

/// Where one page lands on screen: its top-left `origin`, the zoom `scale`
/// (pixels per point) and its size in points.
#[derive(Clone, Copy, Debug)]
pub struct PageFrame {
    pub origin: Point,
    pub scale: f64,
    pub page_w: f64,
    pub page_h: f64,
}

impl PageFrame {
    fn card_rect(&self) -> Rect {
        page_card_rect(self.origin, self.scale, self.page_w, self.page_h)
    }
}

/// The raster backend's pixels for one page: the whole-page bitmap and, for
/// a zoomed page, its sharp tiles. Both absent on the vector-text path.
#[derive(Clone, Copy)]
pub struct PageRaster<'a> {
    pub image: Option<&'a PenikoImage>,
    pub tiles: Option<&'a PageTiles>,
}

/// One find match on a page of the block model: the byte span of a block's
/// text, and whether it is the current match.
#[derive(Clone, Copy, Debug)]
pub struct FoundSpan {
    pub block: usize,
    pub start: usize,
    pub len: usize,
    pub current: bool,
}

/// The document's markup — every annotation and signature, each drawn on
/// the page it is anchored to — and the id of the selected annotation.
#[derive(Clone, Copy)]
pub struct Markup<'a> {
    pub annotations: &'a [Annotation],
    pub signatures: &'a [Signature],
    pub selected: Option<&'a str>,
}

/// The running layout cursor down a page's content column, in screen space:
/// the column's left edge and width, the zoom, and the next block's top.
struct ColumnCursor {
    x: f64,
    w: f64,
    scale: f64,
    y: f64,
}

/// One laid-out line of body text, in **page-point** space (origin =
/// page top-left, independent of zoom and scroll). The smart highlight
/// tool snaps a drag onto these bands so a marker sticks to text exactly
/// where the glyphs sit.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    /// Index of the source block on the page (for de-duping / debugging).
    pub block: usize,
    pub x: f64,
    /// Right edge of the actual glyphs on this line (text extent, not the
    /// full column), in page points.
    pub x_end: f64,
    /// Line-box top in page points.
    pub y: f64,
    pub h: f64,
    /// Each word's `[x0, x1)` extent on the line, in page points, in order.
    pub words: Vec<(f64, f64)>,
}

impl TextLine {
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.x_end, self.y + self.h)
    }

    /// Widen `[x0, x1)` to whole words: from the start of the first word it
    /// reaches into to the end of the last. Unchanged when the line has no
    /// word boxes or the span falls between words.
    pub fn snap_to_words(&self, x0: f64, x1: f64) -> (f64, f64) {
        let first = self.words.iter().find(|(_, e)| *e > x0);
        let last = self.words.iter().rev().find(|(s, _)| *s < x1);
        match (first, last) {
            (Some((s, _)), Some((_, e))) if s < e => (*s, *e),
            _ => (x0, x1),
        }
    }
}

/// The `[x0, x1)` extent of each word of `line` (set in `style`, starting at
/// `x`), measured from the line's own start.
fn word_boxes(tr: &mut TextRenderer, line: &str, style: &TextStyle, x: f64) -> Vec<(f64, f64)> {
    let mut boxes = Vec::new();
    let mut offset = 0;
    for word in line.split(' ') {
        let start = offset;
        let end = start + word.len();
        offset = end + 1;
        if word.is_empty() {
            continue;
        }
        let x0 = tr
            .measure_style(line.get(..start).unwrap_or(""), None, style)
            .0;
        let x1 = tr
            .measure_style(line.get(..end).unwrap_or(""), None, style)
            .0;
        boxes.push((x + f64::from(x0), x + f64::from(x1)));
    }
    boxes
}

/// Replay [`draw_page`]'s block layout *without drawing* to recover the
/// page-point rectangle of every rendered text line. The text engine
/// (`tr`) is needed because line breaking depends on real glyph advances,
/// so this must run where a [`TextRenderer`] is available (the draw pass).
/// It breaks lines with the same `wrap_lines` the painter uses, so the snap
/// geometry matches the painted glyphs.
pub fn collect_text_lines(tr: &mut TextRenderer, page: &Page) -> Vec<TextLine> {
    let content_x = MARGIN;
    let content_w = PAGE_W - 2.0 * MARGIN;
    let mut y = 0.0f64; // running page-point top, mirrors draw_page's `y`
    let mut out = Vec::new();
    for (bi, block) in page.blocks.iter().enumerate() {
        match block {
            Block::Spacer(h) => y += h,
            Block::Divider => y += 16.0,
            Block::Figure { height, label } => {
                y += height + 4.0;
                let style = TextStyle::content(FIGURE_CAPTION_CONTENT_PX).with_italic(true);
                let (_, ch) = tr.measure_style(label, Some(content_w as f32), &style);
                y += f64::from(ch) + 8.0;
            }
            _ => {
                let Some(spec) = body_spec(block) else {
                    continue;
                };
                let text = block.search_text().unwrap_or("");
                let style = spec.style(1.0);
                let x = content_x + spec.indent;
                let line_h = f64::from(style.line_height_px());
                let lines = wrap_lines(tr, text, &style, content_w - spec.indent);
                for (li, range) in lines.iter().enumerate() {
                    let line = text.get(range.clone()).unwrap_or("").trim_end();
                    let (w, _) = tr.measure_style(line, None, &style);
                    out.push(TextLine {
                        block: bi,
                        x,
                        x_end: (x + f64::from(w)).min(content_x + content_w),
                        y: y + li as f64 * line_h,
                        h: line_h,
                        words: word_boxes(tr, line, &style, x),
                    });
                }
                y += lines.len() as f64 * line_h + spec.after;
            }
        }
    }
    out
}

/// The glyphs of the line of `page` that holds `needle`, each with its box
/// in page points, as the page draws them: a glyph layer for the block
/// model's staged selection. Empty when no text block holds `needle`.
pub fn line_glyphs(
    tr: &mut TextRenderer,
    page: &Page,
    needle: &str,
) -> Vec<(char, crate::model::PtRect)> {
    let Some((bi, block, at)) = page.blocks.iter().enumerate().find_map(|(bi, b)| {
        let at = b.search_text()?.find(needle)?;
        Some((bi, b, at))
    }) else {
        return Vec::new();
    };
    let Some(spec) = body_spec(block) else {
        return Vec::new();
    };
    let style = spec.style(1.0);
    let text = block.search_text().unwrap_or("");
    let wrapped = wrap_lines(tr, text, &style, PAGE_W - 2.0 * MARGIN - spec.indent);
    let Some((li, range)) = wrapped
        .into_iter()
        .enumerate()
        .find(|(_, r)| r.contains(&at))
    else {
        return Vec::new();
    };
    let lines = collect_text_lines(tr, page);
    let Some(line) = lines.iter().filter(|l| l.block == bi).nth(li) else {
        return Vec::new();
    };
    let line_text = text.get(range).unwrap_or("");
    let mut out = Vec::new();
    let mut x = line.x;
    for (i, ch) in line_text.char_indices() {
        let end = i + ch.len_utf8();
        let advance = f64::from(
            tr.measure_style(line_text.get(..end).unwrap_or(""), None, &style)
                .0,
        ) + line.x
            - x;
        out.push((
            ch,
            crate::model::PtRect::new(x, line.y, advance.max(0.0), line.h),
        ));
        x += advance.max(0.0);
    }
    out
}

/// The paper a page is printed on. Returns the shape its content is clipped
/// to and its edge stroked along.
fn draw_page_paper(scene: &mut Scene, theme: &Theme, base: Affine, card: Rect) -> RoundedRect {
    let card_rr = RoundedRect::from_rect(card, f64::from(theme.tokens.radii.detail) / 2.0);
    scene.fill(Fill::NonZero, base, paper(), None, &card_rr);
    card_rr
}

pub use tilo_page_raster::tiles::PageTiles;
/// Paint the raster backend's page bitmap (engine output) fitted into the
/// card and centred — a page whose rotation or size differs from the card's
/// keeps its aspect, matching the reader's page maps
/// ([`crate::model::PageMap`]) so overlays line up.
fn draw_page_raster_image(scene: &mut Scene, base: Affine, card: Rect, img: &PenikoImage) {
    if img.width > 0 && img.height > 0 {
        let (iw, ih) = (f64::from(img.width), f64::from(img.height));
        let s = crate::model::fit_scale(card.width(), card.height(), iw, ih);
        let dx = ((card.width() - iw * s) / 2.0).max(0.0);
        let dy = ((card.height() - ih * s) / 2.0).max(0.0);
        let img_tf = base
            * Affine::translate((card.x0 + dx, card.y0 + dy))
            * Affine::scale_non_uniform(s, s);
        scene.draw_image(img, img_tf);
    }
}

/// The edge that seats the paper on the canvas. The paper is document
/// content (always white); the edge is measured from the theme against it,
/// so it reads on every ground, high contrast included.
fn page_card_edge(theme: &Theme) -> Color {
    theme.tokens.colors.ink_on(
        paper(),
        theme.tokens.colors.border_standard,
        ui::tokens::NON_TEXT_MIN_CONTRAST,
    )
}

fn draw_page_border(scene: &mut Scene, theme: &Theme, base: Affine, card_rr: &RoundedRect) {
    scene.stroke(
        &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
        base,
        page_card_edge(theme),
        None,
        card_rr,
    );
}

/// The annotations + signatures anchored to `page`, drawn on top of its
/// content — shared by the raster and vector-text draw paths.
fn draw_page_markup(ctx: &mut PaintCtx<'_>, page: &Page, frame: PageFrame, markup: Markup<'_>) {
    let (origin, scale) = (frame.origin, frame.scale);
    for a in markup.annotations.iter().filter(|a| a.page == page.number) {
        draw_annotation(
            ctx,
            a,
            origin,
            scale,
            markup.selected == Some(a.id.as_str()),
        );
    }
    for s in markup.signatures.iter().filter(|s| s.page == page.number) {
        draw_signature(ctx.scene, ctx.text, ctx.transform, s, origin, scale);
    }
}

/// Draw a figure block (frame, diagram, caption) `height` points tall at the
/// cursor, and advance the cursor past it.
fn draw_page_figure(ctx: &mut PaintCtx<'_>, at: &mut ColumnCursor, height: f64, label: &str) {
    let (scene, base, tr) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text);
    let (content_x, content_w, y, scale) = (at.x, at.w, at.y, at.scale);
    let h = height * scale;
    let box_rect = Rect::new(content_x, y, content_x + content_w, y + h);
    let frame = RoundedRect::from_rect(box_rect, 2.0 * scale);
    scene.fill(Fill::NonZero, base, alpha(ink_faint(), 28), None, &frame);
    scene.stroke(
        &Stroke::new(scale.max(0.5)),
        base,
        ink_faint(),
        None,
        &frame,
    );
    draw_figure_glyph(scene, base, box_rect, scale);
    let y = y + h + 4.0 * scale;
    let style = TextStyle::content(FIGURE_CAPTION_CONTENT_PX * scale as f32).with_italic(true);
    tr.draw_style(
        scene,
        label,
        ink_faint(),
        base * Affine::translate((content_x, y)),
        Some(content_w as f32),
        &style,
    );
    let (_, ch) = tr.measure_style(label, Some(content_w as f32), &style);
    at.y = y + f64::from(ch) + 8.0 * scale;
}

/// Draw one heading/body/quote/caption block's text at the cursor, line by
/// line, with its find matches over exactly their glyphs, and advance the
/// cursor past it.
fn draw_page_text_block(
    ctx: &mut PaintCtx<'_>,
    at: &mut ColumnCursor,
    block: &Block,
    spec: &Spec,
    found: &[FoundSpan],
) {
    let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let (content_x, content_w, y, scale) = (at.x, at.w, at.y, at.scale);
    let text = block.search_text().unwrap_or("");
    // Lines break at the document's own size, so they break where the
    // highlight tool's snap bands say; each is then set at the zoomed size.
    let lines = wrap_lines(tr, text, &spec.style(1.0), content_w / scale - spec.indent);
    let style = spec.style(scale);
    let line_h = f64::from(style.line_height_px());
    let x = content_x + spec.indent * scale;

    if matches!(block, Block::Quote(_)) {
        let rule = Rect::new(
            content_x,
            y,
            content_x + 3.0 * scale,
            y + line_h * lines.len() as f64,
        );
        scene.fill(Fill::NonZero, base, ink_faint(), None, &rule);
    }
    for (li, range) in lines.iter().enumerate() {
        let ly = y + li as f64 * line_h;
        let line = text.get(range.clone()).unwrap_or("");
        // Find matches on this line: one highlight over the matched glyphs,
        // measured from the line's own start.
        for m in found {
            let (s, e) = (m.start.max(range.start), (m.start + m.len).min(range.end));
            if s >= e {
                continue;
            }
            let x0 = tr
                .measure_style(text.get(range.start..s).unwrap_or(""), None, &style)
                .0;
            let x1 = tr
                .measure_style(text.get(range.start..e).unwrap_or(""), None, &style)
                .0;
            ui::widget::paint_find_match(
                scene,
                base,
                Rect::new(x + f64::from(x0), ly, x + f64::from(x1), ly + line_h),
                m.current,
                theme,
            );
        }
        tr.draw_style(
            scene,
            line.trim_end(),
            spec.color,
            base * Affine::translate((x, ly)),
            None,
            &style,
        );
    }
    at.y = y + line_h * lines.len() as f64 + spec.after * scale;
}

/// Lay out and draw every content block of a vector (non-raster) page —
/// spacers, dividers, figures and styled text — updating the running
/// screen-space `y` as it goes.
fn draw_page_blocks(ctx: &mut PaintCtx<'_>, page: &Page, frame: PageFrame, found: &[FoundSpan]) {
    let (origin, scale) = (frame.origin, frame.scale);
    let mut at = ColumnCursor {
        x: origin.x + MARGIN * scale,
        w: (frame.page_w - 2.0 * MARGIN) * scale,
        scale,
        y: origin.y,
    };

    for (bi, block) in page.blocks.iter().enumerate() {
        match block {
            Block::Spacer(h) => {
                at.y += h * scale;
            }
            Block::Divider => {
                let dy = at.y + 7.0 * scale;
                let line = Line::new((at.x, dy), (at.x + at.w, dy));
                ctx.scene.stroke(
                    &Stroke::new(scale.max(0.5)),
                    ctx.transform,
                    ink_faint(),
                    None,
                    &line,
                );
                at.y += 16.0 * scale;
            }
            Block::Figure { height, label } => {
                draw_page_figure(ctx, &mut at, *height, label);
            }
            _ => {
                let Some(spec) = body_spec(block) else {
                    continue;
                };
                let spans: Vec<FoundSpan> =
                    found.iter().filter(|m| m.block == bi).copied().collect();
                draw_page_text_block(ctx, &mut at, block, &spec, &spans);
            }
        }
    }
}

/// Draw one page: paper, content, find matches, then the annotations +
/// signatures that live on it.
pub fn draw_page(
    ctx: &mut PaintCtx<'_>,
    page: &Page,
    frame: PageFrame,
    raster: PageRaster<'_>,
    found: &[FoundSpan],
    markup: Markup<'_>,
) {
    let base = ctx.transform;
    let card = frame.card_rect();
    let card_rr = draw_page_paper(ctx.scene, ctx.theme, base, card);

    // Clip content to the paper.
    ctx.scene
        .push_layer(Fill::NonZero, BlendMode::default(), 1.0, base, &card_rr);

    // Raster backend: paint the page bitmap (engine output) scaled to fill the
    // card width — for a zoomed page, a lower-resolution placeholder under
    // its sharp tiles — then let the shared markup overlay draw on top.
    if raster.image.is_some() || raster.tiles.is_some() {
        if let Some(img) = raster.image {
            draw_page_raster_image(ctx.scene, base, card, img);
        }
        if let Some(tiles) = raster.tiles {
            tiles.paint(ctx.scene, base);
        }
    } else {
        draw_page_blocks(ctx, page, frame, found);
    }
    draw_page_markup(ctx, page, frame, markup);

    ctx.scene.pop_layer();
    draw_page_border(ctx.scene, ctx.theme, base, &card_rr);
}

/// The sample document's diagram: two nodes and the link between them.
fn draw_figure_glyph(scene: &mut Scene, base: Affine, b: Rect, scale: f64) {
    let cy = f64::midpoint(b.y0, b.y1);
    let w = b.width();
    let r = (b.height().min(w) * 0.16).max(8.0 * scale);
    let lx = b.x0 + w * 0.32;
    let rx = b.x0 + w * 0.68;
    let line_w = 1.5 * scale;
    let conn = Line::new((lx + r, cy), (rx - r, cy));
    scene.stroke(&Stroke::new(line_w), base, ink_soft(), None, &conn);
    for cx in [lx, rx] {
        let node = RoundedRect::from_rect(Rect::new(cx - r, cy - r, cx + r, cy + r), r * 0.25);
        scene.stroke(&Stroke::new(line_w), base, ink_soft(), None, &node);
    }
}

fn pt(origin: Point, scale: f64, x: f64, y: f64) -> Point {
    Point::new(origin.x + x * scale, origin.y + y * scale)
}

/// The selection frame round a chosen annotation: an outline and four
/// corner handles in your colour.
fn draw_selected_frame(scene: &mut Scene, theme: &Theme, base: Affine, r: Rect) {
    let you = theme.tokens.colors.intent.you;
    scene.stroke(&Stroke::new(1.5), base, you, None, &r);
    let hs = f64::from(theme.tokens.spacing.s1) * 0.75;
    for (cx, cy) in [(r.x0, r.y0), (r.x1, r.y0), (r.x0, r.y1), (r.x1, r.y1)] {
        let h = Rect::new(cx - hs, cy - hs, cx + hs, cy + hs);
        scene.fill(Fill::NonZero, base, you, None, &h);
    }
}

fn draw_annotation(
    ctx: &mut PaintCtx<'_>,
    a: &Annotation,
    origin: Point,
    scale: f64,
    selected: bool,
) {
    let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let col = swatch_color(a.color);
    match &a.kind {
        AnnoKind::Markup { style, quads } => {
            let mut bounds: Option<Rect> = None;
            for rect in quads {
                let p0 = pt(origin, scale, rect.x, rect.y);
                let r = Rect::new(p0.x, p0.y, p0.x + rect.w * scale, p0.y + rect.h * scale);
                match style {
                    MarkupStyle::Highlight => {
                        fill_highlight(scene, base, col, r);
                    }
                    MarkupStyle::Underline | MarkupStyle::StrikeOut => {
                        // A line at the baseline (underline) or through the
                        // middle (strike-out) of each band.
                        let y = if *style == MarkupStyle::Underline {
                            r.y0 + r.height() * 0.9
                        } else {
                            r.y0 + r.height() * 0.55
                        };
                        let line = Line::new((r.x0, y), (r.x1, y));
                        scene.stroke(&Stroke::new((1.4 * scale).max(1.0)), base, col, None, &line);
                    }
                    MarkupStyle::Squiggly => {
                        // A wave along the baseline.
                        let y = r.y0 + r.height() * 0.9;
                        let step = (2.0 * scale).max(1.5);
                        let mut wave = BezPath::new();
                        wave.move_to((r.x0, y));
                        let steps = (r.width() / step).ceil().clamp(1.0, 4096.0) as usize;
                        for i in 1..=steps {
                            let x = (r.x0 + step * i as f64).min(r.x1);
                            let crest = if i % 2 == 1 { y - step * 0.6 } else { y };
                            wave.line_to((x, crest));
                        }
                        scene.stroke(&Stroke::new(scale.max(0.8)), base, col, None, &wave);
                    }
                }
                bounds = Some(bounds.map_or(r, |b| b.union(r)));
            }
            if selected && let Some(r) = bounds {
                draw_selected_frame(scene, theme, base, r);
            }
        }
        AnnoKind::Ink { strokes } => {
            for points in strokes {
                let (Some(&(fx, fy)), Some(&(lx, ly))) = (points.first(), points.last()) else {
                    continue;
                };
                if points.len() < 2 {
                    continue;
                }
                let mut path = BezPath::new();
                path.move_to(pt(origin, scale, fx, fy));
                for w in points.windows(2) {
                    let [(x0, y0), (x1, y1)] = w else { continue };
                    let a0 = pt(origin, scale, *x0, *y0);
                    let b0 = pt(origin, scale, *x1, *y1);
                    let mid = Point::new(f64::midpoint(a0.x, b0.x), f64::midpoint(a0.y, b0.y));
                    path.quad_to(a0, mid);
                }
                path.line_to(pt(origin, scale, lx, ly));
                scene.stroke(&ink_stroke(scale), base, col, None, &path);
            }
        }
        AnnoKind::Note { x, y } => {
            let p = pt(origin, scale, *x, *y);
            let r = draw_note_marker(scene, base, p, col, scale);
            if selected {
                draw_selected_frame(scene, theme, base, r.inflate(2.0, 2.0));
            }
        }
        AnnoKind::FreeText { rect, text, size } => {
            let p0 = pt(origin, scale, rect.x, rect.y);
            let r = Rect::new(p0.x, p0.y, p0.x + rect.w * scale, p0.y + rect.h * scale);
            draw_free_text(scene, tr, base, r, text, (*size * scale) as f32, col);
            if selected {
                draw_selected_frame(scene, theme, base, r);
            }
        }
    }
}

/// The pen a freehand stroke is drawn with at `scale`.
fn ink_stroke(scale: f64) -> Stroke {
    Stroke::new((2.4 * scale).max(1.5))
        .with_caps(vello::kurbo::Cap::Round)
        .with_join(vello::kurbo::Join::Round)
}

/// Free text drawn in its box: wrapped to the box width from its top-left,
/// clipped to the box, with the same inner padding the exported appearance
/// uses.
pub fn draw_free_text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    base: Affine,
    r: Rect,
    text: &str,
    size: f32,
    color: Color,
) {
    /// In points at 100 %, as in the exported appearance.
    const PADDING: f64 = 2.0; // PDF points inside the annotation, not UI spacing
    if text.is_empty() || !(size.is_finite() && size > 0.5) {
        return;
    }
    let zoom = f64::from(size) / crate::model::FREE_TEXT_SIZE;
    let pad = PADDING * zoom.max(0.25);
    let inner = Rect::new(r.x0 + pad, r.y0 + pad, r.x1 - pad, r.y1 - pad);
    if inner.width() <= 0.0 || inner.height() <= 0.0 {
        return;
    }
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, base, &r);
    tr.draw_style(
        scene,
        text,
        color,
        base * Affine::translate((inner.x0, inner.y0)),
        Some(inner.width() as f32),
        &TextStyle::content(size),
    );
    scene.pop_layer();
}

/// A sticky-note marker at `p`, in the note's colour: a square with a folded
/// corner and three ruled lines. Returns its rect.
fn draw_note_marker(scene: &mut Scene, base: Affine, p: Point, col: Color, scale: f64) -> Rect {
    let s = (22.0 * scale).max(16.0);
    let r = Rect::new(p.x, p.y, p.x + s, p.y + s);
    scene.fill(
        Fill::NonZero,
        base,
        col,
        None,
        &RoundedRect::from_rect(r, s * 0.14),
    );
    let fold = {
        let mut path = BezPath::new();
        path.move_to((r.x1 - s * 0.36, r.y1));
        path.line_to((r.x1, r.y1 - s * 0.36));
        path.line_to((r.x1, r.y1));
        path.close_path();
        path
    };
    scene.fill(Fill::NonZero, base, alpha(ink(), 46), None, &fold);
    for i in 0..3 {
        let ly = r.y0 + s * (0.32 + f64::from(i) * 0.18);
        let line = Line::new((r.x0 + s * 0.2, ly), (r.x1 - s * 0.2, ly));
        scene.stroke(
            &Stroke::new((1.2 * scale).max(0.8)),
            base,
            alpha(paper(), 220),
            None,
            &line,
        );
    }
    r
}

/// A placed signature: the signer's ink above a rule, their name and the
/// date beneath it — all in the document's ink at the document's own sizes,
/// scaled by the zoom like the rest of the page.
fn draw_signature(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    base: Affine,
    s: &Signature,
    origin: Point,
    scale: f64,
) {
    let p0 = pt(origin, scale, s.rect.x, s.rect.y);
    let r = Rect::new(p0.x, p0.y, p0.x + s.rect.w * scale, p0.y + s.rect.h * scale);
    let stroke_area = Rect::new(
        r.x0 + r.width() * 0.06,
        r.y0 + r.height() * 0.10,
        r.x1 - r.width() * 0.06,
        r.y0 + r.height() * 0.56,
    );
    draw_signature_ink(scene, tr, base, ink(), stroke_area, &s.ink, &s.signer);

    let inset = 8.0 * scale;
    let base_y = r.y0 + r.height() * 0.62;
    let baseline = Line::new((r.x0 + inset, base_y), (r.x1 - inset, base_y));
    scene.stroke(
        &Stroke::new(scale.max(0.5)),
        base,
        ink_faint(),
        None,
        &baseline,
    );

    let width = Some((r.width() - 2.0 * inset).max(1.0) as f32);
    let name = TextStyle::content(8.5 * scale as f32).with_weight(TextWeight::Semibold);
    let name_y = base_y + 3.0 * scale;
    tr.draw_style(
        scene,
        &s.signer,
        ink_soft(),
        base * Affine::translate((r.x0 + inset, name_y)),
        width,
        &name,
    );
    let date = TextStyle::content(7.5 * scale as f32);
    tr.draw_style(
        scene,
        &format!("Signed {}", s.date),
        ink_faint(),
        base * Affine::translate((r.x0 + inset, name_y + f64::from(name.line_height_px()))),
        width,
        &date,
    );
}

/// Draw a signature's **real ink** inside `area` (no field chrome): the signer's
/// typed name in a script style, or the freehand strokes they actually drew
/// (normalised `[0,1]²`, scaled into `area`). Shared by the placed-signature
/// draw and the composer's preview, so both show exactly what will be stamped.
pub fn draw_signature_ink(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    base: Affine,
    color: Color,
    area: Rect,
    ink: &SignatureInk,
    signer: &str,
) {
    if area.width() <= 1.0 || area.height() <= 1.0 {
        return;
    }
    match ink {
        SignatureInk::Typed => {
            let name = if signer.trim().is_empty() {
                "Signature"
            } else {
                signer.trim()
            };
            // A signature script: an italic serif face, sized to the field and
            // shrunk to fit its width.
            let script = |size: f32| {
                TextStyle::content(size)
                    .with_weight(TextWeight::Medium)
                    .with_italic(true)
                    .with_family(FontChoice::Serif)
            };
            let mut style = script((area.height() * 0.85) as f32);
            let maxw = area.width() as f32;
            let (mut tw, mut th) = tr.measure_style(name, None, &style);
            if tw > maxw && tw > 0.0 {
                style = script(style.size * maxw / tw);
                (tw, th) = tr.measure_style(name, None, &style);
            }
            let tx = area.x0 + f64::from((area.width() as f32 - tw).max(0.0) / 2.0);
            let ty = area.y0 + (area.height() - f64::from(th)) / 2.0;
            tr.draw_style(
                scene,
                name,
                color,
                base * Affine::translate((tx, ty)),
                None,
                &style,
            );
        }
        SignatureInk::Drawn { strokes } => {
            let w = (area.height() * 0.06).clamp(1.4, 3.0);
            for stroke in strokes {
                if stroke.len() < 2 {
                    continue;
                }
                let mut path = BezPath::new();
                for (i, (nx, ny)) in stroke.iter().enumerate() {
                    let x = area.x0 + nx.clamp(0.0, 1.0) * area.width();
                    let y = area.y0 + ny.clamp(0.0, 1.0) * area.height();
                    if i == 0 {
                        path.move_to((x, y));
                    } else {
                        path.line_to((x, y));
                    }
                }
                scene.stroke(
                    &Stroke::new(w)
                        .with_caps(vello::kurbo::Cap::Round)
                        .with_join(vello::kurbo::Join::Round),
                    base,
                    color,
                    None,
                    &path,
                );
            }
        }
    }
}

// ----------------------------------------------------------------------
// Overlays the reader draws over the content
// ----------------------------------------------------------------------

/// A highlight being dragged out, as the committed highlight will look.
pub fn draw_highlight_preview(scene: &mut Scene, base: Affine, col: Color, r: Rect) {
    fill_highlight(scene, base, col, r);
}

/// One highlighted band, laid on the way a highlighter marks paper: its
/// colour multiplies into what is under it, so the paper takes the colour
/// and the ink over it stays as dark as it was printed.
fn fill_highlight(scene: &mut Scene, base: Affine, col: Color, r: Rect) {
    scene.push_layer(
        Fill::NonZero,
        BlendMode::new(Mix::Multiply, Compose::SrcOver),
        1.0,
        base,
        &r,
    );
    scene.fill(Fill::NonZero, base, alpha(col, HIGHLIGHT_ALPHA), None, &r);
    scene.pop_layer();
}

/// A freehand stroke being drawn, through screen `points`.
pub fn draw_ink_preview(scene: &mut Scene, base: Affine, col: Color, points: &[Point], scale: f64) {
    let Some(first) = points.first() else {
        return;
    };
    let mut path = BezPath::new();
    path.move_to(*first);
    for p in points.get(1..).into_iter().flatten() {
        path.line_to(*p);
    }
    scene.stroke(&ink_stroke(scale), base, col, None, &path);
}

/// The live text selection: your selection tint over each line band.
pub fn draw_selection_bands(scene: &mut Scene, base: Affine, theme: &Theme, bands: &[Rect]) {
    for band in bands {
        scene.fill(
            Fill::NonZero,
            base,
            theme.tokens.colors.intent.selection_tint,
            None,
            band,
        );
    }
}

/// The image-region marquee (Shift + Select): your selection tint with a
/// dashed edge in your colour.
pub fn draw_region_marquee(scene: &mut Scene, base: Affine, theme: &Theme, r: Rect) {
    let intent = &theme.tokens.colors.intent;
    scene.fill(Fill::NonZero, base, intent.selection_tint, None, &r);
    let dash = f64::from(theme.tokens.spacing.s1);
    scene.stroke(
        &Stroke::new(1.5).with_dashes(0.0, [dash + 1.0, dash]),
        base,
        intent.you,
        None,
        &r,
    );
}

/// The box a form field or free text is edited in: the paper, so the
/// document's own appearance underneath does not show through the typing.
pub fn draw_edit_box(scene: &mut Scene, base: Affine, r: Rect) {
    scene.fill(Fill::NonZero, base, paper(), None, &r);
}

/// The value a form field holds in the document's record.
pub enum FieldValue {
    Check(bool),
    Text(String),
}

/// A form field on the page: a quiet outline in your selection tint so the
/// field is discoverable, and the value the record holds drawn over the
/// PDF's own appearance (the export writes the same value into the file).
pub fn draw_form_field(ctx: &mut PaintCtx<'_>, r: Rect, zoom: f64, value: Option<&FieldValue>) {
    let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    scene.stroke(
        &Stroke::new(1.0),
        base,
        theme.tokens.colors.intent.selection_tint,
        None,
        &r,
    );
    let Some(value) = value else {
        return;
    };
    // Cover the document's own appearance with the recorded value.
    scene.fill(Fill::NonZero, base, paper(), None, &r.inflate(-1.0, -1.0));
    match value {
        FieldValue::Check(checked) => {
            if *checked {
                let inset = r.inflate(-r.width() * 0.22, -r.height() * 0.22);
                let mut tick = BezPath::new();
                tick.move_to((inset.x0, inset.center().y));
                tick.line_to((inset.x0 + inset.width() * 0.4, inset.y1));
                tick.line_to((inset.x1, inset.y0));
                scene.stroke(
                    &Stroke::new((r.height() * 0.12).max(1.2)),
                    base,
                    ink(),
                    None,
                    &tick,
                );
            }
        }
        FieldValue::Text(text) => {
            // Set like a PDF's auto-sized field text: a share of the field's
            // height.
            let style = TextStyle::content((r.height() * 0.6) as f32);
            let (_, h) = tr.measure_style(text, None, &style);
            tr.draw_style(
                scene,
                text,
                ink(),
                base * Affine::translate((r.x0 + 3.0 * zoom, r.center().y - f64::from(h) / 2.0)),
                Some((r.width() - 6.0 * zoom).max(1.0) as f32),
                &style,
            );
        }
    }
}

// ----------------------------------------------------------------------
// Thumbnails — the page's content in miniature
// ----------------------------------------------------------------------

/// A miniature of the same page content and saved markup as the canvas.
/// The rail supplies the frame, page number and current-page indication.
pub struct Thumbnail<'a> {
    pub page: &'a Page,
    pub page_w: f64,
    pub page_h: f64,
    pub raster: Option<&'a PenikoImage>,
    pub markup: Markup<'a>,
}

impl Thumbnail<'_> {
    pub fn draw(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let scale = crate::model::fit_scale(rect.width(), rect.height(), self.page_w, self.page_h);
        let origin = Point::new(
            rect.center().x - self.page_w * scale / 2.0,
            rect.center().y - self.page_h * scale / 2.0,
        );
        // Lay out at the document's own point size, then scale the complete
        // page scene. Tiny font sizes must not change wrapping or glyph positions.
        let transform =
            ctx.transform * Affine::translate((origin.x, origin.y)) * Affine::scale(scale);
        let text_scale = ctx.text.text_scale();
        ctx.text.set_text_scale(1.0);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &rect,
        );
        draw_page(
            &mut PaintCtx::new(ctx.scene, transform, ctx.text, ctx.theme),
            self.page,
            PageFrame {
                origin: Point::ZERO,
                scale: 1.0,
                page_w: self.page_w,
                page_h: self.page_h,
            },
            PageRaster {
                image: self.raster,
                tiles: None,
            },
            &[],
            self.markup,
        );
        ctx.scene.pop_layer();
        ctx.text.set_text_scale(text_scale);
    }
}

/// Crop a rectangular region (in page-point space) out of a rendered page
/// bitmap, returning `(width_px, height_px, rgba)` ready for the clipboard.
/// The page bitmap covers the whole page (`page_w × page_h` points) at
/// `img.width × img.height` pixels, so the region maps by a simple scale.
/// Returns `None` for an empty region or a malformed bitmap.
pub fn crop_page_region(
    img: &PenikoImage,
    page_w: f64,
    page_h: f64,
    rect: crate::model::PtRect,
) -> Option<(usize, usize, Vec<u8>)> {
    let iw = img.width as usize;
    let ih = img.height as usize;
    if iw == 0 || ih == 0 || page_w <= 0.0 || page_h <= 0.0 {
        return None;
    }
    let sx = iw as f64 / page_w;
    let sy = ih as f64 / page_h;
    let x0 = ((rect.x * sx).floor().max(0.0) as usize).min(iw);
    let y0 = ((rect.y * sy).floor().max(0.0) as usize).min(ih);
    let x1 = ((((rect.x + rect.w) * sx).ceil()).max(0.0) as usize).min(iw);
    let y1 = ((((rect.y + rect.h) * sy).ceil()).max(0.0) as usize).min(ih);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let src = img.data.data();
    if src.len() < iw * ih * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(cw * ch * 4);
    for row in y0..y1 {
        let start = (row * iw + x0) * 4;
        let Some(slice) = src.get(start..start + cw * 4) else {
            // Unreachable given the length check above; bail rather than
            // paint a partial/garbage crop from a malformed bitmap.
            return None;
        };
        out.extend_from_slice(slice);
    }
    Some((cw, ch, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_document;

    /// The page card's outline is chrome: it must clear the non-text floor
    /// on the paper under every shipped theme, including high contrast.
    #[test]
    fn page_card_edge_is_chrome_and_reads_on_the_paper() {
        use ui::a11y::contrast_ratio;
        use ui::tokens::NON_TEXT_MIN_CONTRAST;
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped theme resolves");
            let edge = page_card_edge(&theme);
            assert!(
                contrast_ratio(edge, paper()) >= NON_TEXT_MIN_CONTRAST,
                "{name} page edge must read on the paper, got {:.2}:1",
                contrast_ratio(edge, paper())
            );
        }
    }

    #[test]
    fn collect_text_lines_produces_bands_inside_the_page() {
        let mut tr = TextRenderer::new();
        let doc = sample_document();
        let page = doc.page(1).unwrap();
        let lines = collect_text_lines(&mut tr, page);
        assert!(!lines.is_empty(), "page 1 has body text -> some lines");
        for l in &lines {
            assert!(l.x >= MARGIN - 0.5, "line left {} inside margin", l.x);
            assert!(
                l.x_end <= PAGE_W - MARGIN + 0.5,
                "line right {} inside margin",
                l.x_end
            );
            assert!(
                l.y >= 0.0 && l.y + l.h <= crate::model::PAGE_H,
                "line within page height"
            );
            assert!(l.h > 0.0);
        }
    }

    /// Lines tile the text and none is wider than the column, except a
    /// single word that cannot break.
    #[test]
    fn wrapped_lines_tile_the_text_and_fit_the_column() {
        let mut tr = TextRenderer::new();
        let style = TextStyle::content(10.5);
        let text = "A collaborative reader keeps every comment, highlight and signature \
                    in one shared document so reviewers never merge copies by hand.";
        let lines = wrap_lines(&mut tr, text, &style, 180.0);
        assert!(lines.len() > 1, "a long paragraph wraps");
        let mut next = 0;
        for range in &lines {
            assert_eq!(range.start, next, "lines tile the text");
            next = range.end;
            let line = text.get(range.clone()).unwrap().trim_end();
            let (w, _) = tr.measure_style(line, None, &style);
            assert!(
                f64::from(w) <= 180.0 || !line.contains(' '),
                "{line:?} is {w} wide"
            );
        }
        assert_eq!(next, text.len());
    }
}
