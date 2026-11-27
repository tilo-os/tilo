//! What resizing a window from its edge shows: the cursor first, the window's outline lit
//! along that whole edge once the pointer rests there, primary ink while the edge is carried, and
//! the size the window is taking in a chip beside the edge that fades once it is let go
//! (`docs/design-language.md`, "Window edge resize"). Nothing marks an edge at rest, and the
//! accent never enters it: the light is ink, quiet until the person asks for the edge.
//!
//! One path for the shell's windows (`ui.WindowFrame`) and the canvas's places (`ui.MapPin`):
//! both ask [`ResizeFeedback`] what stands lit, and both paint it with [`paint_edge`] and
//! [`paint_size_chip`], in the theme's own roles.

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use tilo_ui_base::ColorCompat;
use tilo_ui_base::motion::Follow;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;

use crate::time::{Duration, Instant};
use crate::widget::primitives::{SurfaceLevel, TextLineFace};

/// Which edge of a window a pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Leading,
    Trailing,
    Top,
    Bottom,
}

impl Side {
    /// The other edge across the window.
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Leading => Self::Trailing,
            Self::Trailing => Self::Leading,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

/// How the edge is lit: quietly at rest under the pointer, in primary ink while it is carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lit {
    Resting,
    Dragging,
}

/// How long the pointer rests on an edge before its outline lights: long enough that sweeping
/// across a window offers no resize, short enough to feel like the edge answering the pointer.
pub const DWELL: Duration = Duration::from_millis(300);

/// How wide the light line is, in logical pixels.
const LIGHT_WIDTH: f64 = 2.0;

/// The distance the light and the chip stand inside the edge.
const INSET: f64 = 1.0;

/// The chip's air around its words, and between it and the edge it stands beside.
const CHIP_PAD_X: f64 = 8.0;
const CHIP_PAD_Y: f64 = 4.0;
const CHIP_GAP: f64 = 10.0;

/// The light line along `side` of `rect`, inset so it is wholly inside the window.
#[must_use]
pub fn edge_line(rect: Rect, side: Side) -> Line {
    let (x0, x1) = (rect.x0 + INSET, rect.x1 - INSET);
    let (y0, y1) = (rect.y0 + INSET, rect.y1 - INSET);
    match side {
        Side::Leading => Line::new((x0 + LIGHT_WIDTH / 2.0, y0), (x0 + LIGHT_WIDTH / 2.0, y1)),
        Side::Trailing => Line::new((x1 - LIGHT_WIDTH / 2.0, y0), (x1 - LIGHT_WIDTH / 2.0, y1)),
        Side::Top => Line::new((x0, y0 + LIGHT_WIDTH / 2.0), (x1, y0 + LIGHT_WIDTH / 2.0)),
        Side::Bottom => Line::new((x0, y1 - LIGHT_WIDTH / 2.0), (x1, y1 - LIGHT_WIDTH / 2.0)),
    }
}

/// Paint `side` of `rect` lit, in the ink the theme gives a resting or a carried edge. `opacity`
/// scales it for the fade after the pointer leaves.
pub fn paint_edge(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    rect: Rect,
    side: Side,
    lit: Lit,
    opacity: f32,
) {
    if opacity <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let ink = match lit {
        Lit::Resting => theme.tokens.colors.text_dim,
        Lit::Dragging => theme.tokens.colors.text_primary,
    };
    let line = edge_line(rect, side);
    let (x0, y0, x1, y1) = match side {
        Side::Leading | Side::Trailing => (
            line.p0.x - LIGHT_WIDTH / 2.0,
            line.p0.y,
            line.p0.x + LIGHT_WIDTH / 2.0,
            line.p1.y,
        ),
        Side::Top | Side::Bottom => (
            line.p0.x,
            line.p0.y - LIGHT_WIDTH / 2.0,
            line.p1.x,
            line.p0.y + LIGHT_WIDTH / 2.0,
        ),
    };
    let capsule = RoundedRect::from_rect(Rect::new(x0, y0, x1, y1), LIGHT_WIDTH / 2.0);
    scene.fill(
        Fill::NonZero,
        transform,
        ink.with_alpha_factor(opacity),
        None,
        &capsule,
    );
}

/// The size a window shows while it is resized, as the words a chip says: whole logical pixels.
#[must_use]
pub fn size_label(width: f64, height: f64) -> String {
    format!(
        "{} × {}",
        width.round().max(0.0) as i64,
        height.round().max(0.0) as i64
    )
}

/// Paint the window's size in a chip beside its `side` edge of `rect`, centred on the edge's
/// middle: a piece of the floating material, so it reads over the window and its surroundings
/// alike. It stands on the window's own ground, so its body clears that ground by the material's
/// spread in both themes, never only by the step between the panel and the window level
/// ([`crate::widget::floating_surface::card_ground_over`]).
pub fn paint_size_chip(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    side: Side,
    size: (f64, f64),
    opacity: f32,
) {
    if opacity <= 0.0 {
        return;
    }
    let label = size_label(size.0, size.1);
    let style = theme.tokens.typography.style(TextRole::Caption);
    let face = TextLineFace::styled(&label, &style, TextRole::Caption);
    let (w, h) = face.measure(text);
    let (w, h) = (w + 2.0 * CHIP_PAD_X, h + 2.0 * CHIP_PAD_Y);
    let y = rect.center().y - h / 2.0;
    let x = match side {
        Side::Leading => rect.x0 + CHIP_GAP,
        _ => rect.x1 - CHIP_GAP - w,
    };
    let chip = Rect::new(x, y, x + w, y + h);
    let ground = SurfaceLevel::Window.color(theme);
    crate::widget::floating_surface::paint_card_shadow(scene, transform, chip, opacity, theme);
    crate::widget::floating_surface::paint_card_body_with(
        scene,
        transform,
        chip,
        crate::widget::floating_surface::card_ground_over(ground, theme),
        theme,
    );
    let mut ctx = PaintCtx::new(scene, transform, text, theme);
    face.paint(
        &mut ctx,
        chip,
        theme.tokens.colors.text_primary.with_alpha_factor(opacity),
    );
}

/// The resize feedback one window shows: which edge the pointer rests on (and since when),
/// which edge is carried, and the size the carried edge is taking until the chip fades.
#[derive(Clone, Debug, Default)]
pub struct ResizeFeedback {
    /// The edge under the pointer, and when it came there: the light shows once it rested.
    hover: Option<(Side, Instant)>,
    /// The edge being carried, and the size the window had when it was grabbed.
    dragging: Option<(Side, (f64, f64))>,
    /// The size the chip says, the edge it stands beside, and when the person let go.
    chip: Option<(Side, (f64, f64), Option<Instant>)>,
}

impl ResizeFeedback {
    /// A fresh window, with nothing lit.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The pointer is on `side`, or off every edge.
    pub fn hover(&mut self, side: Option<Side>, now: Instant) {
        if self.hover.map(|(held, _)| held) == side {
            return;
        }
        self.hover = side.map(|edge| (edge, now));
    }

    /// The edge was grabbed at `size` (the window's width and height then): the chip says the
    /// size it takes as the pointer moves it, and the light is primary ink.
    pub fn begin(&mut self, side: Side, size: (f64, f64)) {
        self.dragging = Some((side, size));
        self.hover = None;
        self.chip = Some((side, size, None));
    }

    /// The carried edge is at `size` now.
    pub fn carrying(&mut self, size: (f64, f64)) {
        if let Some((_, held, _)) = &mut self.chip {
            *held = size;
        }
    }

    /// The edge was let go: the light goes, the chip stays a moment and fades.
    pub fn end(&mut self, now: Instant) {
        self.dragging = None;
        if let Some((side, size, _)) = self.chip {
            self.chip = Some((side, size, Some(now)));
        }
    }

    /// Nothing is carried, the pointer left, and any fade is over: the window offers no edge.
    pub fn cancel(&mut self) {
        self.dragging = None;
        self.chip = None;
        self.hover = None;
    }

    /// Whether the edge is being carried.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// The edge being carried, when one is.
    #[must_use]
    pub fn grabbed(&self) -> Option<Side> {
        self.dragging.map(|(side, _)| side)
    }

    /// The edge that lights now, and how: the carried edge while dragging, else the rested one in
    /// the quiet ink. `None` while nothing is lit, or while the pointer is still on its way there.
    #[must_use]
    pub fn lit(&self, now: Instant) -> Option<(Side, Lit)> {
        if let Some((side, _)) = self.dragging {
            return Some((side, Lit::Dragging));
        }
        let (side, since) = self.hover?;
        (now.saturating_duration_since(since) >= DWELL).then_some((side, Lit::Resting))
    }

    /// The opacity of the resting light: it comes up over the theme's quick fade once the pointer
    /// has rested, and is gone at once after the pointer leaves.
    #[must_use]
    pub fn light_alpha(&self, now: Instant, fade: Duration) -> f32 {
        if self.dragging.is_some() {
            return 1.0;
        }
        let Some((_, since)) = self.hover else {
            return 0.0;
        };
        if fade.is_zero() {
            // A theme that holds still lights the edge the moment the pointer has rested.
            return f32::from(u8::from(now.saturating_duration_since(since) >= DWELL));
        }
        // The light is whole once its fade is behind it; while it comes up, it is as far through
        // that fade as the follow's own would have gone.
        let left = (since + DWELL + fade).saturating_duration_since(now);
        1.0 - Follow::fading(left, fade)
    }

    /// The chip's words and ink now: the size being taken while dragging, and for the theme's
    /// quick fade after the edge is let go.
    #[must_use]
    pub fn chip(&self, now: Instant, fade: Duration) -> Option<((f64, f64), Side, f32)> {
        let (side, size, released) = self.chip.as_ref()?;
        let alpha = match released {
            None => 1.0,
            Some(at) => {
                if fade.is_zero() {
                    // A theme that holds still drops the chip the moment it is let go.
                    0.0
                } else {
                    Follow::fading((*at + fade).saturating_duration_since(now), fade)
                }
            }
        };
        (alpha > 0.0).then_some((*size, *side, alpha))
    }

    /// Whether a frame is needed to finish a fade.
    #[must_use]
    pub fn needs_frame(&self, now: Instant, fade: Duration) -> bool {
        if let Some((_, _, Some(at))) = self.chip
            && now.saturating_duration_since(at) < fade
        {
            return true;
        }
        if let Some((_, since)) = self.hover
            && !self.is_dragging()
            && now.saturating_duration_since(since) < DWELL + fade
        {
            return true;
        }
        false
    }

    /// The chip is over: a view that kept it for its fade drops it.
    pub fn forget_chip(&mut self) {
        self.chip = None;
    }
}

/// Where the light stands along `side` of `rect`, for a test or a hint that asks.
#[must_use]
pub fn edge_point(rect: Rect, side: Side, along: f64) -> Point {
    let line = edge_line(rect, side);
    Point::new(
        line.p0.x + (line.p1.x - line.p0.x) * along,
        line.p0.y + (line.p1.y - line.p0.y) * along,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_ui_base::a11y::{contrast_ratio, relative_luminance};
    use tilo_ui_base::tokens::finish::material_spread;

    /// The size chip is a piece of the floating material standing on a window: in either theme
    /// its body clears the window it stands over by the material's own spread, and its ink reads
    /// on that body. A chip lit only by the step between the panel and the window level (a
    /// thirty-second of a lightness in dark, an eighth of one in light) reads as the window.
    #[test]
    fn the_size_chip_clears_the_window_it_stands_over_in_both_themes() {
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let window = SurfaceLevel::Window.color(&theme);
            let chip = crate::widget::floating_surface::card_ground_over(window, &theme);
            let spread = material_spread(&theme.tokens.colors, window);
            let body = contrast_ratio(chip, window);
            // The band steps toward the spread until it reaches it, so its answer stands a
            // search step under the spread at most.
            assert!(
                body >= spread - 0.02,
                "in {name} the chip's body clears the window by {body}, the material's spread \
                 there {spread}: {chip:?} on {window:?}"
            );
            let ink = contrast_ratio(theme.tokens.colors.text_primary, chip);
            assert!(
                ink >= 3.0,
                "in {name} the chip's ink reads on its body: {ink} on {chip:?}"
            );
            assert!(
                relative_luminance(chip) != relative_luminance(window),
                "in {name} the chip is not the window's own tone"
            );
        }
    }
}
