//! A transient floating **status bubble** — a small overlay pill that appears
//! on a state change (a page turn, a zoom step) or while the pointer hovers its
//! anchor, then quietly fades away, so a reader / editor surface need not spend
//! a permanent strip of chrome on status it only occasionally needs.
//!
//! This is the reusable timing + presentation core, deliberately app-agnostic
//! like [`crate::widget::TooltipHost`]: the app owns *what* the bubble says and
//! *where* it floats, and pokes the controller when the state it reflects
//! changes (or reports pointer hover over the bubble's zone). The controller
//! owns *when* it is visible and its fade, all on an injected wall clock so it
//! is unit-testable without an event loop.
//!
//! The intent is a fleet pattern: any app that currently pays for an always-on
//! status strip can drop the strip, give the freed space back to its content,
//! and surface the same information as a [`StatusBubble`] that shows on change
//! or hover. The first adopter is the PDF reader's page indicator.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::Color;

use crate::color_compat::ColorCompat;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;

/// How long the bubble lingers after a poke (or after the pointer leaves its
/// zone) before it fades out, absent any new activity.
pub const DEFAULT_LINGER: Duration = Duration::from_millis(1900);
/// The tail of the linger spent fading to fully transparent, so the bubble
/// leaves quietly rather than blinking out.
const FADE: Duration = Duration::from_millis(300);

/// Transient-visibility controller for a floating status bubble.
///
/// Lifecycle:
/// - [`poke`](Self::poke) — the reflected state changed; show and (re)start the
///   linger countdown.
/// - [`set_hover`](Self::set_hover) — the pointer entered/left the bubble's
///   zone; while hovered the bubble is pinned open, and on leave it lingers
///   briefly then fades.
/// - [`tick`](Self::tick) — advance the wall clock; hide once the linger (plus
///   fade) elapses.
///
/// Every method takes an explicit `now: Instant`, so a test drives the whole
/// show → linger → fade → hide cycle with no real clock.
#[derive(Clone, Debug)]
pub struct StatusBubble {
    linger: Duration,
    /// When the current linger countdown started, if one is running. `None`
    /// while the pointer pins the bubble open.
    shown_at: Option<Instant>,
    /// Whether the pointer is currently over the bubble's zone.
    hovered: bool,
}

impl Default for StatusBubble {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBubble {
    /// A bubble with the default [`DEFAULT_LINGER`], initially hidden.
    pub fn new() -> Self {
        Self { linger: DEFAULT_LINGER, shown_at: None, hovered: false }
    }

    /// Override how long the bubble lingers after a poke / hover-leave.
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.linger = linger;
        self
    }

    /// The reflected state changed (a page turn, a zoom step): reveal the
    /// bubble and restart the linger countdown. Returns whether visibility
    /// changed (so the caller can request a redraw).
    pub fn poke(&mut self, now: Instant) -> bool {
        let was = self.is_visible(now);
        self.shown_at = Some(now);
        !was
    }

    /// Report whether the pointer is over the bubble's zone. While `true` the
    /// bubble is pinned fully open; on the transition to `false` it starts a
    /// fresh linger countdown so it fades a moment after the pointer leaves.
    /// Returns whether visibility changed.
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

    /// Whether the bubble should be drawn at `now`.
    pub fn is_visible(&self, now: Instant) -> bool {
        self.alpha(now) > 0.0
    }

    /// The bubble's opacity at `now` in `0.0..=1.0`: fully opaque while hovered
    /// or fresh, ramping to zero across the final [`FADE`] of the linger.
    pub fn alpha(&self, now: Instant) -> f32 {
        if self.hovered {
            return 1.0;
        }
        let Some(t) = self.shown_at else { return 0.0 };
        let e = now.saturating_duration_since(t);
        if e >= self.linger {
            return 0.0;
        }
        let remain = self.linger - e;
        if remain < FADE {
            (remain.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Whether the controller wants the event loop to keep pumping frames — it
    /// is counting down a linger (so the fade animates and the bubble hides on
    /// its own). Hovered / hidden states need no frames.
    pub fn needs_frame(&self, now: Instant) -> bool {
        !self.hovered && self.shown_at.map(|t| now.saturating_duration_since(t) < self.linger).unwrap_or(false)
    }
}

/// The pill's outer `(width, height)` for `text` at `text_size`, with room for a
/// leading and trailing control glyph slot of `control` px each (`0.0` for a
/// plain label). Height is floored so a single line never looks pinched.
pub fn pill_size(text: &str, tr: &mut TextRenderer, text_size: f32, control: f64) -> (f64, f64) {
    let (tw, _) = tr.measure_weighted(text, text_size, None, TextWeight::Medium);
    let pad_x = 14.0;
    let w = tw as f64 + 2.0 * pad_x + 2.0 * control;
    let h = (text_size as f64 + 16.0).max(30.0);
    (w, h)
}

/// Paint the bubble: a soft drop shadow, an opaque themed surface, a hairline
/// border and the centred label. `rect` is the already-positioned pill; all
/// colours come from the `Theme` so it tracks light / dark. The caller sets the
/// opacity by wrapping this in a `scene.push_layer(... alpha ...)` using
/// [`StatusBubble::alpha`].
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

    // Drop shadow through the shared painter, so the pill is lifted by
    // the same material as every other surface. This was two stacked
    // hard-edged rounded rects with their own inflates and offsets —
    // "soft" in the comment only; a stack of two solid copies has two
    // visible steps in it, and neither the offsets nor the strength came
    // from the token it claimed to be tinted from.
    crate::surface::shadow(scene, transform, rect, radius, &theme.tokens.elevation.e2);

    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(vello::peniko::Fill::NonZero, transform, theme.tokens.colors.surfaces.overlay, None, &shape);
    scene.stroke(&Stroke::new(1.0), transform, theme.tokens.colors.border_standard, None, &shape);

    let (tw, _) = tr.measure_weighted(text, text_size, None, TextWeight::Medium);
    let tx = rect.x0 + (rect.width() - tw as f64) / 2.0;
    let ty = rect.y0 + (rect.height() - text_size as f64) / 2.0;
    tr.draw_text_weighted(
        scene,
        text,
        text_size,
        theme.tokens.colors.text_primary,
        transform * Affine::translate((tx, ty)),
        None,
        TextWeight::Medium,
    );
}

/// A small left/right chevron centred in `r`, pointing `left`. Handy for a
/// bubble that doubles as a stepper (the reader's page nav).
pub fn draw_chevron(scene: &mut Scene, transform: Affine, r: Rect, color: Color, left: bool) {
    use vello::kurbo::BezPath;
    let cx = (r.x0 + r.x1) / 2.0;
    let cy = (r.y0 + r.y1) / 2.0;
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
    fn poke_reveals_then_fades_after_linger() {
        let mut b = StatusBubble::new().with_linger(Duration::from_millis(1000));
        let t0 = Instant::now();
        assert!(!b.is_visible(t0), "hidden until poked");
        assert!(b.poke(t0), "poke reveals and reports the change");
        assert!(b.is_visible(t0), "visible right after a poke");
        assert!((b.alpha(t0) - 1.0).abs() < 1e-6, "fully opaque when fresh");
        // Mid-linger, before the fade tail: still fully opaque.
        assert!((b.alpha(t0 + Duration::from_millis(500)) - 1.0).abs() < 1e-6);
        assert!(b.needs_frame(t0 + Duration::from_millis(500)), "counts down -> wants frames");
        // Into the fade tail: partial opacity.
        let a = b.alpha(t0 + Duration::from_millis(850));
        assert!(a > 0.0 && a < 1.0, "fades out across the tail: {a}");
        // Past the linger: gone.
        assert!(!b.is_visible(t0 + Duration::from_millis(1000)));
        assert!(!b.needs_frame(t0 + Duration::from_millis(1000)));
    }

    #[test]
    fn hover_pins_open_then_lingers_after_leave() {
        let mut b = StatusBubble::new().with_linger(Duration::from_millis(1000));
        let t0 = Instant::now();
        b.set_hover(true, t0);
        // Pinned open regardless of time while the pointer stays over it.
        assert!(b.is_visible(t0 + Duration::from_secs(60)), "hover pins the bubble open");
        assert!(!b.needs_frame(t0 + Duration::from_secs(60)), "a pinned bubble needs no frames");
        // Leaving starts a fresh linger.
        b.set_hover(false, t0 + Duration::from_secs(60));
        assert!(b.is_visible(t0 + Duration::from_secs(60)), "still up right after leaving");
        assert!(
            !b.is_visible(t0 + Duration::from_secs(60) + Duration::from_millis(1000)),
            "fades a linger after the pointer leaves"
        );
    }

    #[test]
    fn a_fresh_poke_restarts_the_countdown() {
        let mut b = StatusBubble::new().with_linger(Duration::from_millis(1000));
        let t0 = Instant::now();
        b.poke(t0);
        // Almost expired…
        assert!(b.is_visible(t0 + Duration::from_millis(900)));
        // …then a new poke restarts the full linger.
        b.poke(t0 + Duration::from_millis(950));
        assert!(b.is_visible(t0 + Duration::from_millis(1800)), "the second poke extended it");
        assert!(!b.is_visible(t0 + Duration::from_millis(1950)));
    }
}
