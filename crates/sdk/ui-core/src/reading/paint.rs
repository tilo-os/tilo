//! Painting a laid-out flow onto its page.
//!
//! [`paint_flow`] draws the part of a [`FlowLayout`] that is on screen:
//! each run in its style, links underlined in the page's ink, footnote
//! references in the accent, code in its well and tables with their rules
//! (both scrolled sideways by their own offset), pictures fitted into the
//! box the layout reserved for them, and the find and selection highlights
//! over the glyphs they cover. It reports where links, notes, pictures and
//! sideways-scrolling blocks were drawn, for hit-testing.

use std::sync::Arc;

use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};

use super::flow::{FlowLayout, LaidBlock, LayoutKind, LineRun, run_style};
use super::palette::ReaderPalette;
use crate::color_compat::ColorCompat;
use crate::image_store::ImageStore;
use crate::paint::PaintCtx;
use crate::text_render::{FontChoice, TextRenderer, TextStyle};
use crate::theme::Theme;

/// A range of one laid block's canonical text, in characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FlowRange {
    /// The index of the block in [`FlowLayout::blocks`].
    pub block: usize,
    pub start: usize,
    pub end: usize,
}

/// What one flow paint needs besides the layout.
pub struct FlowPaint<'a> {
    pub layout: &'a FlowLayout,
    pub palette: &'a ReaderPalette,
    /// The theme printed on the page ([`ReaderPalette::page_theme`]).
    pub page_theme: &'a Theme,
    pub base_family: &'a FontChoice,
    /// Screen x of the measure's left edge.
    pub x: f64,
    /// Screen y of the flow's top.
    pub y: f64,
    /// The measure's width.
    pub measure: f32,
    /// The part of the screen the flow shows in.
    pub clip: Rect,
    /// Each sideways-scrolling block's offset, by block index.
    pub hscroll: &'a dyn Fn(usize) -> f32,
    pub images: Option<&'a ImageStore>,
    pub find: &'a [FlowRange],
    pub current_find: Option<usize>,
    pub selection: Option<FlowRange>,
    /// The person's highlights, each over its range in its colour.
    pub marks: &'a [(FlowRange, Color)],
    pub hovered_link: Option<&'a str>,
}

/// Where a paint drew what can be pointed at.
#[derive(Clone, Debug, Default)]
pub struct FlowHits {
    /// Links, one rect per run, with their address.
    pub links: Vec<(Rect, Arc<str>)>,
    /// Footnote references and the note each opens.
    pub notes: Vec<(Rect, u16)>,
    /// Pictures and their address.
    pub images: Vec<(Rect, String)>,
    /// Blocks that scroll sideways, by block index.
    pub scrollers: Vec<(Rect, usize)>,
}

/// Paint the visible part of the flow (see the [module docs](self)).
pub fn paint_flow(ctx: &mut PaintCtx<'_>, paint: &FlowPaint<'_>) -> FlowHits {
    let mut hits = FlowHits::default();
    let top = crate::num::f32_of_f64(paint.clip.y0 - paint.y);
    let bottom = crate::num::f32_of_f64(paint.clip.y1 - paint.y);
    let blocks = &paint.layout.blocks;
    let first = blocks.partition_point(|block| block.bottom < top);
    for (index, block) in blocks.iter().enumerate().skip(first) {
        if block.top > bottom {
            break;
        }
        let previous = index.checked_sub(1).and_then(|i| blocks.get(i));
        paint_block(ctx, paint, index, block, previous, &mut hits);
    }
    hits
}

fn paint_block(
    ctx: &mut PaintCtx<'_>,
    paint: &FlowPaint<'_>,
    index: usize,
    block: &LaidBlock,
    previous: Option<&LaidBlock>,
    hits: &mut FlowHits,
) {
    let palette = paint.palette;
    let screen = |y: f32| paint.y + f64::from(y);
    let x = paint.x;
    let measure = f64::from(paint.measure);
    let scrolls = block.scrolls(paint.measure);
    let shift = if scrolls {
        f64::from((paint.hscroll)(index))
    } else {
        0.0
    };
    match block.kind {
        LayoutKind::Quote | LayoutKind::Verse { quoted: true } => {
            // Consecutive quoted paragraphs share one rule.
            let joined = previous.is_some_and(|p| {
                matches!(
                    p.kind,
                    LayoutKind::Quote | LayoutKind::Verse { quoted: true }
                )
            });
            let from = if joined {
                previous.map_or(block.top, |p| p.bottom)
            } else {
                block.top
            };
            let rule = Rect::new(x + 2.0, screen(from), x + 5.0, screen(block.bottom));
            ctx.fill_rect(rule, palette.rule);
        }
        LayoutKind::Separator => {
            let cy = screen(block.top) + f64::from(block.line_height) / 2.0;
            let cx = x + measure / 2.0;
            let mark = Rect::new(cx - 28.0, cy - 0.7, cx + 28.0, cy + 0.7);
            ctx.fill_rect(mark, palette.rule);
        }
        LayoutKind::Code => {
            let well = Rect::new(x, screen(block.top), x + measure, screen(block.bottom));
            let radius = f64::from(paint.page_theme.tokens.radii.control);
            ctx.fill_round(well, radius, palette.well);
        }
        _ => {}
    }
    if scrolls {
        let area = Rect::new(x, screen(block.top), x + measure, screen(block.bottom));
        hits.scrollers.push((area, index));
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &area,
        );
    }
    let origin = x + f64::from(block.indent) - shift;
    if let Some(table) = &block.table {
        let width = table
            .columns
            .last()
            .map_or(0.0, |(left, w)| f64::from(left + w));
        for (row, &(y, height, header)) in table.rows.iter().enumerate() {
            let line_y = screen(y + height);
            let last = row + 1 == table.rows.len();
            if last && !header {
                continue;
            }
            let weight = if header { 1.5 } else { 1.0 };
            ctx.fill_rect(
                Rect::new(x - shift, line_y - weight, x - shift + width, line_y),
                palette.rule,
            );
        }
    }
    if let Some(figure) = &block.figure {
        let rect = Rect::new(
            x + f64::from(figure.x),
            screen(block.top),
            x + f64::from(figure.x + figure.width),
            screen(block.top + figure.height),
        )
        .round();
        paint_picture(ctx, paint, rect, &figure.image.src, &figure.image.alt);
        hits.images.push((rect, figure.image.src.clone()));
    }
    if let Some(math) = &block.math {
        crate::math::draw_layout(
            ctx.scene,
            ctx.transform,
            ctx.text,
            &math.layout,
            Point::new(
                x + f64::from(math.x),
                screen(block.top) + f64::from(math.y_offset),
            ),
            palette.ink,
        );
    }
    let family = block.family.as_ref().unwrap_or(paint.base_family);
    if let Some(bullet) = &block.bullet
        && let Some(first) = block.lines.first()
    {
        let style = run_style(block.size, false, false, false, family);
        let width = f64::from(ctx.text.measure_style(bullet, None, &style).0);
        let gap = f64::from(block.size) * 0.45;
        ctx.text.draw_style(
            ctx.scene,
            bullet,
            palette.ink_dim,
            ctx.transform * Affine::translate((origin - gap - width, screen(first.y))),
            None,
            &style,
        );
    }
    let ink = if block.dim {
        palette.ink_dim
    } else {
        palette.ink
    };
    for line in &block.lines {
        let line_top = screen(line.y);
        if line_top > paint.clip.y1 || line_top + f64::from(block.line_height) < paint.clip.y0 {
            continue;
        }
        for run in &line.runs {
            let painted = PaintedRun {
                block,
                index,
                run,
                x: origin + f64::from(run.x),
                line_top,
                glyph_scale: line.glyph_scale,
                family,
                ink,
            };
            paint_run(ctx, paint, &painted, hits);
        }
    }
    if scrolls {
        ctx.scene.pop_layer();
    }
}

/// A picture in its reserved box: the picture fitted inside once it has
/// loaded, a quiet well while it loads, its description if it cannot.
fn paint_picture(ctx: &mut PaintCtx<'_>, paint: &FlowPaint<'_>, rect: Rect, src: &str, alt: &str) {
    let radius = f64::from(paint.page_theme.tokens.radii.detail);
    if let Some(image) = paint.images.and_then(|images| images.get(src)) {
        crate::image_store::paint_image_contain(ctx.scene, ctx.transform, rect, &image, radius);
        return;
    }
    ctx.fill_round(rect, radius, paint.palette.well);
    let failed = paint.images.is_none_or(|images| images.failed(src));
    if failed && !alt.is_empty() {
        let style = paint.page_theme.text(crate::tokens::TextRole::Caption);
        let width = crate::num::f32_of_f64((rect.width() - 24.0).max(0.0));
        let lines = ctx.text.clamp_to_lines(alt, &style, width, 3);
        let line_h = f64::from(ctx.text.measure_style("Ag", None, &style).1);
        let mut y = rect.center().y - line_h * crate::num::f64_of_usize(lines.len()) / 2.0;
        for line in &lines {
            let w = f64::from(ctx.text.measure_style(line, None, &style).0);
            ctx.text.draw_style(
                ctx.scene,
                line,
                paint.palette.ink_dim,
                ctx.transform * Affine::translate((rect.center().x - w / 2.0, y)),
                None,
                &style,
            );
            y += line_h;
        }
    }
}

struct PaintedRun<'a> {
    block: &'a LaidBlock,
    index: usize,
    run: &'a LineRun,
    x: f64,
    line_top: f64,
    glyph_scale: f32,
    family: &'a FontChoice,
    ink: Color,
}

impl PaintedRun<'_> {
    fn family(&self) -> FontChoice {
        if self.run.mono {
            FontChoice::Monospace
        } else {
            self.family.clone()
        }
    }

    fn style(&self) -> TextStyle {
        run_style(
            self.run.size,
            self.run.bold,
            self.run.italic,
            self.run.small_caps,
            &self.family(),
        )
    }

    /// How wide the first `chars` characters of the run paint.
    fn width_of(&self, text: &mut TextRenderer, chars: usize) -> f64 {
        let end = self
            .run
            .display_text
            .char_indices()
            .nth(chars)
            .map_or(self.run.display_text.len(), |(at, _)| at);
        let prefix = self.run.display_text.get(..end).unwrap_or_default();
        if prefix.is_empty() {
            return 0.0;
        }
        f64::from(text.measure_style(prefix, None, &self.style()).0 * self.glyph_scale)
    }

    /// The part of the run `range` covers, as a rect on screen.
    fn covered(&self, text: &mut TextRenderer, range: &FlowRange) -> Option<Rect> {
        let start = self.run.char_start;
        let end = start + self.run.char_len;
        if range.block != self.index || range.end <= start || range.start >= end {
            return None;
        }
        let from = range.start.max(start) - start;
        let to = range.end.min(end) - start;
        let x0 = self.x + self.width_of(text, from);
        let x1 = self.x + self.width_of(text, to);
        Some(Rect::new(
            x0,
            self.line_top,
            x1,
            self.line_top + f64::from(self.block.line_height),
        ))
    }
}

fn paint_run(
    ctx: &mut PaintCtx<'_>,
    paint: &FlowPaint<'_>,
    painted: &PaintedRun<'_>,
    hits: &mut FlowHits,
) {
    let run = painted.run;
    let theme = paint.page_theme;
    let run_y = painted.line_top + f64::from(run.y);
    let style = painted.style();
    let width = f64::from(ctx.text.measure_style(&run.display_text, None, &style).0)
        * f64::from(painted.glyph_scale);
    let line_rect = Rect::new(
        painted.x,
        painted.line_top,
        painted.x + width,
        painted.line_top + f64::from(painted.block.line_height),
    );
    for (range, color) in paint.marks {
        if let Some(rect) = painted.covered(ctx.text, range) {
            let band = Rect::new(rect.x0 - 1.0, rect.y0 + 1.0, rect.x1 + 1.0, rect.y1 - 1.0);
            ctx.fill_round(band, 3.0, *color);
        }
    }
    if let Some(selection) = paint.selection
        && let Some(rect) = painted.covered(ctx.text, &selection)
    {
        ctx.fill_rect(rect, theme.tokens.colors.intent.selection_tint);
    }
    for (number, found) in paint.find.iter().enumerate() {
        if let Some(rect) = painted.covered(ctx.text, found) {
            crate::widget::find_bar::paint_find_match(
                ctx.scene,
                ctx.transform,
                rect,
                paint.current_find == Some(number),
                theme,
            );
        }
    }
    // Code in running text sits on a whisper of the well.
    if run.mono && painted.block.kind != LayoutKind::Code && run.math.is_none() {
        let chip = Rect::new(
            painted.x - 2.0,
            run_y,
            painted.x + width + 2.0,
            run_y + f64::from(run.size) * 1.3,
        );
        ctx.fill_round(chip, 3.0, paint.palette.well);
    }
    let color = if run.note.is_some() {
        theme.tokens.colors.intent.accent_ink
    } else {
        painted.ink
    };
    let transform = ctx.transform
        * Affine::translate((painted.x, run_y))
        * Affine::scale_non_uniform(f64::from(painted.glyph_scale), 1.0);
    if let Some(math) = &run.math {
        crate::math::draw_layout(ctx.scene, transform, ctx.text, math, Point::ZERO, color);
    } else {
        ctx.text
            .draw_style(ctx.scene, &run.display_text, color, transform, None, &style);
    }
    if let Some(link) = &run.link {
        let hovered = paint.hovered_link == Some(link.as_ref());
        let baseline = f64::from(
            ctx.text
                .first_baseline_style(&run.display_text, None, &style),
        );
        let thickness = (f64::from(run.size) / 16.0).max(1.0) * if hovered { 1.6 } else { 1.0 };
        let alpha = if hovered { 1.0 } else { 0.55 };
        let underline = Rect::new(
            painted.x,
            run_y + baseline + 2.0,
            painted.x + width,
            run_y + baseline + 2.0 + thickness,
        );
        ctx.fill_rect(underline, color.with_alpha_factor(alpha));
        hits.links.push((line_rect, Arc::clone(link)));
    }
    if let Some(note) = run.note {
        // A raised number is small; its target is the whole line box.
        hits.notes.push((line_rect.inflate(4.0, 0.0), note));
    }
}

/// The character of `block` under screen x `x` on `line`, given the same
/// placement a paint used: the offset into the block's canonical text.
#[must_use]
pub fn char_at(
    text: &mut TextRenderer,
    block: &LaidBlock,
    line: usize,
    x: f64,
    origin: f64,
    base_family: &FontChoice,
) -> usize {
    let Some(line) = block.lines.get(line) else {
        return block.char_len;
    };
    let family = block.family.as_ref().unwrap_or(base_family);
    let mut best = line.runs.first().map_or(0, |run| run.char_start);
    for run in &line.runs {
        let run_x = origin + f64::from(block.indent) + f64::from(run.x);
        if x < run_x {
            break;
        }
        let run_family = if run.mono {
            FontChoice::Monospace
        } else {
            family.clone()
        };
        let style = run_style(run.size, run.bold, run.italic, run.small_caps, &run_family);
        best = run.char_start + run.char_len;
        let mut left = run_x;
        for (count, (at, c)) in run.display_text.char_indices().enumerate() {
            let next = at + c.len_utf8();
            let prefix = run.display_text.get(..next).unwrap_or_default();
            let right =
                run_x + f64::from(text.measure_style(prefix, None, &style).0 * line.glyph_scale);
            if x < (left + right) / 2.0 {
                return (run.char_start + count).min(run.char_start + run.char_len);
            }
            left = right;
        }
    }
    best
}
