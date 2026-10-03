//! A transient floating **status bubble** — a small overlay pill that appears
//! on a state change (a page turn, a zoom step) or while the pointer hovers
//! its anchor, then quietly fades away, so a reader/editor surface need not
//! spend a permanent strip of chrome on status it only occasionally needs.
//!
//! App-agnostic like [`crate::widget::TooltipHost`]: the app owns *what* it
//! says and *where* it floats; the controller owns *when* it is visible and
//! its fade, on an injected wall clock so it is unit-testable without an
//! event loop.

use crate::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Rect, Stroke, Vec2};
use vello::peniko::Color;

use crate::text_metrics::centered_text_top;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;

/// How long the bubble lingers after a poke (or after the pointer leaves its
/// zone) before it fades out, absent any new activity.
pub const DEFAULT_LINGER: Duration = Duration::from_millis(1900);
/// The tail of the linger spent fading to fully transparent, so the bubble
/// leaves quietly rather than blinking out.
const FADE: Duration = Duration::from_millis(300);

/// Every method takes an explicit `now: Instant`, so a test drives the
/// whole show → linger → fade → hide cycle without a wall clock.
#[derive(Clone, Debug)]
pub struct StatusBubble {
    linger: Duration,
    /// `None` while the pointer pins the bubble open.
    shown_at: Option<Instant>,
    hovered: bool,
}

impl Default for StatusBubble {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBubble {
    pub fn new() -> Self {
        Self {
            linger: DEFAULT_LINGER,
            shown_at: None,
            hovered: false,
        }
    }

    #[must_use]
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.linger = linger;
        self
    }

    /// Returns whether visibility changed (so the caller can request a redraw).
    pub fn poke(&mut self, now: Instant) -> bool {
        let was = self.is_visible(now);
        self.shown_at = Some(now);
        !was
    }

    /// While `true` the bubble is pinned fully open; on the transition to
    /// `false` it starts a fresh linger countdown.
    pub fn set_hover(&mut self, over: bool, now: Instant) -> bool {
        let was = self.is_visible(now);
        if over {
            self.hovered = true;
            self.shown_at = None;
        } else if self.hovered {
            self.hovered = false;
            self.shown_at = Some(now);
        }
        self.is_visible(now) != was
    }

    pub fn is_visible(&self, now: Instant) -> bool {
        self.alpha(now) > 0.0
    }

    /// Fully opaque while hovered or fresh, ramping to zero across the
    /// final `FADE` of the linger.
    pub fn alpha(&self, now: Instant) -> f32 {
        if self.hovered {
            return 1.0;
        }
        let Some(t) = self.shown_at else { return 0.0 };
        let e = now.saturating_duration_since(t);
        if e >= self.linger {
            return 0.0;
        }
        let Some(remain) = self.linger.checked_sub(e) else {
            // `e >= self.linger` already returned above, so this is
            // unreachable in practice; treat it as fully faded rather than
            // panic if that invariant ever slips.
            return 0.0;
        };
        if remain < FADE {
            (remain.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Hovered/hidden states need no frames.
    pub fn needs_frame(&self, now: Instant) -> bool {
        !self.hovered
            && self
                .shown_at
                .is_some_and(|t| now.saturating_duration_since(t) < self.linger)
    }
}

/// `control` is a leading+trailing glyph slot in px each (`0.0` for a
/// plain label).
///
/// `text_size` is renderer-relative: the renderer applies the accessibility
/// text scale, so pass a design size (`12.0`) or a theme token divided once
/// through [`crate::text_metrics::renderer_type_size`] — never a raw
/// `theme.tokens.typography.scale.*` value, which paints 4× at 200 %.
pub fn pill_size(text: &str, tr: &mut TextRenderer, text_size: f32, control: f64) -> (f64, f64) {
    let (tw, th) = tr.measure_weighted(text, text_size, None, TextWeight::Medium);
    let pad_x = 14.0;
    let w = f64::from(tw) + 2.0 * pad_x + 2.0 * control;
    // `TextRenderer` applies the user's text scale to metrics, so the pill
    // geometry must use the same effective font size.  Otherwise at 200% the
    // glyphs extend through the lower edge of a 30px bubble.
    let h = pill_height(text_size, tr.text_scale()).max(f64::from(th) + 12.0);
    (w, h)
}

/// Height shared by layout reservations and the rendered status pill.
///
/// This is the same renderer-relative `TextStyle` that
/// [`TextRenderer::measure_weighted`] scales before measuring, plus the
/// pill's vertical padding. Keeping this metric free of a renderer lets the
/// layout pass reserve exactly the line box the later draw pass needs.
pub fn pill_height(text_size: f32, text_scale: f32) -> f64 {
    let text_scale = sanitized_text_scale(text_scale);
    let style = TextStyle::new(text_size * text_scale).with_weight(TextWeight::Medium);
    f64::from(style.line_height_px() + 12.0).max(30.0)
}

/// Mirrors [`TextRenderer::set_text_scale`] so layout and renderer-relative
/// status geometry handle invalid accessibility preferences identically.
fn sanitized_text_scale(text_scale: f32) -> f32 {
    if text_scale.is_finite() {
        text_scale.clamp(0.5, 4.0)
    } else {
        1.0
    }
}

/// The caller sets opacity by wrapping this in a `scene.push_layer(...
/// alpha ...)` using [`StatusBubble::alpha`]. `text_size` follows the
/// [`pill_size`] contract: renderer-relative, scaled once by the renderer.
pub fn draw_pill(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    text: &str,
    text_size: f32,
) {
    let radius = rect.height() / 2.0;

    crate::surface::shadow(scene, transform, rect, radius, &theme.tokens.elevation.e2);

    // Overlay fill under a rest edge derived from it (HIG §9.5): the
    // declared `border_standard` this replaces is one fixed colour per theme
    // and lands within rounding distance of the dark preset's overlay.
    crate::surface::card(
        scene,
        transform,
        rect,
        radius,
        theme.tokens.colors.surfaces.overlay,
    );

    let (tw, th) = tr.measure_weighted(text, text_size, None, TextWeight::Medium);
    let tx = rect.x0 + (rect.width() - f64::from(tw)) / 2.0;
    // Centre the measured line box, not the font size.
    let ty = centered_text_top(rect.y0, rect.height(), f64::from(th));
    tr.draw_text_weighted(
        scene,
        text,
        text_size,
        theme.tokens.colors.text_primary,
        transform.pre_translate(Vec2::new(tx, ty)),
        None,
        TextWeight::Medium,
    );
}

pub fn draw_chevron(scene: &mut Scene, transform: Affine, r: Rect, color: Color, left: bool) {
    use vello::kurbo::BezPath;
    let cx = f64::midpoint(r.x0, r.x1);
    let cy = f64::midpoint(r.y0, r.y1);
    let dx = 3.5;
    let dy = 5.0;
    let mut p = BezPath::new();
    if left {
        p.move_to((cx + dx, cy - dy));
        p.line_to((cx - dx, cy));
        p.line_to((cx + dx, cy + dy));
    } else {
        p.move_to((cx - dx, cy - dy));
        p.line_to((cx + dx, cy));
        p.line_to((cx - dx, cy + dy));
    }
    scene.stroke(&Stroke::new(1.8), transform, color, None, &p);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bubble_geometry_reveal_linger_and_fade_follow_pokes_and_hover() {
        // Case `pill_reservation_matches_the_renderer_line_box_at_supported_scales`.
        {
            for scale in [1.0, 1.5, 2.0, 4.0] {
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let (_, measured) = pill_size("Saved", &mut tr, 12.0, 0.0);
                let reserved = pill_height(12.0, tr.text_scale());
                assert_eq!(
                    reserved, measured,
                    "layout reservation must equal draw geometry at {scale}x"
                );
            }
            assert_eq!(pill_height(12.0, 1.0), 30.0);
            assert_eq!(pill_height(12.0, 2.0), 42.0);
        }

        // Case `poke_reveals_then_fades_after_linger`.
        {
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            assert!(!b.is_visible(t0), "hidden until poked");
            assert!(b.poke(t0), "poke reveals and reports the change");
            assert!(b.is_visible(t0), "visible right after a poke");
            assert!((b.alpha(t0) - 1.0).abs() < 1e-6, "fully opaque when fresh");
            // Mid-linger, before the fade tail: still fully opaque.
            assert!((b.alpha(t0 + Duration::from_millis(500)) - 1.0).abs() < 1e-6);
            assert!(
                b.needs_frame(t0 + Duration::from_millis(500)),
                "counts down -> wants frames"
            );
            // Into the fade tail: partial opacity.
            let a = b.alpha(t0 + Duration::from_millis(850));
            assert!(a > 0.0 && a < 1.0, "fades out across the tail: {a}");
            // Past the linger: gone.
            assert!(!b.is_visible(t0 + Duration::from_secs(1)));
            assert!(!b.needs_frame(t0 + Duration::from_secs(1)));
        }

        // Case `hover_pins_open_then_lingers_after_leave`.
        {
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            b.set_hover(true, t0);
            // Pinned open regardless of time while the pointer stays over it.
            assert!(
                b.is_visible(t0 + Duration::from_secs(60)),
                "hover pins the bubble open"
            );
            assert!(
                !b.needs_frame(t0 + Duration::from_secs(60)),
                "a pinned bubble needs no frames"
            );
            // Leaving starts a fresh linger.
            b.set_hover(false, t0 + Duration::from_secs(60));
            assert!(
                b.is_visible(t0 + Duration::from_secs(60)),
                "still up right after leaving"
            );
            assert!(
                !b.is_visible(t0 + Duration::from_secs(60) + Duration::from_secs(1)),
                "fades a linger after the pointer leaves"
            );
        }

        // Case `a_fresh_poke_restarts_the_countdown`.
        {
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            b.poke(t0);
            // Almost expired…
            assert!(b.is_visible(t0 + Duration::from_millis(900)));
            // …then a new poke restarts the full linger.
            b.poke(t0 + Duration::from_millis(950));
            assert!(
                b.is_visible(t0 + Duration::from_millis(1800)),
                "the second poke extended it"
            );
            assert!(!b.is_visible(t0 + Duration::from_millis(1950)));
        }
    }
}
