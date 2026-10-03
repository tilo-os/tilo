//! Quiet current: the shared authentication artwork for the lock and greeter.
//!
//! Four curved sheets catch a narrow band of light. Their folds move slowly,
//! leaving the clock and credentials on calm ground. Everything is vector
//! geometry and theme color; no image, texture, shader, or frame history is
//! needed. Callers own the clock and request frames only while visible.

use std::f64::consts::TAU;

use vello::{
    Scene,
    kurbo::{Affine, BezPath, Point, Rect, Stroke},
    peniko::{BlendMode, Color, Fill, Gradient},
};

use crate::{Theme, tokens::colors::mix8};

const PERIOD_SECONDS: f64 = 120.0;

/// Whether the caller should schedule animation frames for this backdrop.
///
/// High contrast omits decorative geometry entirely. Reduced motion preserves
/// the initial composition without requiring a redraw timer.
#[must_use]
pub fn needs_animation(theme: &Theme) -> bool {
    !theme.reduce_motion && !theme.colors_are_forced()
}

/// Paint an opaque authentication backdrop inside `bounds`.
///
/// The elapsed time is supplied by the host, making stills and recordings
/// deterministic. Invalid or negative times use the initial composition; long
/// uptimes wrap before trigonometry. Empty or non-finite bounds draw nothing.
/// The base is always opaque, even for a custom theme with translucent tokens.
pub fn draw(scene: &mut Scene, theme: &Theme, bounds: Rect, elapsed_seconds: f64) {
    if !bounds.is_finite() || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return;
    }

    let base = theme.tokens.colors.surfaces.base.with_alpha(1.0);
    scene.fill(Fill::NonZero, Affine::IDENTITY, base, None, &bounds);
    if theme.colors_are_forced() {
        return;
    }

    let phase = if needs_animation(theme) && elapsed_seconds.is_finite() && elapsed_seconds > 0.0 {
        elapsed_seconds.rem_euclid(PERIOD_SECONDS) / PERIOD_SECONDS * TAU
    } else {
        0.0
    };
    let transform = Affine::translate((bounds.x0, bounds.y0))
        .pre_scale_non_uniform(bounds.width(), bounds.height());
    let accent = theme.tokens.colors.intent.inverted;
    let light = if theme.tokens.colors.lifts_bright() {
        theme.text_primary
    } else {
        base
    };
    let shaded = mix8(base, accent, 0.18);
    let middle = mix8(base, accent, 0.48);
    let lit = mix8(base, accent, 0.68);
    let highlight = mix8(lit, light, 0.34);
    let silk = Gradient::new_linear((-0.08, 1.05), (0.74, -0.10)).with_stops(
        &[
            (0.0, shaded),
            (0.28, middle),
            (0.48, lit),
            (0.55, highlight),
            (0.66, middle),
            (1.0, mix8(base, accent, 0.10)),
        ][..],
    );
    let rim = Gradient::new_linear((-0.08, 1.05), (0.74, -0.10)).with_stops(
        &[
            (0.0, light.with_alpha(0.0)),
            (0.44, light.with_alpha(0.24)),
            (0.56, light.with_alpha(0.66)),
            (0.80, light.with_alpha(0.10)),
            (1.0, light.with_alpha(0.0)),
        ][..],
    );

    // The sculpture extends past the canvas. Clip its geometry so embedded
    // previews cannot paint neighboring content. Four sheets and eight fine
    // contours keep the work fixed, independent of resolution and uptime.
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &bounds,
    );
    let hairline = f64::from(theme.ui_scale) / bounds.height().max(1.0);
    for layer in 0..4 {
        let layer = f64::from(layer);
        let (sheet, edge) = ribbon(phase, layer, 0.0);
        scene.fill(Fill::NonZero, transform, &silk, None, &sheet);
        scene.stroke(&Stroke::new(hairline), transform, &rim, None, &edge);

        // Delicate parallel contours suggest tension along the material,
        // instead of introducing grain or a costly blurred glow.
        for offset in [0.008, 0.017] {
            let (_, contour) = ribbon(phase, layer, offset);
            scene.stroke(
                &Stroke::new(hairline * 0.45),
                transform,
                &rim,
                None,
                &contour,
            );
        }
    }

    draw_readability_veils(scene, transform, base);
    scene.pop_layer();
}

/// Quiet both text columns without altering the animated sculpture.
fn draw_readability_veils(scene: &mut Scene, transform: Affine, base: Color) {
    // Keep the welcome copy and clock on quiet ground even when the moving
    // fold crosses their column. The fade leaves the central sculpture lit,
    // while the lower-left sheets remain visible as a subdued continuation.
    let intro_veil = Gradient::new_linear((0.42, 0.0), (0.66, 0.0)).with_stops(
        &[
            (0.0, base.with_alpha(0.90)),
            (0.35, base.with_alpha(0.70)),
            (0.70, base.with_alpha(0.20)),
            (1.0, base.with_alpha(0.0)),
        ][..],
    );
    scene.fill(
        Fill::NonZero,
        transform,
        &intro_veil,
        None,
        &Rect::new(0.0, 0.0, 1.0, 1.0),
    );

    // A theme-colored veil quiets the credential side without putting a
    // bright halo behind text or requiring an opaque full-height side panel.
    let veil = Gradient::new_linear((0.42, 0.0), (0.84, 0.0)).with_stops(
        &[
            (0.0, base.with_alpha(0.0)),
            (0.62, base.with_alpha(0.38)),
            (1.0, base),
        ][..],
    );
    scene.fill(
        Fill::NonZero,
        transform,
        &veil,
        None,
        &Rect::new(0.0, 0.0, 1.0, 1.0),
    );
}

/// A normalized sheet and its leading edge. The two periodic displacements
/// change the curvature, rather than translating a wallpaper across the UI.
fn ribbon(phase: f64, layer: f64, inset: f64) -> (BezPath, BezPath) {
    let sway = (phase + layer * 0.24).sin() * 0.018;
    let breath = (phase * 2.0 + layer * 0.18).sin() * 0.012;
    let p = |x: f64, y: f64| Point::new(x - layer * 0.055 + inset, y + layer * 0.016);
    let mut edge = BezPath::new();
    edge.move_to(p(-0.22, 1.17));
    edge.curve_to(
        p(0.06, 0.72),
        p(0.43 + sway, 0.93),
        p(0.40 + sway, 0.62 + breath),
    );
    edge.curve_to(p(0.38 + sway, 0.46), p(0.66, 0.42 + breath), p(0.62, 0.19));
    edge.curve_to(p(0.60 - sway, 0.05), p(0.65, -0.08), p(0.88, -0.21));
    let mut sheet = edge.clone();
    sheet.line_to(p(1.0, -0.17));
    sheet.curve_to(p(0.78, -0.02), p(0.75 - sway, 0.10), p(0.77, 0.23));
    sheet.curve_to(
        p(0.81, 0.50 + breath),
        p(0.56 + sway, 0.55),
        p(0.57 + sway, 0.73),
    );
    sheet.curve_to(p(0.57, 0.98), p(0.27, 1.17), p(0.11, 1.24));
    sheet.close_path();
    (sheet, edge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::{kurbo::Affine, peniko::Fill};
    const BOUNDS: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);

    fn frame(theme: &Theme, time: f64) -> Scene {
        let mut scene = Scene::new();
        draw(&mut scene, theme, BOUNDS, time);
        scene
    }

    fn assert_same_scene(a: &Scene, b: &Scene) {
        let (a, b) = (a.encoding(), b.encoding());
        assert_eq!(a.path_data, b.path_data);
        assert_eq!(a.draw_data, b.draw_data);
        assert!(a.draw_tags == b.draw_tags);
        assert_eq!(a.resources.color_stops, b.resources.color_stops);
    }

    #[test]
    fn backdrop_animates_only_when_allowed_and_stays_opaque() {
        // Case `ordinary_themes_animate_actual_scene_geometry`.
        {
            for theme in [Theme::dark(), Theme::light()] {
                assert!(needs_animation(&theme));
                let (first, later) = (frame(&theme, 0.0), frame(&theme, 7.0));
                assert_ne!(first.encoding().path_data, later.encoding().path_data);
                assert!(!first.encoding().resources.color_stops.is_empty());
                assert!(first.encoding().n_paths > 1);
            }
        }

        // Case `reduced_motion_keeps_the_entire_composition_static`.
        {
            let theme = Theme::dark().with_reduce_motion(true);
            assert!(!needs_animation(&theme));
            let first = frame(&theme, 0.0);
            assert!(
                first.encoding().n_paths > 1,
                "keep the artwork when motion is disabled"
            );
            assert_same_scene(&first, &frame(&theme, 29.0));
        }

        // Case `high_contrast_draws_only_the_opaque_theme_base`.
        {
            let theme = Theme::high_contrast();
            assert!(!needs_animation(&theme));
            let mut expected = Scene::new();
            expected.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                theme.tokens.colors.surfaces.base.with_alpha(1.0),
                None,
                &BOUNDS,
            );
            assert_same_scene(&frame(&theme, 0.0), &expected);
            assert_same_scene(&frame(&theme, 45.0), &expected);
        }

        // Case `every_frame_starts_with_an_opaque_cover_of_its_bounds`.
        {
            let mut transparent_theme = Theme::dark();
            let ground = &mut transparent_theme.tokens.colors.surfaces.base;
            *ground = ground.with_alpha(0.2);
            for theme in [Theme::dark(), Theme::light(), transparent_theme] {
                let scene = frame(&theme, 7.0);
                let mut cover = Scene::new();
                cover.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    theme.tokens.colors.surfaces.base.with_alpha(1.0),
                    None,
                    &BOUNDS,
                );
                assert!(
                    scene
                        .encoding()
                        .path_data
                        .starts_with(&cover.encoding().path_data)
                );
                assert!(
                    scene
                        .encoding()
                        .draw_data
                        .starts_with(&cover.encoding().draw_data)
                );
            }
        }
    }

    #[test]
    fn backdrop_scenes_stay_seamless_small_and_finite_for_any_input() {
        // Case `looping_animation_has_no_jump_at_the_period_boundary`.
        {
            let theme = Theme::dark();
            assert_same_scene(&frame(&theme, 0.0), &frame(&theme, 120.0));
            assert_same_scene(&frame(&theme, 3.5), &frame(&theme, 123.5));
        }

        // Case `invalid_time_uses_the_initial_composition`.
        {
            let theme = Theme::dark();
            for time in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
                assert_same_scene(&frame(&theme, time), &frame(&theme, 0.0));
            }
        }

        // Case `scenes_stay_small_and_finite_even_after_long_uptime`.
        {
            for time in [0.0, 7.0, 120.0, 10_000_000.0, f64::MAX] {
                let scene = frame(&Theme::dark(), time);
                let encoded = scene.encoding();
                assert!(encoded.n_paths > 1);
                assert!(encoded.n_paths < 64);
                assert!(encoded.path_data.len() < 4096);
                assert!(
                    encoded
                        .path_data
                        .iter()
                        .all(|bits| f32::from_bits(*bits).is_finite())
                );
                assert_eq!(encoded.n_open_clips, 0);
            }
        }

        // Case `invalid_bounds_do_not_poison_the_scene`.
        {
            for bounds in [
                Rect::ZERO,
                Rect::new(10.0, 0.0, 0.0, 10.0),
                Rect::new(0.0, 0.0, f64::NAN, 900.0),
            ] {
                let mut scene = Scene::new();
                draw(&mut scene, &Theme::dark(), bounds, 0.0);
                assert!(scene.encoding().is_empty());
            }
        }
    }
}
