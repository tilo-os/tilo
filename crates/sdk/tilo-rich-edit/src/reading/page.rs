//! One page of a laid-out chapter, painted: its words at the styles the layout gave them, each
//! block's rule or bullet where the block's kind puts it, and the passages a reader marked as
//! bands under the words they cover.
//!
//! A page is the range the layout broke at [`ChapterLayout::pages`]: a block's line is drawn at
//! its distance below where the page begins. Nothing leaves the rect: the page is its own clip,
//! so a line a paragraph overhangs or a glyph's colour never paints over the page's neighbours.

use ui::text_render::{FontChoice, TextRenderer, TextWeight};
use vello::Scene;
use vello::kurbo::Affine;
use vello::kurbo::{Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

use super::layout::{ChapterLayout, LaidBlock, LayoutKind};

/// How a page is painted: where, in what ink, and which passages are marked.
pub struct PagePaint<'a> {
    /// The area the page's text is set in: its left edge is the column's, its top the first
    /// line's.
    pub rect: Rect,
    /// The paper behind the words, in the reading theme the person chose.
    pub paper: Option<Color>,
    pub ink: Color,
    /// Secondary ink: a list's bullet, a section's rule.
    pub ink_dim: Color,
    /// The face the body is set in; a monospace run is set in the mono face whatever this is.
    pub base_family: &'a FontChoice,
    /// The passages marked, in the text's coordinates: block, first and last character, and the
    /// band's colour.
    pub marks: &'a [(usize, usize, usize, Color)],
    /// How far down the flow the column stands: the continuous mode's scroll, `0` at the top.
    pub offset: f32,
}

/// Paint page `page` of `layout` into `paint.rect`; `usize::MAX` paints the flow from the top
/// the paint's `offset` names, which is the continuous mode.
pub fn draw_blocks(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    layout: &ChapterLayout,
    page: usize,
    paint: &PagePaint<'_>,
) {
    let whole = page == usize::MAX;
    if !whole && page >= layout.pages.len() {
        // A spread's second page past the text's end: there is nothing to set there.
        return;
    }
    let start = if whole {
        0.0
    } else {
        layout.pages.get(page).copied().unwrap_or(0.0)
    };
    let end = if whole {
        layout.total_height
    } else {
        layout
            .pages
            .get(page.saturating_add(1))
            .copied()
            .unwrap_or(layout.total_height)
    };
    let shows = |top: f32, height: f32| top + height > start && top < end;

    if let Some(paper) = paint.paper {
        scene.fill(Fill::NonZero, transform, paper, None, &paint.rect);
    }
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        transform,
        &paint.rect,
    );
    let y = |flow: f32| paint.rect.y0 + f64::from(flow - start) - f64::from(paint.offset);
    for block in &layout.blocks {
        if !shows(block.top, block.bottom - block.top) {
            continue;
        }
        if let Some(math) = &block.math {
            ui::math::draw_layout(
                scene,
                transform
                    * Affine::translate((
                        paint.rect.x0 + f64::from(math.x),
                        y(block.top + math.y_offset),
                    )),
                text,
                &math.layout,
                Point::ZERO,
                paint.ink,
            );
        }
        draw_decoration(scene, transform, text, block, start, &y, paint);
        draw_lines(scene, transform, text, block, start, &y, paint);
    }
    scene.pop_layer();
}

fn draw_decoration(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    block: &LaidBlock,
    start: f32,
    y: &impl Fn(f32) -> f64,
    paint: &PagePaint<'_>,
) {
    let Some(first) = block.lines.first() else {
        return;
    };
    let left = paint.rect.x0;
    match block.kind {
        LayoutKind::Quote | LayoutKind::Verse { quoted: true } => {
            let top = y(block.top.max(start));
            let bottom = y(block.bottom);
            let rule = Line::new(
                Point::new(left - 12.0, top),
                Point::new(left - 12.0, bottom),
            );
            scene.stroke(&Stroke::new(2.0), transform, paint.ink_dim, None, &rule);
        }
        LayoutKind::Separator => {
            let at = y(block.top + (block.bottom - block.top) / 2.0);
            let rule = Line::new(
                Point::new(paint.rect.x0 + paint.rect.width() * 0.2, at),
                Point::new(paint.rect.x0 + paint.rect.width() * 0.8, at),
            );
            scene.stroke(&Stroke::new(1.0), transform, paint.ink_dim, None, &rule);
        }
        _ => {}
    }
    if let Some(bullet) = &block.bullet {
        let at = y(first.y);
        text.draw_style(
            scene,
            bullet,
            paint.ink_dim,
            transform * Affine::translate((left + f64::from(block.indent) - 22.0, at)),
            None,
            &ui::text_render::TextStyle::new(block.size),
        );
    }
}

fn draw_lines(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    block: &LaidBlock,
    start: f32,
    y: &impl Fn(f32) -> f64,
    paint: &PagePaint<'_>,
) {
    for line in &block.lines {
        if line.y + block.line_height <= start {
            continue;
        }
        let line_y = y(line.y);
        // The marks first, under the words they cover.
        for run in &line.runs {
            if run.char_len == 0 {
                continue;
            }
            let width = run_width(text, block, run, line.glyph_scale, paint.base_family);
            for (mark_block, from, to, colour) in paint.marks {
                if *mark_block != block.index {
                    continue;
                }
                let run_from = run.char_start;
                let run_to = run.char_start + run.char_len;
                let lo = (*from).max(run_from);
                let hi = (*to).min(run_to);
                if lo >= hi {
                    continue;
                }
                let (x0, x1) = if width > 0.5 {
                    let scale = width / f64::from(u32::try_from(run.char_len).unwrap_or(1));
                    (
                        block.indent as f64
                            + f64::from(run.x)
                            + scale * (lo.saturating_sub(run_from)) as f64,
                        block.indent as f64
                            + f64::from(run.x)
                            + scale * (hi.saturating_sub(run_from)) as f64,
                    )
                } else {
                    (
                        block.indent as f64 + f64::from(run.x),
                        block.indent as f64 + f64::from(run.x) + 4.0,
                    )
                };
                let band = Rect::new(
                    paint.rect.x0 + x0,
                    line_y - 1.0,
                    paint.rect.x0 + x1,
                    line_y + f64::from(block.size) + 2.0,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colour.with_alpha(0.45),
                    None,
                    &band,
                );
            }
        }
        for run in &line.runs {
            let weight = if run.bold {
                TextWeight::Bold
            } else {
                TextWeight::Regular
            };
            let family = if run.mono {
                FontChoice::Monospace
            } else {
                paint.base_family.clone()
            };
            let x = paint.rect.x0 + f64::from(block.indent) + f64::from(run.x);
            let run_y = line_y + f64::from(run.y);
            if run.mono && !matches!(paint.base_family, FontChoice::Monospace) && run.math.is_none()
            {
                let width = run_width(text, block, run, line.glyph_scale, paint.base_family);
                let chip = Rect::new(
                    x - 3.0,
                    run_y - 1.0,
                    x + width + 3.0,
                    run_y + f64::from(block.size) + 3.0,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    paint.ink.with_alpha(0.06),
                    None,
                    &chip,
                );
            }
            if let Some(math) = &run.math {
                ui::math::draw_layout(
                    scene,
                    transform
                        * Affine::translate((x, run_y))
                        * Affine::scale_non_uniform(f64::from(line.glyph_scale), 1.0),
                    text,
                    math,
                    Point::ZERO,
                    paint.ink,
                );
                continue;
            }
            text.draw_style(
                scene,
                &run.display_text,
                paint.ink,
                transform
                    * Affine::translate((x, run_y))
                    * Affine::scale_non_uniform(f64::from(line.glyph_scale), 1.0),
                None,
                &ui::text_render::TextStyle::new(block.size)
                    .with_weight(weight)
                    .with_italic(run.italic)
                    .with_family(family),
            );
        }
    }
}

/// A run's painted width, in points.
fn run_width(
    text: &mut TextRenderer,
    block: &LaidBlock,
    run: &super::layout::LineRun,
    glyph_scale: f32,
    base: &FontChoice,
) -> f64 {
    if let Some(math) = &run.math {
        return f64::from(math.width) * f64::from(glyph_scale);
    }
    let weight = if run.bold {
        TextWeight::Bold
    } else {
        TextWeight::Regular
    };
    let family = if run.mono {
        FontChoice::Monospace
    } else {
        base.clone()
    };
    f64::from(
        text.measure_font(
            &run.display_text,
            block.size,
            None,
            weight,
            run.italic,
            &family,
        )
        .0,
    ) * f64::from(glyph_scale)
}
