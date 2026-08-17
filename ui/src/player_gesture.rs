//! Player touch gestures + the animated on-screen level indicator.
//!
//! The Netflix / Grayjay player gesture: a **vertical drag on the right half of
//! the surface adjusts volume**, a **vertical drag on the left half adjusts
//! brightness**, each surfaced by a soft, animated level meter that fades away
//! after the gesture. This module owns the two reusable pieces:
//!
//! * [`VerticalDrag`] — a press→move recognizer that classifies which half the
//!   drag started on and converts vertical travel into a signed level delta
//!   (drag up = increase). It also distinguishes a *tap* (never crossed the
//!   movement threshold) from a *drag*, so the surface can toggle play on a tap
//!   without a drag also toggling it.
//! * [`LevelIndicator`] — a self-fading, themed meter (icon + vertical bar +
//!   percentage) that any surface can paint to show a volume/brightness change.
//!
//! Both are deliberately widget-free and app-agnostic so the exact same volume
//! gesture + indicator can later drive a **global mobile volume control**, not
//! just the video player — the reason they live in `ui` rather than inside
//! `VideoPlayer`.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use crate::icons::Icon;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// Which half of the surface a vertical drag started on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragSide {
    /// Left half — brightness.
    Left,
    /// Right half — volume.
    Right,
}

/// Which quantity a [`LevelIndicator`] is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LevelKind {
    Volume,
    Brightness,
}

/// A vertical-drag recognizer for a video/media surface.
///
/// Constructed on press with [`VerticalDrag::begin`]; fed each pointer move with
/// [`VerticalDrag::update`], which returns the incremental level change (a
/// fraction of the full range, drag *up* positive) once the pointer has moved
/// far enough to count as a drag rather than a tap.
#[derive(Clone, Copy, Debug)]
pub struct VerticalDrag {
    start: Point,
    last: Point,
    side: DragSide,
    /// Surface height the drag maps against (a full-height swipe = full range).
    height: f64,
    /// Whether the pointer has crossed [`Self::THRESHOLD`] (a drag, not a tap).
    is_drag: bool,
}

impl VerticalDrag {
    /// How far (logical px) the pointer must travel before a press is treated as
    /// a drag gesture rather than a tap.
    pub const THRESHOLD: f64 = 8.0;

    /// Begin tracking a press at `start` within `rect`. The starting X picks the
    /// side (left = brightness, right = volume); the rect height sets the drag
    /// sensitivity (a full-height swipe covers the whole 0..1 range).
    pub fn begin(start: Point, rect: Rect) -> Self {
        let side = if start.x >= rect.center().x {
            DragSide::Right
        } else {
            DragSide::Left
        };
        Self {
            start,
            last: start,
            side,
            height: rect.height().max(1.0),
            is_drag: false,
        }
    }

    /// Which half the drag started on.
    pub fn side(&self) -> DragSide {
        self.side
    }

    /// Whether the gesture has become a drag (vs. a still-a-tap press).
    pub fn is_drag(&self) -> bool {
        self.is_drag
    }

    /// Feed the latest pointer position. Returns the incremental level delta (a
    /// signed fraction of the full range, *up* = positive) since the last update,
    /// or `None` while the press has not yet crossed the drag threshold.
    pub fn update(&mut self, p: Point) -> Option<f32> {
        if !self.is_drag {
            if (p.y - self.start.y).abs() < Self::THRESHOLD {
                return None;
            }
            // Cross into drag mode; start integrating from here so the level
            // doesn't jump by the threshold distance on the first step.
            self.is_drag = true;
            self.last = p;
            return Some(0.0);
        }
        // Up (decreasing Y) increases the level.
        let dy = self.last.y - p.y;
        self.last = p;
        Some((dy / self.height) as f32)
    }
}

/// A self-fading level meter (icon + vertical bar + percentage) painted over a
/// media surface while the user changes volume or brightness.
///
/// It holds only its kind, current level and the instant it was last refreshed;
/// opacity is derived from elapsed time so the caller just paints it every frame
/// and drops it once [`LevelIndicator::is_visible`] returns false.
#[derive(Clone, Copy, Debug)]
pub struct LevelIndicator {
    kind: LevelKind,
    level: f32,
    shown_at: Instant,
}

impl LevelIndicator {
    /// How long the indicator stays fully opaque after the last change.
    pub const HOLD: Duration = Duration::from_millis(850);
    /// How long it then takes to fade out.
    pub const FADE: Duration = Duration::from_millis(350);

    /// Show `level` (0..=1) for `kind`, resetting the fade timer.
    pub fn new(kind: LevelKind, level: f32) -> Self {
        Self { kind, level: level.clamp(0.0, 1.0), shown_at: Instant::now() }
    }

    /// Update the shown value + kind and reset the fade timer (a continuing drag).
    pub fn refresh(&mut self, kind: LevelKind, level: f32) {
        self.kind = kind;
        self.level = level.clamp(0.0, 1.0);
        self.shown_at = Instant::now();
    }

    pub fn kind(&self) -> LevelKind {
        self.kind
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    /// Opacity 0..=1: fully opaque for [`Self::HOLD`], then eased to 0 over
    /// [`Self::FADE`].
    pub fn opacity(&self) -> f32 {
        let e = self.shown_at.elapsed();
        if e <= Self::HOLD {
            1.0
        } else {
            let into_fade = (e - Self::HOLD).as_secs_f32();
            (1.0 - into_fade / Self::FADE.as_secs_f32()).clamp(0.0, 1.0)
        }
    }

    /// Whether the indicator still has any visible opacity (else the owner drops
    /// it and stops requesting animation frames).
    pub fn is_visible(&self) -> bool {
        self.opacity() > 0.0
    }

    /// Paint the indicator centred in `surface`, scaled by its current opacity.
    /// Colours come from the [`Theme`]; the glyph tracks the kind and, for
    /// volume, mute (level 0).
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        surface: Rect,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let a = self.opacity();
        if a <= 0.0 || surface.width() <= 0.0 || surface.height() <= 0.0 {
            return;
        }
        let colors = &theme.tokens.colors;
        let accent = colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX];

        // A compact vertical pill, centred on the surface.
        let panel_w = 74.0f64;
        let panel_h = (surface.height() * 0.52).clamp(150.0, 260.0);
        let cx = surface.center().x;
        let cy = surface.center().y;
        let panel = Rect::new(cx - panel_w * 0.5, cy - panel_h * 0.5, cx + panel_w * 0.5, cy + panel_h * 0.5);
        let bg = RoundedRect::from_rect(panel, theme.tokens.radii.lg as f64);
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, (170.0 * a) as u8),
            None,
            &bg,
        );

        let pad = 12.0;
        // Icon at the top.
        let icon_sz = 24.0;
        let icon_r = Rect::new(cx - icon_sz * 0.5, panel.y0 + pad, cx + icon_sz * 0.5, panel.y0 + pad + icon_sz);
        let glyph = match self.kind {
            LevelKind::Brightness => Icon::Sun,
            LevelKind::Volume if self.level <= 0.001 => Icon::VolumeX,
            LevelKind::Volume => Icon::Volume2,
        };
        glyph.draw(scene, transform, icon_r, colors.text_primary.multiply_alpha(a));

        // Percentage at the bottom.
        let label = format!("{}%", (self.level * 100.0).round() as i32);
        let text_size = 13.0f32;
        let (tw, _) = text_renderer.measure(&label, text_size, None);
        text_renderer.draw_text(
            scene,
            &label,
            text_size,
            colors.text_primary.multiply_alpha(a),
            transform * Affine::translate((cx - tw as f64 * 0.5, panel.y1 - pad - text_size as f64)),
            None,
        );

        // Vertical meter between the icon and the label.
        let track_top = icon_r.y1 + pad;
        let track_bottom = panel.y1 - pad - text_size as f64 - pad;
        if track_bottom > track_top {
            let track_w = 6.0;
            let track = Rect::new(cx - track_w * 0.5, track_top, cx + track_w * 0.5, track_bottom);
            let track_shape = RoundedRect::from_rect(track, track_w * 0.5);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.border_standard.multiply_alpha(a),
                None,
                &track_shape,
            );
            // Fill grows from the bottom up.
            let h = track.height() * self.level.clamp(0.0, 1.0) as f64;
            if h > 0.0 {
                let fill = Rect::new(track.x0, track.y1 - h, track.x1, track.y1);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    accent.multiply_alpha(a),
                    None,
                    &RoundedRect::from_rect(fill, track_w * 0.5),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 640.0, 360.0)
    }

    #[test]
    fn side_is_picked_from_the_start_x() {
        // Right half → volume, left half → brightness.
        assert_eq!(VerticalDrag::begin(Point::new(500.0, 180.0), rect()).side(), DragSide::Right);
        assert_eq!(VerticalDrag::begin(Point::new(100.0, 180.0), rect()).side(), DragSide::Left);
    }

    #[test]
    fn a_small_move_is_a_tap_not_a_drag() {
        let mut g = VerticalDrag::begin(Point::new(500.0, 180.0), rect());
        // Under the threshold: no delta, still a tap.
        assert_eq!(g.update(Point::new(500.0, 184.0)), None);
        assert!(!g.is_drag(), "a sub-threshold move stays a tap");
    }

    #[test]
    fn dragging_up_increases_and_down_decreases() {
        let mut g = VerticalDrag::begin(Point::new(500.0, 180.0), rect());
        // Cross the threshold (first update returns 0.0, integration starts here).
        assert_eq!(g.update(Point::new(500.0, 160.0)), Some(0.0));
        assert!(g.is_drag(), "crossing the threshold makes it a drag");
        // Move further up → positive delta.
        let up = g.update(Point::new(500.0, 100.0)).unwrap();
        assert!(up > 0.0, "drag up increases the level ({up})");
        // Move back down → negative delta.
        let down = g.update(Point::new(500.0, 200.0)).unwrap();
        assert!(down < 0.0, "drag down decreases the level ({down})");
    }

    #[test]
    fn indicator_fades_out_after_its_hold_window() {
        let ind = LevelIndicator::new(LevelKind::Volume, 0.5);
        assert_eq!(ind.opacity(), 1.0, "freshly shown → fully opaque");
        assert!(ind.is_visible());
        // Simulate time past HOLD + FADE by back-dating `shown_at`.
        let mut old = ind;
        old.shown_at = Instant::now() - (LevelIndicator::HOLD + LevelIndicator::FADE + Duration::from_millis(50));
        assert_eq!(old.opacity(), 0.0, "past hold + fade → gone");
        assert!(!old.is_visible());
    }
}
