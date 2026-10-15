//! Screenshot-driven visual E2E smoke test.
//!
//! Renders a minimal vello scene through the headless offscreen harness
//! (`tilo_ui::offscreen`), then asserts pixel invariants — background tint
//! at (0, 0), a filled rect lighting up opaque pixels inside its bounds,
//! untouched regions staying at the base colour.
//!
//! Skipped automatically (via `eprintln` + early return) if no wgpu adapter
//! is available (e.g. sandboxed CI without llvmpipe) — not `#[ignore]`,
//! since CI logs should show whether the headless pipeline ran, and not a
//! panic, so local runs without GPU libraries stay green.
//!
//! Intentionally not a golden-image diff test — that requires a design pass
//! for reference-set provenance + per-DPI bases + tolerance. This is an "am
//! I wired up correctly" smoke check.

use tilo_ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
#[allow(
    clippy::cast_precision_loss,
    reason = "w/h and bar_w/bar_h are small fixed test-canvas dimensions, exactly representable in f32"
)]
fn headless_renderer_paints_writes_and_captures_widgets() {
    // Case `headless_renderer_paints_a_solid_background_and_a_filled_rect`.
    {
        fn headless_renderer_paints_a_solid_background_and_a_filled_rect() {
            let Some(mut renderer) = OffscreenRenderer::try_new(128, 64).ok() else {
                eprintln!("offscreen: no wgpu adapter available — skipping screenshot test");
                return;
            };

            let mut scene = Scene::new();
            // Filled indigo rect dead-center, 40×20 (from (44, 22) to (84, 42)).
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(99, 102, 241),
                None,
                &Rect::new(44.0, 22.0, 84.0, 42.0),
            );

            // Slate-900 background: #0f172a in hex.
            let bg = Color::from_rgb8(0x0f, 0x17, 0x2a);
            let rgba = renderer.render(&scene, bg);

            let (r, g, b, a) = OffscreenRenderer::sample(&rgba, 128, 0, 0);
            assert_eq!(
                (r, g, b, a),
                (0x0f, 0x17, 0x2a, 0xff),
                "top-left pixel should be the slate-900 base colour"
            );

            // The rect spans (44..84, 22..42), so (64, 32) is solidly inside.
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
        headless_renderer_paints_a_solid_background_and_a_filled_rect();
    }

    // Case `headless_renderer_writes_a_png_file`.
    {
        fn headless_renderer_writes_a_png_file() {
            let Some(mut renderer) = OffscreenRenderer::try_new(32, 32).ok() else {
                eprintln!("offscreen: no wgpu adapter available — skipping PNG test");
                return;
            };

            let mut scene = Scene::new();
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(255, 255, 255),
                None,
                &Rect::new(8.0, 8.0, 24.0, 24.0),
            );

            let out = std::env::temp_dir().join("quick-ui-screenshot-test.png");
            // Clear any stale file so the size assertion can't alias a leftover.
            let _ = std::fs::remove_file(&out);
            renderer.render_png(&scene, Color::from_rgb8(0, 0, 0), &out);

            let meta = std::fs::metadata(&out).expect("PNG should be written");
            assert!(meta.len() > 0, "PNG file must be non-empty");

            // Cheapest possible signature check — PNGs start with 0x89 P N G.
            let bytes = std::fs::read(&out).expect("read PNG");
            assert_eq!(
                &bytes[..4],
                b"\x89PNG",
                "file must start with PNG signature"
            );

            let _ = std::fs::remove_file(&out);
        }
        headless_renderer_writes_a_png_file();
    }

    // Case `headless_renderer_captures_a_widget_tree`.
    {
        fn headless_renderer_captures_a_widget_tree() {
            // Drives a real widget through `build_layout` → taffy compute →
            // `apply_layout` → `draw`, proving the offscreen harness can be pointed
            // at the actual widget pipeline.
            use std::collections::HashMap;
            use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
            use tilo_ui::presence::PresenceCtx;
            use tilo_ui::text_render::TextRenderer;
            use tilo_ui::theme::Theme;
            use tilo_ui::widget::{Container, Widget};

            let (w, h) = (160, 80);
            let Some(mut renderer) = OffscreenRenderer::try_new(w, h).ok() else {
                eprintln!("offscreen: no wgpu adapter available — skipping widget render test");
                return;
            };

            let mut widget: Box<dyn Widget> =
                Box::new(Container::new().with_background().filling());

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

            // Red base colour: any pixel where the red channel dropped is proof the
            // widget rendered over it, otherwise the buffer would still read red.
            let bg = Color::from_rgb8(0xff, 0x00, 0x00);
            let rgba = renderer.render(&scene, bg);

            let (r, _g, _b, a) = OffscreenRenderer::sample(&rgba, w, 10, 10);
            assert_eq!(a, 0xff, "widget render should yield opaque pixels");
            assert_ne!(
                r, 0xff,
                "container background should have overpainted the red base colour"
            );
        }
        headless_renderer_captures_a_widget_tree();
    }

    // Case `tab_bar_renders_pills_inside_six_px_insets`.
    // Render the real `TabBar` widget and assert the 44px / 6px-inset-pill
    // layout shows up in the rasterised output.
    //
    // Drawn against a magenta base so every pixel inside the pill shows up as
    // non-magenta. Pixels in the 6px insets above/below stay
    // *colourless-over-magenta* (green = 0, r == b) — the pill body never
    // extends there, though the per-pill drop shadow may darken the base.
    {
        fn tab_bar_renders_pills_inside_six_px_insets() {
            use std::collections::HashMap;
            use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
            use tilo_ui::presence::PresenceCtx;
            use tilo_ui::text_render::TextRenderer;
            use tilo_ui::theme::Theme;
            use tilo_ui::widget::{TabBar, TabDescriptor, Widget};

            // 240 wide gives us room for the first 96 px min-width pill + gap;
            // 44 tall matches TAB_BAR_HEIGHT exactly.
            let (bar_w, bar_h) = (240u32, 44u32);
            let Some(mut renderer) = OffscreenRenderer::try_new(bar_w, bar_h).ok() else {
                eprintln!("offscreen: no wgpu adapter available — skipping tab-bar test");
                return;
            };

            let tabs = vec![TabDescriptor::new(1, "a"), TabDescriptor::new(2, "b")];
            let mut bar: Box<dyn Widget> = Box::new(TabBar::new("grp", tabs, 1));
            // Exercise the visible keyboard focus treatment at a real output scale,
            // under the theme most likely to expose a low-contrast or cramped ring.
            bar.set_focus(true);

            let theme = Theme::high_contrast().with_text_scale(1.5);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let root = bar.build_layout(&mut taffy, &theme, &mut node_map);

            let mut tr = TextRenderer::new();
            taffy
                .compute_layout_with_measure(
                    root,
                    Size {
                        width: AvailableSpace::Definite(bar_w as f32),
                        height: AvailableSpace::Definite(bar_h as f32),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = node_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, &mut tr) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .expect("taffy compute_layout");

            bar.apply_layout(&taffy, root, vello::kurbo::Point::ORIGIN);

            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

            // Magenta base — not in any theme colour, so it's unambiguously "no
            // widget pixels here" when sampled.
            let bg = Color::from_rgb8(0xff, 0x00, 0xff);
            let rgba = renderer.render(&scene, bg);

            // If TILO_UI_SCREENSHOT_DIR is set, write the rasterised bar to
            // `<dir>/tab_bar.png` for visual inspection. Off by default so normal
            // CI runs don't accumulate artifacts.
            if let Ok(dir) = std::env::var("TILO_UI_SCREENSHOT_DIR") {
                let path = std::path::Path::new(&dir).join("tab_bar.png");
                std::fs::create_dir_all(&dir).ok();
                tilo_ui::png::write_rgba8(&path, bar_w, bar_h, &rgba)
                    .expect("write screenshot png");
                eprintln!("wrote tab-bar screenshot to {}", path.display());
            }

            // Top inset (y < 6): pill body does not extend here; a drop shadow may
            // darken the base but cannot introduce green or break r==b.
            let (r, g, b, _a) = OffscreenRenderer::sample(&rgba, bar_w, 20, 2);
            assert_eq!(
                g, 0x00,
                "no pill body fill in top inset (got r={r}, g={g}, b={b})"
            );
            assert_eq!(
                r, b,
                "drop shadow preserves magenta r==b ratio in top inset"
            );

            // Bottom inset (y > 38): same rationale; the shadow's +6px y-offset
            // means more bleed here, but the r==b / g==0 invariant still holds.
            let (r, g, b, _a) = OffscreenRenderer::sample(&rgba, bar_w, 20, 41);
            assert_eq!(
                g, 0x00,
                "no pill body fill in bottom inset (got r={r}, g={g}, b={b})"
            );
            assert_eq!(
                r, b,
                "drop shadow preserves magenta r==b ratio in bottom inset"
            );

            // Pill interior (first pill center ~= (48, 20)): translucent-white fill
            // drops the G channel below 0xff when composited over magenta.
            let (r, g, b, a) = OffscreenRenderer::sample(&rgba, bar_w, 48, 20);
            assert_eq!(a, 0xff, "pill center should be opaque");
            assert!(
                g < 0xff,
                "pill fill should have composited over magenta (r={r}, g={g}, b={b})"
            );
        }
        tab_bar_renders_pills_inside_six_px_insets();
    }
}
