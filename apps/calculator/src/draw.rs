//! Small Vello drawing helpers shared across the calculator's panels.
//! Everything reads colours from the caller (which pulls them from the
//! `Theme`), so nothing here hard-codes a palette.

use ui::ColorCompat;
use ui::paint::PaintCtx;
pub use ui::paint::{fill_rect, fill_round, stroke_round, with_alpha};
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Color;

std::thread_local! {
    /// Calculator is custom-painted, so its literal type ramp does not pass
    /// through the toolkit's retained text widgets. The root refreshes this
    /// once per paint from `Theme::text_scale`; keeping it module-local means
    /// it cannot affect another app or a shared widget.
    static TEXT_SCALE: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

/// Set the type scale for Calculator's custom-painted text for this paint pass.
pub fn set_text_scale(scale: f32) {
    TEXT_SCALE.with(|current| current.set(scale.clamp(0.5, 4.0)));
}

/// The multiplier this module must apply itself: the theme's accessibility
/// text scale minus whatever the shared renderer already applies to every
/// size-based `measure`/`draw_text` call. On the live desktop the renderer
/// carries the full scale, so this is 1.0; a headless renderer left at 1×
/// makes it the theme's scale. Either way a 14 px label paints at 28 px at
/// 200 % — never 56 px.
pub fn paint_scale(tr: &TextRenderer, theme: &ui::theme::Theme) -> f32 {
    let renderer = tr.text_scale();
    if renderer.is_finite() && renderer > f32::EPSILON {
        theme.text_scale / renderer
    } else {
        theme.text_scale
    }
}

/// `size` through this paint pass's scale — the renderer-relative size to
/// hand a toolkit painter that sets canvas text beside this module's own.
pub fn type_size(size: f32) -> f32 {
    scaled_size(size)
}

fn scaled_size(size: f32) -> f32 {
    TEXT_SCALE.with(|scale| size * scale.get())
}

/// Measured line box of one line of `text` at `size` through this module's
/// paint scale and the renderer's own — the height `text_in` centres on and
/// stacked labels step by.
pub fn line_box(tr: &mut TextRenderer, size: f32) -> f64 {
    let (_, h) = tr.measure("0", scaled_size(size), None);
    f64::from(h)
}

/// Horizontal text alignment for [`text_in`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// `base` washed `t` of the way toward whichever extreme moves it more —
/// white on a dark key, black on a light one — so hover / press feedback
/// keeps its polarity on every palette instead of a literal white veneer.
pub fn wash(base: Color, t: f64) -> Color {
    mix(base, ui::tokens::Colors::hairline_on(base, 255), t)
}

pub fn line(scene: &mut Scene, t: Affine, a: Point, b: Point, color: Color, width: f64) {
    scene.stroke(&Stroke::new(width), t, color, None, &Line::new(a, b));
}

/// Width one line of `text` paints at through [`text_in`] / [`text_at`] — the
/// same scaled size, so a control sized from this seats the label it draws.
pub fn measure_w(tr: &mut TextRenderer, text: &str, size: f32, weight: TextWeight) -> f64 {
    let (w, _) = tr.measure_weighted(text, scaled_size(size), None, weight);
    f64::from(w)
}

/// Draw a single line of text inside `rect`, aligned + vertically centered.
/// `style.size` is scaled through this paint pass like every label here.
pub fn text_in(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    text: &str,
    style: &TextStyle,
    color: Color,
    align: Align,
) {
    if text.is_empty() {
        return;
    }
    let style = TextStyle {
        size: scaled_size(style.size),
        ..style.clone()
    };
    // Centre on the measured line box, not `size × 1.25`: the renderer may
    // still apply its own accessibility scale on top of `size`.
    let (w, h) = ctx.text.measure_style(text, None, &style);
    let x = match align {
        Align::Left => rect.x0,
        Align::Center => rect.center().x - f64::from(w) / 2.0,
        Align::Right => rect.x1 - f64::from(w),
    };
    let y = rect.y0 + (rect.height() - f64::from(h)) / 2.0;
    ctx.text(text, &style, color, Point::new(x, y), None);
}

/// Draw text at an explicit top-left point.
pub fn text_at(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    pos: Point,
    text: &str,
    size: f32,
    color: Color,
    weight: TextWeight,
) {
    if text.is_empty() {
        return;
    }
    tr.draw_text_weighted(
        scene,
        text,
        scaled_size(size),
        color,
        t * Affine::translate((pos.x, pos.y)),
        None,
        weight,
    );
}

/// Mix two colours by `t` in [0, 1] (straight, non-premultiplied bytes).
pub fn mix(a: Color, b: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| -> u8 {
        (f64::from(x) + (f64::from(y) - f64::from(x)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::from_rgba8(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        lerp(a.a(), b.a()),
    )
}
