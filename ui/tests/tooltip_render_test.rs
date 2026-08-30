//! Offscreen render proof for the hover tooltip primitive: paints a small
//! icon-button toolbar and the revealed tooltip chip over the first button,
//! in both light and dark themes, and asserts the chip actually rasterises an
//! opaque surface where it is anchored (not just that *something* painted).
//!
//! Skips gracefully when no wgpu adapter is available, matching the other
//! offscreen tests. Set `QUICK_UI_SCREENSHOT_DIR=<path>` to dump the PNGs
//! (`tooltip_over_icon_button_dark.png` / `_light.png`) for visual review.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::tooltip;
use ui::widget::{Button, Widget};

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

fn render_case(theme: &Theme, bg: Color, name: &str) {
    let (w, h) = (360u32, 150u32);
    let Some(mut renderer) = try_new_renderer(w, h) else {
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
        Button::new("search", "", None)
            .with_icon(Icon::Search)
            .ghost()
            .tooltip("Search"),
        Button::new("add", "", None)
            .with_icon(Icon::Plus)
            .ghost()
            .tooltip("New folder"),
        Button::new("more", "", None)
            .with_icon(Icon::MoreHorizontal)
            .ghost()
            .tooltip("View options"),
    ];
    for (b, r) in buttons.iter_mut().zip(rects.iter()) {
        b.apply_layout_external(*r);
        b.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }

    // Reveal the tooltip over the first (Search) button via the shared
    // primitive — the same call the App's central TooltipHost makes.
    let viewport = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
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
    let (r, g, b, a) = OffscreenRenderer::sample(&rgba, w, bx, by);
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

    if let Ok(dir) = std::env::var("QUICK_UI_SCREENSHOT_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{dir}/tooltip_over_icon_button_{name}.png");
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&rgba).unwrap();
        }
        std::fs::write(&path, png).unwrap();
        eprintln!("wrote {path}");
    }
}

#[test]
fn tooltip_renders_over_icon_button_dark() {
    render_case(&Theme::dark(), Color::from_rgb8(0x0f, 0x17, 0x2a), "dark");
}

#[test]
fn tooltip_renders_over_icon_button_light() {
    render_case(&Theme::light(), Color::from_rgb8(0xf5, 0xf6, 0xf8), "light");
}
