//! Pixel-level test for `tilo_ui::presence::draw_remote_cursors`. Renders a
//! single remote cursor at a known position into an offscreen surface,
//! then samples pixels around the apex to assert that *something*
//! non-background was painted there — the lightest possible proof the
//! helper emits geometry (data-side population and wire integration are
//! covered by `presence_remote_cursors_test` and `cross_process_presence_pump`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use vello::Scene;
use vello::peniko::Color;

use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::{PresenceCtx, draw_remote_cursors};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "apex_x/apex_y are a fixed non-negative test pixel coordinate (30.0), always \
exactly representable as u32"
)]
fn ghost_cursor_paints_only_remote_presence() {
    // Case `ghost_cursor_rendered_at_remote_position`.
    {
        // 128x96 is just enough to fit the caret + name pill without
        // clipping; small enough to stay sub-millisecond on llvmpipe.
        let width: u32 = 128;
        let height: u32 = 96;

        let mut presence = PresenceCtx::empty();
        // Position the apex well inside the surface so the triangle
        // (10 px wide) doesn't bleed off the right edge.
        let apex_x: f32 = 30.0;
        let apex_y: f32 = 30.0;
        presence.upsert_remote_cursor("alice".into(), [apex_x, apex_y], None);

        let mut scene = Scene::new();
        let mut text_renderer = TextRenderer::new();
        let theme = Theme::dark();

        draw_remote_cursors(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut text_renderer,
            &theme,
            &presence,
        );

        let mut offscreen = OffscreenRenderer::new(width, height);
        // BLACK background so any non-zero RGB stamp is unambiguous —
        // the caret fill is alice's pink (~ec4899), which differs in
        // every channel from black.
        let rgba = offscreen.render(&scene, Color::BLACK);

        // The triangle is ~10 px tall, so sweep (apex.x, apex.x+8) ×
        // (apex.y, apex.y+8) for a pixel diverging from BLACK.
        let apex_xi = apex_x as u32;
        let apex_y_rounded = apex_y as u32;
        let mut hits = 0;
        let mut first_hit = None;
        for dy in 0..9u32 {
            for dx in 0..9u32 {
                let px = apex_xi + dx;
                let py = apex_y_rounded + dy;
                if px >= width || py >= height {
                    continue;
                }
                let (r, g, b, a) = OffscreenRenderer::sample(&rgba, width, px, py);
                // Threshold 16 catches weak AA-edge hits; `as u32` widens
                // before summing so a bright (255,255,255) doesn't overflow u8 add.
                if u32::from(r) + u32::from(g) + u32::from(b) > 16 && a > 16 {
                    hits += 1;
                    if first_hit.is_none() {
                        first_hit = Some((px, py, r, g, b, a));
                    }
                }
            }
        }
        assert!(
            hits > 0,
            "draw_remote_cursors must paint something inside the cursor stamp; sampled {} pixels around the apex with no hits",
            9 * 9,
        );
        let (hx, hy, r, g, b, _a) = first_hit.unwrap();
        // Alice's palette is #ec4899 (mostly R+B); assert loosely "warm-tinted"
        // so AA borders don't trip the test, while still ruling out pure white.
        assert!(
            r > g,
            "ghost cursor at ({hx},{hy}) should reflect alice's warm palette colour; got ({r},{g},{b})"
        );
    }

    // Case `ghost_cursor_with_empty_presence_paints_nothing`.
    {
        // An empty `remote_cursors` vec must short-circuit cleanly, or an
        // idle peer-list would still drag through TextRenderer / theme
        // lookups every frame.
        let presence = PresenceCtx::empty();
        let mut scene = Scene::new();
        let mut text_renderer = TextRenderer::new();
        let theme = Theme::dark();

        draw_remote_cursors(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut text_renderer,
            &theme,
            &presence,
        );

        let mut offscreen = OffscreenRenderer::new(32, 32);
        let rgba = offscreen.render(&scene, Color::BLACK);
        // Four corners + center as a cheap proxy for "the scene was empty".
        for (x, y) in [(0, 0), (31, 0), (0, 31), (31, 31), (16, 16)] {
            let (r, g, b, _a) = OffscreenRenderer::sample(&rgba, 32, x, y);
            assert_eq!(
                (r, g, b),
                (0, 0, 0),
                "empty presence_ctx must produce a pure-black frame; pixel ({x},{y}) was ({r},{g},{b})",
            );
        }
    }
}
