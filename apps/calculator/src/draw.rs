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
