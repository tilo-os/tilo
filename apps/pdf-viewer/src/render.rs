//! Page rendering: a page's engine pixels on its paper, with annotations and
//! signatures over them. Page geometry is in PDF points and `scale` is pixels per point,
//! so a point maps to `origin + point * scale` and then through `base`. The paper, the
//! ink and the markup keep the document's own colours; only what the reader adds over the
//! content takes the theme's.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Compose, Fill, ImageData as PenikoImage, Mix};

use ui::paint::{PaintCtx, with_alpha};
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;

use crate::model::{
    AnnoKind, Annotation, HighlightColor, MarkupStyle, Page, Signature, SignatureInk,
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
pub fn paper() -> Color {
    Color::from_rgb8(255, 255, 255)
}

pub fn swatch_color(s: HighlightColor) -> Color {
    let (red, green, blue) = s.rgb();
    Color::from_rgb8(red, green, blue)
}

pub fn alpha(c: Color, a: u8) -> Color {
    with_alpha(c, a)
}

const HIGHLIGHT_ALPHA: u8 = 92;

pub fn page_card_rect(origin: Point, scale: f64, page_w: f64, page_h: f64) -> Rect {
    Rect::new(
        origin.x,
        origin.y,
        origin.x + page_w * scale,
        origin.y + page_h * scale,
    )
}

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

#[derive(Clone, Copy)]
pub struct PageRaster<'a> {
    pub image: Option<&'a PenikoImage>,
    pub tiles: Option<&'a PageTiles>,
}

#[derive(Clone, Copy)]
pub struct Markup<'a> {
    pub annotations: &'a [Annotation],
    pub signatures: &'a [Signature],
    pub selected: Option<&'a str>,
}

fn draw_page_paper(scene: &mut Scene, theme: &Theme, base: Affine, card: Rect) -> RoundedRect {
    let card_rr = RoundedRect::from_rect(card, f64::from(theme.tokens.radii.detail) / 2.0);
    scene.fill(Fill::NonZero, base, paper(), None, &card_rr);
    card_rr
}

pub use tilo_page_raster::tiles::PageTiles;
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

pub fn draw_page(
    ctx: &mut PaintCtx<'_>,
    page: &Page,
    frame: PageFrame,
    raster: PageRaster<'_>,
    markup: Markup<'_>,
) {
    let base = ctx.transform;
    let card = frame.card_rect();
    let card_rr = draw_page_paper(ctx.scene, ctx.theme, base, card);

    ctx.scene
        .push_layer(Fill::NonZero, BlendMode::default(), 1.0, base, &card_rr);

    if let Some(img) = raster.image {
        draw_page_raster_image(ctx.scene, base, card, img);
    }
    if let Some(tiles) = raster.tiles {
        tiles.paint(ctx.scene, base);
    }
    draw_page_markup(ctx, page, frame, markup);

    ctx.scene.pop_layer();
    draw_page_border(ctx.scene, ctx.theme, base, &card_rr);
}

fn pt(origin: Point, scale: f64, x: f64, y: f64) -> Point {
    Point::new(origin.x + x * scale, origin.y + y * scale)
}

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
                        let y = if *style == MarkupStyle::Underline {
                            r.y0 + r.height() * 0.9
                        } else {
                            r.y0 + r.height() * 0.55
                        };
                        let line = Line::new((r.x0, y), (r.x1, y));
                        scene.stroke(&Stroke::new((1.4 * scale).max(1.0)), base, col, None, &line);
                    }
                    MarkupStyle::Squiggly => {
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

fn ink_stroke(scale: f64) -> Stroke {
    Stroke::new((2.4 * scale).max(1.5))
        .with_caps(vello::kurbo::Cap::Round)
        .with_join(vello::kurbo::Join::Round)
}

pub fn draw_free_text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    base: Affine,
    r: Rect,
    text: &str,
    size: f32,
    color: Color,
) {
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
    let stamp = crate::t_args(
        "pdf-drawn-signed",
        &[("date", crate::dates::shown(&s.date).into())],
    );
    tr.draw_style(
        scene,
        &stamp,
        ink_faint(),
        base * Affine::translate((r.x0 + inset, name_y + f64::from(name.line_height_px()))),
        width,
        &date,
    );
}

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
            let unnamed = crate::t("pdf-signature-unnamed");
            let name = if signer.trim().is_empty() {
                unnamed.as_str()
            } else {
                signer.trim()
            };
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

pub fn draw_highlight_preview(scene: &mut Scene, base: Affine, col: Color, r: Rect) {
    fill_highlight(scene, base, col, r);
}

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

pub fn draw_edit_box(scene: &mut Scene, base: Affine, r: Rect) {
    scene.fill(Fill::NonZero, base, paper(), None, &r);
}

pub enum FieldValue {
    Check(bool),
    Text(String),
}

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
        // Lay out at the document's own point size, then scale the scene, so a tiny font
        // size cannot change wrapping or glyph positions.
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
            self.markup,
        );
        ctx.scene.pop_layer();
        ctx.text.set_text_scale(text_scale);
    }
}

/// Crop a page-point region out of a rendered page bitmap as `(width, height, rgba)`
/// for the clipboard; `None` for an empty region or a malformed bitmap.
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
            return None;
        };
        out.extend_from_slice(slice);
    }
    Some((cw, ch, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card's outline is chrome: it clears the non-text floor under every theme.
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
}
