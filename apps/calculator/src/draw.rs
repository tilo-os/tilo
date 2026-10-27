//! Text placement shared by the calculator's painted panels. Every string is
//! set in a [`TextStyle`] taken from a [`TextRole`](ui::tokens::TextRole)
//! (`theme.text(role)`), already scaled by the theme, so nothing here sizes
//! text or reads a palette.

use ui::paint::PaintCtx;
use ui::text_render::TextStyle;
use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

/// Horizontal text alignment for [`text_in`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Draw `text` inside `rect` broken at spaces into at most `max_lines`
/// lines, each aligned, the last elided: a sentence that must not be cut off
/// at its first line.
pub fn text_block(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    text: &str,
    style: &TextStyle,
    color: Color,
    align: Align,
    max_lines: usize,
) {
    if text.is_empty() || rect.width() <= 0.0 {
        return;
    }
    let lines =
        ctx.text
            .clamp_to_lines_style(text, ui::num::f32_of_f64(rect.width()), max_lines, style);
    let line_h = f64::from(style.line_height_px());
    for (index, line) in lines.iter().enumerate() {
        #[allow(clippy::cast_precision_loss, reason = "a handful of lines of text")]
        let top = rect.y0 + line_h * index as f64;
        let band = Rect::new(rect.x0, top, rect.x1, top + line_h);
        text_in(ctx, band, line, style, color, align);
    }
}

/// Draw a single line of `text` inside `rect`, aligned and vertically
/// centred on its measured line box, elided with an ellipsis when it is
/// wider than the rect.
pub fn text_in(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    text: &str,
    style: &TextStyle,
    color: Color,
    align: Align,
) {
    if text.is_empty() || rect.width() <= 0.0 {
        return;
    }
    let shown = ctx
        .text
        .elide_to_width_style(text, ui::num::f32_of_f64(rect.width()), style);
    let (w, h) = ctx.text.measure_style(&shown, None, style);
    let x = match align {
        Align::Left => rect.x0,
        Align::Center => rect.center().x - f64::from(w) / 2.0,
        Align::Right => rect.x1 - f64::from(w),
    };
    let y = ui::text_metrics::centered_text_top(rect.y0, rect.height(), f64::from(h));
    ctx.text(&shown, style, color, Point::new(x, y), None);
}
