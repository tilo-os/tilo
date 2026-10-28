//! What the diff and merge panes paint the same way: code metrics measured in
//! the theme's `Code` role, column headers, and the wash a change takes.

use ui::num::f32_of_f64;
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::{TextRole, WashStrength};
use ui::widget::Tone;
use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

use crate::highlight;

/// The style line numbers and counters are set in: `Caption`, with tabular
/// figures so columns of digits line up.
pub fn number_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption).numeric()
}

/// Measured sizes every code pane shares, read from the theme and the
/// renderer so they follow the text scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CodeMetrics {
    /// One line of code: the `Code` line box, in whole pixels.
    pub row_h: f64,
    /// Top offset that centres a code line in `row_h`.
    pub text_dy: f64,
    /// Width of one monospace cell.
    pub cell_w: f64,
    /// A column header band: one `Label` line between `s2` insets.
    pub header_h: f64,
    /// Inset of code from the pane edge.
    pub pad: f64,
    /// Gap between a gutter or marker and what follows it.
    pub gap: f64,
}

impl CodeMetrics {
    pub fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let code = theme.text(TextRole::Code);
        let line_h = f64::from(tr.measure_style("Ag", None, &code).1);
        let cell_w = f64::from(tr.measure_style("0", None, &code).0);
        let label_h = f64::from(tr.measure_style("Ag", None, &theme.text(TextRole::Label)).1);
        let sp = &theme.tokens.spacing;
        let row_h = line_h.ceil();
        Self {
            row_h,
            text_dy: (row_h - line_h) / 2.0,
            cell_w,
            header_h: (label_h + 2.0 * f64::from(sp.s2)).ceil(),
            pad: f64::from(sp.s3),
            gap: f64::from(sp.s2),
        }
    }

    /// What is known before the first frame: line heights come from the
    /// theme's roles, the glyph width only from a renderer, so it is zero
    /// until the first draw measures it. Nothing paints before then.
    pub fn unmeasured(theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let line = |role| f64::from(theme.text(role).line_height_px());
        let row_h = line(TextRole::Code).ceil();
        let label_h = line(TextRole::Label);
        Self {
            row_h,
            text_dy: 0.0,
            cell_w: 0.0,
            header_h: (label_h + 2.0 * f64::from(sp.s2)).ceil(),
            pad: f64::from(sp.s3),
            gap: f64::from(sp.s2),
        }
    }

    /// Width of a line-number gutter that holds `digits` figures.
    pub fn gutter_w(&self, tr: &mut TextRenderer, theme: &Theme, digits: usize) -> f64 {
        let sample = "0".repeat(digits.max(1));
        let (w, _) = tr.measure_style(&sample, None, &number_style(theme));
        f64::from(w) + 2.0 * self.pad
    }
}

/// A column header: the file's name in `Label` and, after it, the revision in
/// `Caption`, in the band `rect`. `rule` is the colour of the rule under it,
/// the hairline of the ground when `None`.
pub fn paint_header(ctx: &mut PaintCtx<'_>, rect: Rect, label: &str, rule: Option<Color>) {
    let colors = &ctx.theme.tokens.colors;
    let pad = f64::from(ctx.theme.tokens.spacing.s3);
    let gap = f64::from(ctx.theme.tokens.spacing.s2);
    let (name, note) = highlight::split_label(label);
    let room = (rect.width() - 2.0 * pad).max(0.0);
    if !name.is_empty() {
        let label_style = ctx.theme.text(TextRole::Label);
        let shown = ctx
            .text
            .elide_to_width_style(name, f32_of_f64(room), &label_style);
        let used = f64::from(ctx.text.measure_style(&shown, None, &label_style).0);
        ctx.text_role_in_band(
            &shown,
            TextRole::Label,
            colors.text_primary,
            rect.x0 + pad,
            rect,
            None,
        );
        if let Some(note) = note {
            let left = room - used - gap;
            if left > 0.0 {
                let caption = ctx.theme.text(TextRole::Caption);
                let shown = ctx
                    .text
                    .elide_to_width_style(note, f32_of_f64(left), &caption);
                ctx.text_role_in_band(
                    &shown,
                    TextRole::Caption,
                    colors.text_dim,
                    rect.x0 + pad + used + gap,
                    rect,
                    None,
                );
            }
        }
    }
    if let Some(color) = rule {
        let thickness = f64::from(ctx.theme.tokens.metrics.chrome_rule_px.max(1.0)) * 2.0;
        ctx.fill_rect(
            Rect::new(rect.x0, rect.y1 - thickness, rect.x1, rect.y1),
            color,
        );
    } else {
        ui::surface::separator(
            ctx.scene,
            ctx.transform,
            rect.x0,
            rect.x1,
            rect.y1,
            0.0,
            colors.surfaces.content,
        );
    }
}

/// The opaque status wash of `tone` on the content ground; the change under
/// review (`active`) takes the selected wash, which keeps the hue.
pub fn washed(theme: &Theme, tone: Tone, active: bool) -> Color {
    let strength = if active {
        WashStrength::Selected
    } else {
        WashStrength::Rest
    };
    let colors = &theme.tokens.colors;
    colors.wash_on(colors.surfaces.content, tone, strength).fill
}

/// A line's top-left, centred in a row of `m` that starts at `y`.
pub fn line_origin(x: f64, y: f64, m: &CodeMetrics) -> Point {
    Point::new(x, y + m.text_dy)
}
