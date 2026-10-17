//! Offscreen render proof for the hover tooltip primitive: paints a small
//! icon-button toolbar and the revealed tooltip chip over the first button,
//! in both light and dark themes, and asserts the chip actually rasterises an
//! opaque surface where it is anchored (not just that *something* painted).
//!
//! Skips gracefully when no wgpu adapter is available, matching the other
//! offscreen tests. Set `TILO_UI_SCREENSHOT_DIR=<path>` to dump the PNGs
//! (`tooltip_over_icon_button_dark.png` / `_light.png`) for visual review.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use tilo_ui::icons::Icon;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::tooltip;
use tilo_ui::widget::{Button, Widget};

fn render_case(theme: &Theme, bg: Color, name: &str) {
    let (canvas_w, canvas_h) = (360u32, 150u32);
    let Some(mut renderer) = OffscreenRenderer::try_new(canvas_w, canvas_h).ok() else {
        eprintln!("no wgpu adapter — skipping tooltip render ({name})");
        return;
    };
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();

    // A three-button icon toolbar near the top of the canvas.
    let rects = [
        Rect::new(24.0, 24.0, 64.0, 60.0),
        Rect::new(72.0, 24.0, 112.0, 60.0),
        Rect::new(120.0, 24.0, 160.0, 60.0),
    ];
    let mut buttons = [
        Button::new("search", "")
            .with_icon(Icon::Search)
            .role(tilo_ui::widget::ButtonRole::Plain)
            .tooltip("Search"),
        Button::new("add", "")
            .with_icon(Icon::Plus)
            .role(tilo_ui::widget::ButtonRole::Plain)
            .tooltip("New folder"),
        Button::new("more", "")
            .with_icon(Icon::MoreHorizontal)
            .role(tilo_ui::widget::ButtonRole::Plain)
            .tooltip("View options"),
    ];
    for (b, r) in buttons.iter_mut().zip(rects.iter()) {
        b.apply_layout_external(*r);
        b.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }

    // Reveal the tooltip over the first (Search) button via the shared
    // primitive — the same call the App's central TooltipHost makes.
    let viewport = Rect::new(0.0, 0.0, f64::from(canvas_w), f64::from(canvas_h));
    tooltip::draw_hover_tooltip(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        rects[0],
        "Search",
        viewport,
    );

    let rgba = renderer.render(&scene, bg);

    // The chip is anchored just below the first button — sample a pixel there
    // and assert it's opaque and differs from the background.
    let (bx, by) = (44u32, 78u32);
    let (r, g, b, a) = OffscreenRenderer::sample(&rgba, canvas_w, bx, by);
    let bg8 = bg.to_rgba8();
    assert_eq!(a, 0xff, "tooltip chip is opaque ({name})");
    assert!(
        [r, g, b] != [bg8.r, bg8.g, bg8.b],
        "tooltip chip surface differs from the background under the anchor ({name}): got {:?}",
        (r, g, b)
    );

    // And the frame as a whole has substantial painted content (icons + chip).
    let bg_rgba = [bg8.r, bg8.g, bg8.b, bg8.a];
    let non_bg = rgba.chunks_exact(4).filter(|px| px[..4] != bg_rgba).count();
    assert!(
        non_bg > 500,
        "expected painted content ({name}), got {non_bg} px"
    );

    if let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{dir}/tooltip_over_icon_button_{name}.png");
        tilo_ui::png::write_rgba8(&path, canvas_w, canvas_h, &rgba).unwrap();
        eprintln!("wrote {path}");
    }
}

#[test]
fn tooltip_renders_over_icon_button_in_both_themes() {
    // Case `tooltip_renders_over_icon_button_dark`.
    {
        render_case(&Theme::dark(), Color::from_rgb8(0x0f, 0x17, 0x2a), "dark");
    }

    // Case `tooltip_renders_over_icon_button_light`.
    {
        render_case(&Theme::light(), Color::from_rgb8(0xf5, 0xf6, 0xf8), "light");
    }
}
