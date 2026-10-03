//! Page rendering: lays a [`crate::model::Page`] out with the toolkit's
//! text engine and paints it onto a white "paper" card, then overlays
//! annotations and signatures.
//!
//! Coordinate convention: page geometry is in PDF points; `scale` is
//! pixels-per-point (1.0 == 100% zoom). A point `(px, py)` on the page
//! maps to screen `origin + (px, py) * scale`, then the caller's `base`
//! affine is applied (it is identity for the full-window root and the
//! offscreen shot harness, but threading it keeps the renderer correct if
//! the host ever offsets the widget).
//!
//! The page *paper* (white) and *ink* (dark) deliberately do not follow
//! the OS theme — they represent the document's own content, exactly as a
//! real rasterised PDF renders dark glyphs on white paper regardless of
//! the desktop palette. Everything that is *chrome* is fully theme-driven;
//! see `app_draw.rs`.

use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill, ImageData as PenikoImage};

use ui::paint::{PaintCtx, with_alpha};
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;

use crate::model::{
    AnnoKind, Annotation, Block, MARGIN, MarkupStyle, PAGE_W, Page, Signature, SignatureInk, Swatch,
};

pub fn ink() -> Color {
    Color::from_rgb8(28, 33, 47)
}
pub fn ink_soft() -> Color {
    Color::from_rgb8(71, 85, 105)
}
pub fn ink_faint() -> Color {
    Color::from_rgb8(148, 163, 184)
}
/// The page's own stock.
///
/// A PDF page is *document content*, not chrome: the ink above is the
/// document's ink, fixed at near-black regardless of theme, so the stock it
/// sits on has to be paper for the same reason — a themed page would leave
/// black type on a dark ground the moment the desktop went dark. It used to
/// borrow `surfaces.window_card`, which happened to be light on the default
/// theme; that made the page's legibility depend on the colour of the
/// *window frame*, which is not something a document has an opinion about.
pub fn paper(_theme: &Theme) -> Color {
    Color::WHITE
}

/// Resolve an annotation swatch into a concrete colour. Pulls from the
/// theme's semantic + accent ramps so a palette swap keeps markup
/// coherent with the rest of the UI.
pub fn swatch_color(theme: &Theme, s: Swatch) -> Color {
    let c = &theme.tokens.colors;
    match s {
        Swatch::Yellow => c.semantic.warning,
        Swatch::Green => c.semantic.success,
        Swatch::Pink => c.semantic.danger,
        Swatch::Blue => c.semantic.info,
        Swatch::Purple => c.accent[ACCENT_PURPLE],
        Swatch::Custom(_) => {
            let (red, green, blue) = s.rgb();
            Color::from_rgb8(red, green, blue)
        }
    }
}
const ACCENT_PURPLE: usize = 6; // indigo-400, reads as a violet ink

pub fn alpha(c: Color, a: u8) -> Color {
    with_alpha(c, a)
}

/// Per-block typographic spec, in points.
struct Spec {
    size: f32,
    weight: TextWeight,
    italic: bool,
    color: Color,
    after: f64,
}

fn body_spec(block: &Block, theme: &Theme) -> Option<Spec> {
    Some(match block {
        Block::Title(_) => Spec {
            size: 24.0,
            weight: TextWeight::Bold,
            italic: false,
            color: ink(),
            after: 6.0,
        },
        Block::H1(_) => Spec {
            size: 17.0,
            weight: TextWeight::Semibold,
            italic: false,
            color: ink(),
            after: 5.0,
        },
        Block::H2(_) => Spec {
            size: 13.5,
            weight: TextWeight::Semibold,
            italic: false,
            color: ink(),
            after: 3.0,
        },
        Block::Body(_) => Spec {
            size: 10.5,
            weight: TextWeight::Regular,
            italic: false,
            color: ink_soft(),
            after: 6.0,
        },
        Block::Caption(_) => Spec {
            size: 9.5,
            weight: TextWeight::Regular,
            italic: false,
            color: ink_faint(),
            after: 4.0,
        },
        Block::Quote(_) => Spec {
            size: 12.0,
            weight: TextWeight::Regular,
            italic: true,
            color: theme.accent_hover,
            after: 8.0,
        },
        _ => return None,
    })
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

/// A page's search matches: the blocks that matched, and the one holding
/// the active match.
#[derive(Clone, Copy)]
pub struct SearchBands<'a> {
    pub matched: &'a HashSet<usize>,
    pub active: Option<usize>,
}

impl SearchBands<'_> {
    fn band(&self, block: usize) -> Option<MatchBand> {
        if !self.matched.contains(&block) {
            None
        } else if self.active == Some(block) {
            Some(MatchBand::Active)
        } else {
            Some(MatchBand::Match)
        }
    }
}

/// How a matched block's search band is painted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MatchBand {
    Match,
    Active,
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextLine {
    /// Index of the source block on the page (for de-duping / debugging).
    pub block: usize,
    /// Left edge of the text column for this line, in page points.
    pub x: f64,
    /// Right edge of the actual glyphs on this line (text extent, not the
    /// full column), in page points.
    pub x_end: f64,
    /// Baseline-box top in page points.
    pub y: f64,
    /// Line height in page points.
    pub h: f64,
}

impl TextLine {
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.x_end, self.y + self.h)
    }
}

/// Replay [`draw_page`]'s block layout *without drawing* to recover the
/// page-point rectangle of every rendered text line. The text engine
/// (`tr`) is needed because line wrapping depends on real glyph advances,
/// so this must run where a [`TextRenderer`] is available (the draw pass).
/// Keeping it as a pure function next to `draw_page` guarantees the snap
/// geometry matches the painted glyphs.
pub fn collect_text_lines(tr: &mut TextRenderer, theme: &Theme, page: &Page) -> Vec<TextLine> {
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
                let csize = 9.5f32;
                let (_, ch) = tr.measure(label, csize, Some(content_w as f32));
                y += f64::from(ch) + 8.0;
            }
            _ => {
                let Some(spec) = body_spec(block, theme) else {
                    continue;
                };
                let text = block.search_text().unwrap_or("");
                let size = spec.size; // page-point size (scale == 1 here)
                let max_w = content_w as f32;
                let (line_w, th) = tr.measure(text, size, Some(max_w));
                // Quote blocks indent their text past a left rule.
                let x = if matches!(block, Block::Quote(_)) {
                    content_x + 14.0
                } else {
                    content_x
                };
                let line_h = f64::from(size * 1.25);
                let n_lines = if line_h > 0.0 {
                    (f64::from(th) / line_h).round().max(1.0) as usize
                } else {
                    1
                };
                for li in 0..n_lines {
                    // The final wrapped line is usually shorter; only its
                    // measured width is known precisely, so the last line
                    // uses `line_w` and full lines span the column.
                    let x_end = if li == n_lines - 1 {
                        (x + f64::from(line_w)).min(content_x + content_w)
                    } else {
                        content_x + content_w
                    };
                    out.push(TextLine {
                        block: bi,
                        x,
                        x_end,
                        y: y + li as f64 * line_h,
                        h: line_h,
                    });
                }
                y += f64::from(th) + spec.after;
            }
        }
    }
    out
}

/// Shadow + paper backdrop for a page card. Returns the rounded rect used to
/// clip its content and to stroke its border.
fn draw_page_backdrop(scene: &mut Scene, theme: &Theme, base: Affine, card: Rect) -> RoundedRect {
    // Soft drop shadow (two offset translucent slabs).
    for (off, a) in [(7.0, 22u8), (3.0, 30u8)] {
        let s = RoundedRect::new(card.x0, card.y0 + off, card.x1, card.y1 + off, 3.0);
        scene.fill(Fill::NonZero, base, Color::from_rgba8(0, 0, 0, a), None, &s);
    }
    let card_rr = RoundedRect::from_rect(card, 3.0);
    scene.fill(Fill::NonZero, base, paper(theme), None, &card_rr);
    card_rr
}

/// Paint the raster backend's page bitmap (engine output) fitted into the
/// card and centred — a page whose rotation or size differs from the card's
/// keeps its aspect, matching the reader's page maps
/// ([`crate::model::PageMap`]) so overlays line up.
/// The tiles available for a zoomed page this frame, and where the page's
/// pixels at their zoom level land on screen.
#[derive(Clone, Debug)]
pub struct PageTiles {
    pub placement: crate::tiles::PagePlacement,
    pub tiles: Vec<(tilo_pdf::PixelRect, PenikoImage)>,
}

/// Paint a zoomed page's tiles, each at its own pixel region, over whatever
/// placeholder is already drawn.
fn draw_page_tiles(scene: &mut Scene, base: Affine, tiles: &PageTiles) {
    let k = tiles.placement.screen_per_px;
    for (region, image) in &tiles.tiles {
        let (x0, y0, _, _) = tiles.placement.screen_rect(*region);
        scene.draw_image(
            image,
            base * Affine::translate((x0, y0)) * Affine::scale_non_uniform(k, k),
        );
    }
}

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

/// Stroke a page card's outline. Shared by the raster and vector-text paths.
///
/// The paper itself is document content (always white); the edge that seats
/// it on the desk is chrome, so it is measured from the theme against that
/// paper. High contrast steps a white `border_standard` out until the card
/// still reads as a card.
fn page_card_edge(theme: &Theme) -> Color {
    theme.tokens.colors.ink_on(
        paper(theme),
        theme.tokens.colors.border_standard,
        ui::tokens::NON_TEXT_MIN_CONTRAST,
    )
}

fn draw_page_border(scene: &mut Scene, theme: &Theme, base: Affine, card_rr: &RoundedRect) {
    scene.stroke(
        &Stroke::new(1.0),
        base,
        page_card_edge(theme),
        None,
        card_rr,
    );
}

/// The annotations + signatures anchored to `page`, drawn on top of its
/// content — shared by the raster and vector-text draw paths.
fn draw_page_markup(ctx: &mut PaintCtx<'_>, page: &Page, frame: PageFrame, markup: Markup<'_>) {
    let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let (origin, scale) = (frame.origin, frame.scale);
    for a in markup.annotations.iter().filter(|a| a.page == page.number) {
        draw_annotation(
            scene,
            tr,
            theme,
            base,
            a,
            origin,
            scale,
            markup.selected == Some(a.id.as_str()),
        );
    }
    for s in markup.signatures.iter().filter(|s| s.page == page.number) {
        draw_signature(scene, tr, theme, base, s, origin, scale);
    }
}

/// Draw a figure placeholder block (box, glyph, caption) `height` points
/// tall at the cursor, and advance the cursor past it.
fn draw_page_figure(ctx: &mut PaintCtx<'_>, at: &mut ColumnCursor, height: f64, label: &str) {
    let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let (content_x, content_w, y, scale) = (at.x, at.w, at.y, at.scale);
    let h = height * scale;
    let box_rect = Rect::new(content_x, y, content_x + content_w, y + h);
    scene.fill(
        Fill::NonZero,
        base,
        Color::from_rgb8(241, 245, 249),
        None,
        &RoundedRect::from_rect(box_rect, 2.0),
    );
    scene.stroke(
        &Stroke::new(1.0),
        base,
        ink_faint(),
        None,
        &RoundedRect::from_rect(box_rect, 2.0),
    );
    draw_figure_glyph(scene, theme, base, box_rect);
    let y = y + h + 4.0 * scale;
    let csize = 9.5 * scale as f32;
    tr.draw_style(
        scene,
        label,
        ink_faint(),
        base * Affine::translate((content_x, y)),
        Some(content_w as f32),
        &TextStyle::new(csize).with_italic(true),
    );
    let (_, ch) = tr.measure(label, csize, Some(content_w as f32));
    at.y = y + f64::from(ch) + 8.0 * scale;
}

/// Draw one heading/body/quote/caption block's text at the cursor, plus its
/// search-match `band` when it matched, and advance the cursor past it.
fn draw_page_text_block(
    ctx: &mut PaintCtx<'_>,
    at: &mut ColumnCursor,
    block: &Block,
    spec: &Spec,
    band: Option<MatchBand>,
) {
    let (scene, base, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
    let (content_x, content_w, y, scale) = (at.x, at.w, at.y, at.scale);
    let text = block.search_text().unwrap_or("");
    let size = spec.size * scale as f32;
    let max_w = content_w as f32;
    let (_, th) = tr.measure(text, size, Some(max_w));

    if let Some(band_kind) = band {
        let band = Rect::new(
            content_x - 4.0 * scale,
            y - 1.0 * scale,
            content_x + content_w + 4.0 * scale,
            y + f64::from(th) + 1.0 * scale,
        );
        let is_active = band_kind == MatchBand::Active;
        let fill = if is_active {
            alpha(theme.accent_primary, 64)
        } else {
            alpha(theme.accent_primary, 26)
        };
        scene.fill(
            Fill::NonZero,
            base,
            fill,
            None,
            &RoundedRect::from_rect(band, 2.0),
        );
        if is_active {
            let bar = Rect::new(band.x0, band.y0, band.x0 + 2.5 * scale, band.y1);
            scene.fill(Fill::NonZero, base, theme.accent_primary, None, &bar);
        }
    }

    let mut x = content_x;
    if matches!(block, Block::Quote(_)) {
        let bar = Rect::new(content_x, y, content_x + 3.0 * scale, y + f64::from(th));
        scene.fill(Fill::NonZero, base, theme.accent_primary, None, &bar);
        x += 14.0 * scale;
    }
    tr.draw_style(
        scene,
        text,
        spec.color,
        base * Affine::translate((x, y)),
        Some(max_w),
        &TextStyle::new(size)
            .with_weight(spec.weight)
            .with_italic(spec.italic),
    );
    at.y = y + f64::from(th) + spec.after * scale;
}

/// Lay out and draw every content block of a vector (non-raster) page —
/// spacers, dividers, figures and styled text — updating the running
/// screen-space `y` as it goes.
fn draw_page_blocks(
    ctx: &mut PaintCtx<'_>,
    page: &Page,
    frame: PageFrame,
    search: SearchBands<'_>,
) {
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
                ctx.scene
                    .stroke(&Stroke::new(1.0), ctx.transform, ink_faint(), None, &line);
                at.y += 16.0 * scale;
            }
            Block::Figure { height, label } => {
                draw_page_figure(ctx, &mut at, *height, label);
            }
            _ => {
                let Some(spec) = body_spec(block, ctx.theme) else {
                    continue;
                };
                draw_page_text_block(ctx, &mut at, block, &spec, search.band(bi));
            }
        }
    }
}

/// Draw one page: paper card, drop shadow, content, search bands, then
/// the annotations + signatures that live on it.
pub fn draw_page(
    ctx: &mut PaintCtx<'_>,
    page: &Page,
    frame: PageFrame,
    raster: PageRaster<'_>,
    search: SearchBands<'_>,
    markup: Markup<'_>,
) {
    let base = ctx.transform;
    let card = frame.card_rect();
    let card_rr = draw_page_backdrop(ctx.scene, ctx.theme, base, card);

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
            draw_page_tiles(ctx.scene, base, tiles);
        }
        draw_page_markup(ctx, page, frame, markup);
        ctx.scene.pop_layer();
        draw_page_border(ctx.scene, ctx.theme, base, &card_rr);
        return;
    }

    draw_page_blocks(ctx, page, frame, search);
    draw_page_markup(ctx, page, frame, markup);

    ctx.scene.pop_layer();
    draw_page_border(ctx.scene, ctx.theme, base, &card_rr);
}

fn draw_figure_glyph(scene: &mut Scene, theme: &Theme, base: Affine, b: Rect) {
    let cy = f64::midpoint(b.y0, b.y1);
    let w = b.width();
    let r = (b.height().min(w) * 0.16).max(8.0);
    let lx = b.x0 + w * 0.32;
    let rx = b.x0 + w * 0.68;
    let conn = Line::new((lx + r, cy), (rx - r, cy));
    scene.stroke(&Stroke::new(2.0), base, theme.accent_primary, None, &conn);
    for cx in [lx, rx] {
        let node = Rect::new(cx - r, cy - r, cx + r, cy + r);
        scene.fill(
            Fill::NonZero,
            base,
            alpha(theme.accent_primary, 46),
            None,
            &RoundedRect::from_rect(node, 4.0),
        );
        scene.stroke(
            &Stroke::new(1.5),
            base,
            theme.accent_primary,
            None,
            &RoundedRect::from_rect(node, 4.0),
        );
    }
}

fn pt(origin: Point, scale: f64, x: f64, y: f64) -> Point {
    Point::new(origin.x + x * scale, origin.y + y * scale)
}

fn draw_annotation(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    base: Affine,
    a: &Annotation,
    origin: Point,
    scale: f64,
    selected: bool,
) {
    let col = swatch_color(theme, a.color);
    match &a.kind {
        AnnoKind::Markup { style, quads } => {
            let mut bounds: Option<Rect> = None;
            for rect in quads {
                let p0 = pt(origin, scale, rect.x, rect.y);
                let r = Rect::new(p0.x, p0.y, p0.x + rect.w * scale, p0.y + rect.h * scale);
                match style {
                    MarkupStyle::Highlight => {
                        scene.fill(Fill::NonZero, base, alpha(col, 92), None, &r);
                    }
                    MarkupStyle::Underline | MarkupStyle::StrikeOut => {
                        // A line at the baseline (underline) or through the
                        // middle (strike-out) of each band.
                        let y = if *style == MarkupStyle::Underline {
                            r.y0 + r.height() * 0.9
                        } else {
                            r.y0 + r.height() * 0.55
                        };
                        let line = vello::kurbo::Line::new((r.x0, y), (r.x1, y));
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
                        scene.stroke(&Stroke::new((1.0 * scale).max(0.8)), base, col, None, &wave);
                    }
                }
                bounds = Some(bounds.map_or(r, |b| b.union(r)));
            }
            if selected && let Some(r) = bounds {
                scene.stroke(&Stroke::new(1.5), base, col, None, &r);
                draw_handles(scene, theme, base, r);
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
                scene.stroke(&Stroke::new((2.4 * scale).max(1.5)), base, col, None, &path);
            }
        }
        AnnoKind::Note { x, y } => {
            let p = pt(origin, scale, *x, *y);
            draw_note_marker(scene, base, p, col, scale, selected);
        }
        AnnoKind::FreeText { rect, text, size } => {
            let p0 = pt(origin, scale, rect.x, rect.y);
            let r = Rect::new(p0.x, p0.y, p0.x + rect.w * scale, p0.y + rect.h * scale);
            draw_free_text(scene, tr, base, r, text, (*size * scale) as f32, col);
            if selected {
                scene.stroke(&Stroke::new(1.5), base, col, None, &r);
                draw_handles(scene, theme, base, r);
            }
        }
    }
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
    tr.draw_text(
        scene,
        text,
        size,
        color,
        base * Affine::translate((inner.x0, inner.y0)),
        Some(inner.width() as f32),
    );
    scene.pop_layer();
}

fn draw_note_marker(
    scene: &mut Scene,
    base: Affine,
    p: Point,
    col: Color,
    scale: f64,
    selected: bool,
) {
    let s = (22.0 * scale).max(16.0);
    let r = Rect::new(p.x, p.y, p.x + s, p.y + s);
    scene.fill(
        Fill::NonZero,
        base,
        col,
        None,
        &RoundedRect::from_rect(r, 3.0),
    );
    let fold = {
        let mut path = BezPath::new();
        path.move_to((r.x1 - s * 0.36, r.y1));
        path.line_to((r.x1, r.y1 - s * 0.36));
        path.line_to((r.x1, r.y1));
        path.close_path();
        path
    };
    scene.fill(
        Fill::NonZero,
        base,
        Color::from_rgba8(0, 0, 0, 46),
        None,
        &fold,
    );
    for i in 0..3 {
        let ly = r.y0 + s * (0.32 + f64::from(i) * 0.18);
        let line = Line::new((r.x0 + s * 0.2, ly), (r.x1 - s * 0.2, ly));
        scene.stroke(
            &Stroke::new((1.2 * scale).max(0.8)),
            base,
            Color::from_rgba8(255, 255, 255, 220),
            None,
            &line,
        );
    }
    if selected {
        let ring = RoundedRect::from_rect(r.inflate(2.5, 2.5), 5.0);
        scene.stroke(&Stroke::new(2.0), base, Color::WHITE, None, &ring);
    }
}

fn draw_handles(scene: &mut Scene, theme: &Theme, base: Affine, r: Rect) {
    let hs = 3.0;
    for (cx, cy) in [(r.x0, r.y0), (r.x1, r.y0), (r.x0, r.y1), (r.x1, r.y1)] {
        let h = Rect::new(cx - hs, cy - hs, cx + hs, cy + hs);
        scene.fill(Fill::NonZero, base, Color::WHITE, None, &h);
        scene.stroke(&Stroke::new(1.0), base, theme.accent_primary, None, &h);
    }
}

fn draw_signature(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    base: Affine,
    s: &Signature,
    origin: Point,
    scale: f64,
) {
    let p0 = pt(origin, scale, s.rect.x, s.rect.y);
    let r = Rect::new(p0.x, p0.y, p0.x + s.rect.w * scale, p0.y + s.rect.h * scale);
    scene.fill(
        Fill::NonZero,
        base,
        alpha(theme.accent_primary, 16),
        None,
        &RoundedRect::from_rect(r, 3.0),
    );
    scene.stroke(
        &Stroke::new(1.2),
        base,
        alpha(theme.accent_primary, 150),
        None,
        &RoundedRect::from_rect(r, 3.0),
    );

    let stroke_area = Rect::new(
        r.x0 + r.width() * 0.06,
        r.y0 + r.height() * 0.10,
        r.x1 - r.width() * 0.06,
        r.y0 + r.height() * 0.56,
    );
    draw_signature_ink(scene, tr, base, ink(), stroke_area, &s.ink, &s.signer);

    let base_y = r.y0 + r.height() * 0.62;
    let baseline = Line::new((r.x0 + 8.0, base_y), (r.x1 - 8.0, base_y));
    scene.stroke(&Stroke::new(1.0), base, ink_faint(), None, &baseline);

    let name_size = (8.5 * scale).max(8.0) as f32;
    tr.draw_text_weighted(
        scene,
        &s.signer,
        name_size,
        ink_soft(),
        base * Affine::translate((r.x0 + 8.0, base_y + 3.0)),
        Some((r.width() - 16.0) as f32),
        TextWeight::Semibold,
    );
    let date_label = format!("Signed {}", s.date);
    tr.draw_text(
        scene,
        &date_label,
        (7.5 * scale).max(7.0) as f32,
        ink_faint(),
        base * Affine::translate((r.x0 + 8.0, base_y + 3.0 + f64::from(name_size) + 2.0)),
        Some((r.width() - 16.0) as f32),
    );
}

/// Draw a signature's **real ink** inside `area` (no field chrome): the signer's
/// typed name in a script style, or the freehand strokes they actually drew
/// (normalised `[0,1]²`, scaled into `area`). Shared by the placed-signature
/// draw and the sidebar preview, so both show exactly what will be stamped.
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
            // A signature script: an italic serif face, sized to the field.
            let family = FontChoice::Serif;
            let mut size = (area.height() * 0.85).clamp(9.0, 30.0) as f32;
            let maxw = area.width() as f32;
            let (mut tw, _) = tr.measure_font(name, size, None, TextWeight::Medium, true, &family);
            if tw > maxw && tw > 0.0 {
                size = (size * maxw / tw).max(7.0);
                tw = tr
                    .measure_font(name, size, None, TextWeight::Medium, true, &family)
                    .0;
            }
            let tx = area.x0 + f64::from((area.width() as f32 - tw).max(0.0) / 2.0);
            let ty = area.y0 + (area.height() - f64::from(size)) / 2.0;
            tr.draw_style(
                scene,
                name,
                color,
                base * Affine::translate((tx, ty)),
                None,
                &TextStyle::new(size)
                    .with_weight(TextWeight::Medium)
                    .with_italic(true)
                    .with_family(family.clone()),
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
                scene.stroke(&Stroke::new(w), base, color, None, &path);
            }
        }
    }
}

/// Public wrapper to draw just the signature ink (no field chrome), used by the
/// signatures sidebar for previews.
pub fn draw_signature_preview(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    base: Affine,
    color: Color,
    area: Rect,
    ink: &SignatureInk,
    signer: &str,
) {
    draw_signature_ink(scene, tr, base, color, area, ink, signer);
}

// ----------------------------------------------------------------------
// Thumbnails — abstract bar representation (fast, readable at small size)
// ----------------------------------------------------------------------

/// One page-rail thumbnail: the page, its width in points (the block
/// sketch's scale), the engine bitmap when the raster backend has one, and
/// whether it is the current page and carries markup.
pub struct Thumbnail<'a> {
    pub page: &'a Page,
    pub page_w: f64,
    pub raster: Option<&'a PenikoImage>,
    pub is_current: bool,
    pub has_markup: bool,
}

impl Thumbnail<'_> {
    pub fn draw(&self, scene: &mut Scene, theme: &Theme, base: Affine, rect: Rect) {
        let rr = RoundedRect::from_rect(rect, 2.0);
        scene.fill(Fill::NonZero, base, paper(theme), None, &rr);

        // Raster backend: paint the real page miniature.
        if let Some(img) = self.raster {
            self.draw_raster(scene, theme, base, rect, img);
            return;
        }

        draw_thumbnail_vector_blocks(scene, base, self.page, rect, self.page_w);
        self.draw_border_and_marker(scene, theme, base, rect);
    }

    /// The current-page border + has-markup dot, shared by the raster and
    /// vector-bar thumbnail paths.
    fn draw_border_and_marker(&self, scene: &mut Scene, theme: &Theme, base: Affine, rect: Rect) {
        let (is_current, has_markup) = (self.is_current, self.has_markup);
        let rr = &RoundedRect::from_rect(rect, 2.0);
        let border = if is_current {
            theme.accent_primary
        } else {
            theme.tokens.colors.border_standard
        };
        let stroke_w = if is_current { 2.0 } else { 1.0 };
        scene.stroke(&Stroke::new(stroke_w), base, border, None, rr);

        if has_markup {
            let d = 5.0;
            let dot = Circle::new((rect.x1 - d - 2.0, rect.y0 + d + 2.0), d);
            scene.fill(
                Fill::NonZero,
                base,
                theme.tokens.colors.semantic.warning,
                None,
                &dot,
            );
        }
    }

    /// Paint the real page miniature (raster backend) plus its border + marker.
    fn draw_raster(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        base: Affine,
        rect: Rect,
        img: &PenikoImage,
    ) {
        let rr = RoundedRect::from_rect(rect, 2.0);
        if img.width > 0 && img.height > 0 {
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, base, &rr);
            // Same fit as the page itself, so a rotated page's miniature keeps
            // its aspect.
            let (iw, ih) = (f64::from(img.width), f64::from(img.height));
            let s = crate::model::fit_scale(rect.width(), rect.height(), iw, ih);
            let dx = ((rect.width() - iw * s) / 2.0).max(0.0);
            let dy = ((rect.height() - ih * s) / 2.0).max(0.0);
            let tf = base
                * Affine::translate((rect.x0 + dx, rect.y0 + dy))
                * Affine::scale_non_uniform(s, s);
            scene.draw_image(img, tf);
            scene.pop_layer();
        }
        self.draw_border_and_marker(scene, theme, base, rect);
    }
}

/// Abstract bar representation of a page's content blocks (vector backend):
/// a small coloured bar per heading/body/figure line, fast and readable at
/// thumbnail size.
fn draw_thumbnail_vector_blocks(
    scene: &mut Scene,
    base: Affine,
    page: &Page,
    rect: Rect,
    page_w: f64,
) {
    let scale = rect.width() / page_w;
    let cx = rect.x0 + MARGIN * scale;
    let cw = (page_w - 2.0 * MARGIN) * scale;
    let mut y = rect.y0;
    let line_col = Color::from_rgb8(203, 213, 225);
    let head_col = Color::from_rgb8(100, 116, 139);
    let bar = |scene: &mut Scene, y: f64, hgt: f64, x: f64, w: f64, color: Color| {
        let r = Rect::new(x, y, x + w, y + hgt);
        scene.fill(Fill::NonZero, base, color, None, &r);
    };
    for block in &page.blocks {
        match block {
            Block::Spacer(h) => y += h * scale,
            Block::Divider => {
                bar(scene, y + 3.0 * scale, 1.0, cx, cw, line_col);
                y += 16.0 * scale;
            }
            Block::Figure { height, .. } => {
                let h = height * scale;
                bar(scene, y, h, cx, cw, Color::from_rgb8(226, 232, 240));
                y += h + 10.0 * scale;
            }
            Block::Title(_) => {
                bar(
                    scene,
                    y + 2.0 * scale,
                    (16.0 * scale).max(2.5),
                    cx,
                    cw * 0.85,
                    head_col,
                );
                y += 30.0 * scale;
            }
            Block::H1(_) | Block::H2(_) => {
                bar(
                    scene,
                    y + 1.0 * scale,
                    (8.0 * scale).max(2.0),
                    cx,
                    cw * 0.55,
                    head_col,
                );
                y += 18.0 * scale;
            }
            Block::Quote(t) | Block::Body(t) | Block::Caption(t) => {
                let lines = (t.len() / 52).clamp(1, 8);
                let lh = 6.0 * scale;
                let indent = if matches!(block, Block::Quote(_)) {
                    10.0 * scale
                } else {
                    0.0
                };
                for i in 0..lines {
                    let w = if i == lines - 1 {
                        cw * 0.6
                    } else {
                        cw - indent
                    };
                    bar(scene, y, (2.2 * scale).max(1.0), cx + indent, w, line_col);
                    y += lh;
                }
                y += 6.0 * scale;
            }
        }
        if y > rect.y1 {
            break;
        }
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
                contrast_ratio(edge, paper(&theme)) >= NON_TEXT_MIN_CONTRAST,
                "{name} page edge must read on the paper, got {:.2}:1",
                contrast_ratio(edge, paper(&theme))
            );
        }
    }

    #[test]
    fn collect_text_lines_produces_bands_inside_the_page() {
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let doc = sample_document();
        let page = doc.page(1).unwrap();
        let lines = collect_text_lines(&mut tr, &theme, page);
        assert!(!lines.is_empty(), "page 1 has body text -> some lines");
        for l in &lines {
            // Bands sit within the page's text column and below the top.
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
}
