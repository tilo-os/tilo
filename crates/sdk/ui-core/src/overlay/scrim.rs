//! Scrims: the dim behind a modal Panel and the fade under text on media.
//!
//! Both read their colour from the theme (`surfaces.scrim`, `media.scrim`).
//! A scrim is never a literal colour and never an opaque fill, so what it
//! covers stays visible behind it.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{BlendMode, ColorStop, Fill, Gradient};

use crate::theme::Theme;

/// Dim `rect` (the whole window) behind a modal Panel. `openness` runs from
/// 0 (closed) to 1 (open), so the scrim fades in with the panel. Call after
/// everything it dims is drawn into the same scene.
pub fn modal(scene: &mut Scene, transform: Affine, rect: Rect, openness: f32, theme: &Theme) {
    crate::chrome::fill_scrim(
        scene,
        transform,
        rect,
        theme.tokens.colors.surfaces.scrim_strength * openness.clamp(0.0, 1.0),
        theme,
    );
}

/// The ground of a whole-view surface that pictures the windows it covers
/// (the workspace overview): the desktop, blurred and tinted as the
/// floating material is, over everything drawn before it, then the modal
/// dim. Unlike [`modal`] it hides what it covers once `openness` reaches 1,
/// because the surface shows those windows itself and they must not be
/// seen twice; on the way there they fade out under it. Opaque base where
/// the theme does not blur (reduced transparency, forced colours) or no
/// desktop is published. Call after everything it covers is drawn.
pub fn whole_view(scene: &mut Scene, transform: Affine, rect: Rect, openness: f32, theme: &Theme) {
    let openness = openness.clamp(0.0, 1.0);
    if openness <= 0.0 {
        return;
    }
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        openness,
        transform,
        &rect,
    );
    crate::backdrop::desktop::fill(
        scene,
        (transform, Affine::IDENTITY),
        &rect,
        crate::floating_surface::Material::Floating,
        theme,
        theme.tokens.colors.surfaces.base,
    );
    scene.pop_layer();
    modal(scene, transform, rect, openness, theme);
}

/// Dim the whole window behind a modal Panel, the part under the panel
/// included. Floating material blurs the scene recorded behind it, so the
/// scrim belongs under the material: a bright control behind the panel (a
/// primary button) is dimmed with everything else before it is blurred,
/// instead of glowing through the sheet as a coloured smudge. `panel` is
/// where the sheet stands; the dim does not stop at its edge.
pub fn behind_panel(
    scene: &mut Scene,
    transform: Affine,
    window: Rect,
    panel: RoundedRect,
    theme: &Theme,
) {
    let _ = panel;
    modal(scene, transform, window, 1.0, theme);
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

/// The least a fade over media is: over dark content (a night map, a dark
/// photo) a fainter scrim does not move the picture enough to read as a
/// band under the text, so any fade at all is at least this strong.
pub const MIN_FADE_STRENGTH: f32 = 0.4;

/// Fade `rect` from `media.scrim` at `strength` on `edge` to clear on the
/// opposite edge, so text drawn on media near that edge (in `media.ink`)
/// stays legible. One smooth gradient, eased so the fade has no visible
/// start line; never stepped bands. `strength` is clamped to
/// [`MIN_FADE_STRENGTH`]`..=1`; zero paints nothing.
pub fn vertical_fade(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    edge: FadeEdge,
    strength: f32,
    theme: &Theme,
) {
    if strength <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let strength = strength.clamp(MIN_FADE_STRENGTH, 1.0);
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

    /// The scrim is under the sheet's material, not cut around it: the
    /// material blurs a dimmed scene, so a primary button behind a sheet
    /// does not glow through it as a coloured smudge.
    #[test]
    fn the_scrim_runs_under_the_panel_so_its_material_blurs_dimmed_content() {
        let window = Rect::new(0.0, 0.0, 800.0, 600.0);
        let panel = RoundedRect::from_rect(Rect::new(200.0, 150.0, 600.0, 450.0), 18.0);
        let mut scene = Scene::new();
        behind_panel(&mut scene, Affine::IDENTITY, window, panel, &Theme::dark());
        let mut whole = Scene::new();
        modal(&mut whole, Affine::IDENTITY, window, 1.0, &Theme::dark());
        assert!(
            scene.encoding().draw_tags == whole.encoding().draw_tags,
            "the same dim as the whole window's, with no hole cut for the panel"
        );
    }

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

        // Case `a_faint_fade_still_takes_the_minimum_step`.
        {
            const { assert!(MIN_FADE_STRENGTH > 0.0 && MIN_FADE_STRENGTH < 1.0) };
            let mut scene = Scene::new();
            vertical_fade(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FadeEdge::Top,
                0.0,
                &Theme::dark(),
            );
            assert!(
                scene.encoding().draw_tags.is_empty(),
                "no fade asked, none drawn"
            );
        }
    }
}
