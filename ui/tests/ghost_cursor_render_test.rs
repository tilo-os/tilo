//! Pixel-level test for `ui::presence::draw_remote_cursors`. Renders a
//! single remote cursor at a known position into an offscreen surface,
//! then samples pixels around the apex to assert that *something*
//! non-background was painted there. This is the lightest possible
//! proof that the helper actually emits geometry; the existing
//! `presence_remote_cursors_test` covers data-side population and the
//! new `cross_process_presence_pump` covers wire integration.

use vello::Scene;
use vello::peniko::Color;

use ui::offscreen::OffscreenRenderer;
use ui::presence::{draw_remote_cursors, PresenceCtx};
use ui::text_render::TextRenderer;
use ui::theme::Theme;

#[test]
fn ghost_cursor_rendered_at_remote_position() {
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

    // Sample a small grid around the apex looking for SOMETHING the
    // helper painted. The triangle is ~10 px tall, so we sweep
    // (apex.x, apex.x+8) × (apex.y, apex.y+8) and assert at least
    // one of those pixels diverged from the BLACK background.
    let apex_xi = apex_x as u32;
    let apex_yi = apex_y as u32;
    let mut hits = 0;
    let mut first_hit = None;
    for dy in 0..9u32 {
        for dx in 0..9u32 {
            let x = apex_xi + dx;
            let y = apex_yi + dy;
            if x >= width || y >= height {
                continue;
            }
            let (r, g, b, a) = OffscreenRenderer::sample(&rgba, width, x, y);
            // Any pixel diverging from BLACK background counts as a
            // hit. Anti-aliased edges produce weaker hits than the
            // interior; the threshold is conservative so we still
            // catch the AA edge. `as u32` widens before summing so a
            // bright (255,255,255) doesn't overflow u8 add.
            if r as u32 + g as u32 + b as u32 > 16 && a > 16 {
                hits += 1;
                if first_hit.is_none() {
                    first_hit = Some((x, y, r, g, b, a));
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
    // The caret should be pink-ish (alice's palette is #ec4899 → mostly
    // R + B). We only assert "warm-tinted" (R > B > G in the broadcast
    // case) loosely so AA borders don't trip the test, but we DO assert
    // that the helper picked the palette colour and not "pure white".
    assert!(
        r > g,
        "ghost cursor at ({hx},{hy}) should reflect alice's warm palette colour; got ({r},{g},{b})"
    );
}

#[test]
fn ghost_cursor_with_empty_presence_paints_nothing() {
    // Regression guard: an empty `remote_cursors` vec must short-
    // circuit cleanly. Otherwise an idle peer-list would still drag
    // through TextRenderer / theme lookups every frame.
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
    // Every pixel in the surface must be BLACK (or close to it,
    // accounting for AA filter taps in shader). We check the four
    // corners + center as a cheap proxy for "the scene was empty".
    for (x, y) in [(0, 0), (31, 0), (0, 31), (31, 31), (16, 16)] {
        let (r, g, b, _a) = OffscreenRenderer::sample(&rgba, 32, x, y);
        assert_eq!(
            (r, g, b),
            (0, 0, 0),
            "empty presence_ctx must produce a pure-black frame; pixel ({x},{y}) was ({r},{g},{b})",
        );
    }
}
