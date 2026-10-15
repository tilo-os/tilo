//! Fractional-scale render invariants.
//!
//! The live `App` renders `HiDPI` by keeping layout in *logical* space and
//! compositing the whole scene through `Affine::scale(scale_factor)` into
//! a *physical*-pixel surface (see `crates/sdk/ui-core/src/app.rs`). This test pins that
//! contract deterministically for a **fractional** 1.5× factor: a shape
//! placed at logical coordinates must land at `logical × 1.5` physical
//! pixels, with a hard vector edge — not the blurry integer-rounded
//! placement a 1× or 2× compositor would produce.
//!
//! Skipped (eprintln + early return) when no wgpu adapter is available,
//! matching the sibling `screenshot_test.rs`; we never `#[ignore]`.

use tilo_ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

/// A 40×40 logical square, top-left (10, 10).
const LOGICAL_L: f64 = 10.0;
const LOGICAL_T: f64 = 10.0;
const LOGICAL_R: f64 = 50.0;
const LOGICAL_B: f64 = 50.0;

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "phys/mid_y are derived from a small positive logical size (≤ 105) times a bounded \
scale factor (1.25/1.75), always positive and well within u32's range"
)]
fn fractional_scales_map_logical_edges_to_proportional_physical_edges() {
    // Case `fractional_scale_maps_logical_to_physical_with_a_hard_edge`.
    {
        fn fractional_scale_maps_logical_to_physical_with_a_hard_edge() {
            let scale = 1.5_f64;
            // Logical canvas is 60×60; physical surface is logical × scale = 90×90.
            let phys = 90u32;
            let Some(mut renderer) = OffscreenRenderer::try_new(phys, phys).ok() else {
                eprintln!("offscreen: no wgpu adapter — skipping fractional-scale render test");
                return;
            };

            let mut scene = Scene::new();
            // Draw the square at LOGICAL coordinates but composite through the
            // fractional scale transform — exactly what the live App does.
            let fill = Color::from_rgb8(99, 102, 241);
            scene.fill(
                Fill::NonZero,
                Affine::scale(scale),
                fill,
                None,
                &Rect::new(LOGICAL_L, LOGICAL_T, LOGICAL_R, LOGICAL_B),
            );

            let bg = Color::from_rgb8(0x0f, 0x17, 0x2a);
            let rgba = renderer.render(&scene, bg);

            let is_fill = |x: u32, y: u32| -> bool {
                let (r, _g, b, _a) = OffscreenRenderer::sample(&rgba, phys, x, y);
                // Indigo-dominant = inside the square; slate bg is blue-ish but dark.
                b >= 200 && r < 150
            };
            let is_bg = |x: u32, y: u32| -> bool {
                let (r, g, b, _a) = OffscreenRenderer::sample(&rgba, phys, x, y);
                (r, g, b) == (0x0f, 0x17, 0x2a)
            };

            assert!(
                is_fill(45, 45),
                "square center should be filled at physical (45,45) for a 1.5x scale"
            );
            assert!(
                is_bg(2, 2),
                "corner should be background outside the scaled square"
            );

            // Decisive: the right edge sits at logical 50 × 1.5 = physical 75. An
            // integer 1× compositor would put it at x=50 (so x=72 would be bg);
            // an integer 2× at x=100 (off a 90px buffer). Only the fractional
            // factor satisfies both checks.
            assert!(
                is_fill(72, 45),
                "pixel at physical x=72 (logical ~48, inside the square at 1.5x) should be fill"
            );
            assert!(
                is_bg(78, 45),
                "pixel at physical x=78 (logical ~52, past the 1.5x-scaled right edge at 75) should be bg"
            );

            assert!(
                is_fill(45, 72),
                "pixel at physical y=72 (inside at 1.5x) should be fill"
            );
            assert!(
                is_bg(45, 78),
                "pixel at physical y=78 (past the 1.5x-scaled bottom edge) should be bg"
            );
        }
        fractional_scale_maps_logical_to_physical_with_a_hard_edge();
    }

    // Case `different_fractional_factors_place_the_edge_proportionally`.
    {
        fn different_fractional_factors_place_the_edge_proportionally() {
            // Proves the placement scales with the factor, not a fixed rounding.
            for (scale, edge_in, edge_out) in [(1.25_f64, 60u32, 65u32), (1.75_f64, 85u32, 90u32)] {
                let phys = ((60.0 * scale).round() as u32) + 4;
                let Some(mut renderer) = OffscreenRenderer::try_new(phys, phys).ok() else {
                    eprintln!("offscreen: no wgpu adapter — skipping ({scale}x) case");
                    return;
                };

                let mut scene = Scene::new();
                let fill = Color::from_rgb8(99, 102, 241);
                scene.fill(
                    Fill::NonZero,
                    Affine::scale(scale),
                    fill,
                    None,
                    &Rect::new(LOGICAL_L, LOGICAL_T, LOGICAL_R, LOGICAL_B),
                );
                let bg = Color::from_rgb8(0x0f, 0x17, 0x2a);
                let rgba = renderer.render(&scene, bg);

                let mid_y = (30.0 * scale).round() as u32; // logical-30 center row
                let inside = OffscreenRenderer::sample(&rgba, phys, edge_in, mid_y);
                let outside = OffscreenRenderer::sample(&rgba, phys, edge_out, mid_y);
                assert!(
                    inside.2 >= 200 && inside.0 < 150,
                    "at {scale}x, x={edge_in} (logical {:.1}) should be inside the square",
                    f64::from(edge_in) / scale
                );
                assert_eq!(
                    (outside.0, outside.1, outside.2),
                    (0x0f, 0x17, 0x2a),
                    "at {scale}x, x={edge_out} (logical {:.1}) should be past the scaled edge",
                    f64::from(edge_out) / scale
                );
            }
        }
        different_fractional_factors_place_the_edge_proportionally();
    }
}
