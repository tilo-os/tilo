//! Screenshot-driven visual E2E smoke test.
//!
//! Renders a minimal vello scene through the headless offscreen
//! harness (`support::offscreen`), then asserts pixel invariants —
//! the background tint at (0, 0) matches the requested base colour,
//! a filled rect lights up opaque pixels inside its bounds, and
//! untouched regions stay at the base colour.
//!
//! Skipped automatically (via `eprintln + early return`) if no wgpu
//! adapter is available in the test environment (e.g. sandboxed CI
//! without llvmpipe). We don't `#[ignore]` the whole test because we
//! want CI logs to show whether the headless pipeline was exercised,
//! and we don't panic on missing adapters so local runs without GPU
//! libraries stay green.
//!
//! Intentionally not a golden-image diff test — that requires a design
//! pass for reference-set provenance + per-DPI bases + tolerance. This
//! is the "am I wired up correctly" smoke check the ralph-loop brief
//! asked for ("use screenshotting here to check for issues. Only
//! inspect screenshots where you suspect issues/changed something").

mod support;

use support::offscreen::OffscreenRenderer;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};
use vello::Scene;

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| OffscreenRenderer::new(w, h)))
        .ok()
}

#[test]
fn headless_renderer_paints_a_solid_background_and_a_filled_rect() {
    let mut renderer = match try_new_renderer(128, 64) {
        Some(r) => r,
        None => {
            eprintln!("offscreen: no wgpu adapter available — skipping screenshot test");
            return;
        }
    };

    let mut scene = Scene::new();
    // Filled indigo rect dead-center, 40×20 (from (44, 22) to (84, 42)).
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::rgb8(99, 102, 241),
        None,
        &Rect::new(44.0, 22.0, 84.0, 42.0),
    );

    // Slate-900 background: #0f172a in hex.
    let bg = Color::rgb8(0x0f, 0x17, 0x2a);
    let rgba = renderer.render(&scene, bg);

    // Corner pixel should be the background (outside the filled rect).
    let (r, g, b, a) = OffscreenRenderer::sample(&rgba, 128, 0, 0);
    assert_eq!(
        (r, g, b, a),
        (0x0f, 0x17, 0x2a, 0xff),
        "top-left pixel should be the slate-900 base colour"
    );

    // Dead-center pixel should be opaque indigo — the rect spans (44..84, 22..42),
    // so (64, 32) is solidly inside.
    let (r, g, b, a) = OffscreenRenderer::sample(&rgba, 128, 64, 32);
    assert_eq!(a, 0xff, "center pixel should be opaque");
    assert!(
        r < g.max(b) && b >= 200,
        "center pixel should be indigo-dominant (r={r}, g={g}, b={b})"
    );

    // A pixel just past the rect on the x axis (x = 90, y = 32) is back
    // to background.
    let (r, g, b, _a) = OffscreenRenderer::sample(&rgba, 128, 90, 32);
    assert_eq!(
        (r, g, b),
        (0x0f, 0x17, 0x2a),
        "pixel outside the rect on the right should fall back to the base colour"
    );
}

#[test]
fn headless_renderer_writes_a_png_file() {
    let mut renderer = match try_new_renderer(32, 32) {
        Some(r) => r,
        None => {
            eprintln!("offscreen: no wgpu adapter available — skipping PNG test");
            return;
        }
    };

    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::rgb8(255, 255, 255),
        None,
        &Rect::new(8.0, 8.0, 24.0, 24.0),
    );

    let out = std::env::temp_dir().join("quick-ui-screenshot-test.png");
    // Clear any stale file so the size assertion can't alias a leftover.
    let _ = std::fs::remove_file(&out);
    renderer.render_png(&scene, Color::rgb8(0, 0, 0), &out);

    let meta = std::fs::metadata(&out).expect("PNG should be written");
    assert!(meta.len() > 0, "PNG file must be non-empty");

    // Cheapest possible signature check — PNGs start with 0x89 P N G.
    let bytes = std::fs::read(&out).expect("read PNG");
    assert_eq!(&bytes[..4], b"\x89PNG", "file must start with PNG signature");

    let _ = std::fs::remove_file(&out);
}

#[test]
fn headless_renderer_captures_a_widget_tree() {
    // Drive a real widget through `build_layout` → taffy compute →
    // `apply_layout` → `draw`, then render to an offscreen texture.
    // Proves the offscreen harness can be pointed at the actual
    // widget pipeline — the foundation for visual regression tests.
    use quick_ui::presence::PresenceCtx;
    use quick_ui::text_render::TextRenderer;
    use quick_ui::theme::Theme;
    use quick_ui::widget::{Container, Widget};
    use std::collections::HashMap;
    use taffy::{AvailableSpace, NodeId, Size, TaffyTree};

    let (w, h) = (160, 80);
    let mut renderer = match try_new_renderer(w, h) {
        Some(r) => r,
        None => {
            eprintln!("offscreen: no wgpu adapter available — skipping widget render test");
            return;
        }
    };

    // Widget tree: a single filling Container with a background. The
    // container fills whatever viewport we hand it.
    let mut widget: Box<dyn Widget> = Box::new(Container::new().with_background().filling());

    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root_node = widget.build_layout(&mut taffy, &theme, &mut node_map);

    let mut text_renderer = TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&widget_ptr) = node_map.get(&node_id) {
                    unsafe { (*widget_ptr).measure(known, available, &mut text_renderer) }
                } else {
                    Size::ZERO
                }
            },
        )
        .expect("taffy compute_layout");

    widget.apply_layout(&taffy, root_node, vello::kurbo::Point::ORIGIN);

    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    widget.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
    );

    // Render the scene on top of a red base colour so we can assert
    // that the container actually painted *something* — otherwise the
    // whole buffer would still read red. Any pixel where the red
    // channel dropped is proof the widget rendered over the base.
    let bg = Color::rgb8(0xff, 0x00, 0x00);
    let rgba = renderer.render(&scene, bg);

    // Widget should have painted a background over at least the top-left
    // quadrant; sample a point at (10, 10).
    let (r, _g, _b, a) = OffscreenRenderer::sample(&rgba, w, 10, 10);
    assert_eq!(a, 0xff, "widget render should yield opaque pixels");
    assert_ne!(
        r, 0xff,
        "container background should have overpainted the red base colour"
    );
}
