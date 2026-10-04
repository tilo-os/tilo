//! Scrims: the dim behind a modal Panel and the fade under text on media.
//!
//! Both read their colour from the theme (`surfaces.scrim`, `media.scrim`).
//! A scrim is never a literal colour and never an opaque fill, so what it
//! covers stays visible behind it.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{ColorStop, Fill, Gradient};

use crate::theme::Theme;

/// Strength of the scrim behind a modal Panel at full openness, as
/// [`crate::chrome::fill_scrim`] reads it: it takes a uniform 21-23 L* off
/// every ground from white to mid grey.
pub const MODAL_STRENGTH: f32 = 0.45;

/// Dim `rect` (the whole window) behind a modal Panel. `openness` runs from
/// 0 (closed) to 1 (open), so the scrim fades in with the panel. Call after
/// everything it dims is drawn into the same scene.
pub fn modal(scene: &mut Scene, transform: Affine, rect: Rect, openness: f32, theme: &Theme) {
    crate::chrome::fill_scrim(
        scene,
        transform,
        rect,
        MODAL_STRENGTH * openness.clamp(0.0, 1.0),
        theme,
    );
}

/// The edge of a [`vertical_fade`] where it is strongest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeEdge {
    /// Strongest at the top of the rect, clear at its bottom.
    Top,
    /// Strongest at the bottom of the rect, clear at its top.
    Bottom,
}

/// Samples of the fade's ease; enough that no band shows at 8 bits.
const FADE_STOPS: usize = 9;

/// Fade `rect` from `media.scrim` at `strength` on `edge` to clear on the
/// opposite edge, so text drawn on media near that edge (in `media.ink`)
/// stays legible. One smooth gradient, eased so the fade has no visible
/// start line; never stepped bands. `strength` is clamped to `0..=1`.
pub fn vertical_fade(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    edge: FadeEdge,
    strength: f32,
    theme: &Theme,
) {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let (strong, clear) = match edge {
        FadeEdge::Top => ((rect.x0, rect.y0), (rect.x0, rect.y1)),
        FadeEdge::Bottom => ((rect.x0, rect.y1), (rect.x0, rect.y0)),
    };
    let colour = theme.tokens.colors.media.scrim;
    let stops: [ColorStop; FADE_STOPS] = std::array::from_fn(|i| {
        #[allow(
            clippy::cast_precision_loss,
            reason = "i < FADE_STOPS, a single-digit index"
        )]
        let t = i as f32 / (FADE_STOPS - 1) as f32;
        ColorStop {
            offset: t,
            color: colour.with_alpha(fade_alpha(f64::from(t), strength)).into(),
        }
    });
    let gradient = Gradient::new_linear(strong, clear).with_stops(stops.as_slice());
    scene.fill(Fill::NonZero, transform, &gradient, None, &rect);
}

/// The fade's opacity at `t` (0 at the strong edge, 1 at the clear one): a
/// smoothstep from `strength` to clear, flat at both ends so neither end of
/// the fade reads as a line.
#[must_use]
pub fn fade_alpha(t: f64, strength: f32) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "t is clamped to 0..=1 before narrowing"
    )]
    let t = t.clamp(0.0, 1.0) as f32;
    strength.clamp(0.0, 1.0) * (1.0 - t * t * (3.0 - 2.0 * t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fade_is_smooth_monotonic_and_one_gradient() {
        // Case `fade_runs_from_full_to_clear_without_a_step`.
        {
            let mut previous = fade_alpha(0.0, 0.8);
            assert!((previous - 0.8).abs() < 1e-6, "full strength at the edge");
            for i in 1..=100 {
                let a = fade_alpha(f64::from(i) / 100.0, 0.8);
                assert!(a <= previous + 1e-6, "the fade never brightens again");
                // A smoothstep's steepest 1 % moves 1.5 % of the strength;
                // a stepped band moves the whole step at once.
                assert!(previous - a <= 0.8 * 0.016, "no band at {i} %");
                previous = a;
            }
            assert!(previous.abs() < 1e-6, "clear at the far edge");
        }

        // Case `painting_is_one_gradient_fill_not_bands`.
        {
            let mut scene = Scene::new();
            vertical_fade(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 400.0, 120.0),
                FadeEdge::Bottom,
                0.7,
                &Theme::light(),
            );
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|tag| **tag == vello_encoding::DrawTag::LINEAR_GRADIENT)
                .count();
            assert_eq!(fills, 1, "one linear gradient fill");
            assert_eq!(scene.encoding().draw_tags.len(), 1, "and nothing else");
        }
    }
}
