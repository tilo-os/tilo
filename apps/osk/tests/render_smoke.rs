//! Offscreen render guard for [`OskWidget`]: drives the full
//! `build_layout` → `compute_layout` → `apply_layout` → draw pipeline and asserts
//! the keyboard rasterises substantial non-background content (surface +
//! toolbar + keycaps + labels).
//!
//! Requires a working offscreen adapter: an unavailable renderer is a failed
//! check, because it cannot prove that the production keyboard paints.
//! Set `TILO_UI_SCREENSHOT_DIR=<path>` to dump PNGs for each theme.

#![allow(
    clippy::cast_precision_loss,
    reason = "test viewport dims are small hard-coded literals (a few hundred px), always exactly representable in f32"
)]

use std::collections::HashMap;

use osk::OskWidget;
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

#[test]
fn osk_renders_keys_and_toolbar() {
    let (w, h) = (720u32, 320u32);
    let mut renderer = OffscreenRenderer::new(w, h);

    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let mut osk = OskWidget::new();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();

        let child = osk.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(w as f32, h as f32),
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                root,
                Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
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
        osk.apply_layout(&taffy, child, Point::ORIGIN);

        assert!(
            osk.key_geometry().len() > 30,
            "expected a full key grid, got {}",
            osk.key_geometry().len()
        );
        assert_eq!(osk.toolbar_geometry().len(), 4, "default toolbar present");

        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        osk.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        // Distinct background so painted content (surface, caps, labels) differs.
        let bg = Color::from_rgb8(0xff, 0x00, 0xff);
        let rgba = renderer.render(&scene, bg);
        let bg_rgba = [0xff, 0x00, 0xff, 0xff];
        let non_bg = rgba.chunks_exact(4).filter(|px| px[..4] != bg_rgba).count();
        assert!(
            non_bg > 1000,
            "{name}: expected substantial painted content, got {non_bg} px"
        );

        if let Ok(dir) = ui::env::var("TILO_UI_SCREENSHOT_DIR") {
            let _ = std::fs::create_dir_all(&dir);
            let path = std::path::Path::new(&dir).join(format!("osk-{name}.png"));
            // Reuse the toolkit's PNG writer so the test needs no `png` dep.
            renderer.render_png(&scene, bg, &path);
            eprintln!("wrote {}", path.display());
        }
    }
}

#[test]
fn a_live_swipe_trail_paints_and_cancellation_removes_it_in_every_theme() {
    use ui::State;
    use ui::headless::FrameHarness;
    use winit::event::TouchPhase;

    let (w, h) = (720, 360);
    let mut renderer = OffscreenRenderer::new(w, h);
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let mut driver = FrameHarness::for_root(State::new(), OskWidget::new(), (w, h), theme);
        let keys = driver.root::<OskWidget>().key_geometry();
        let start = keys[0].1.center();
        let end = keys[9].1.center();
        let middle = start.lerp(end, 0.5);
        let x = (0..w)
            .find(|x| (f64::from(*x) - middle.x).abs() <= 0.5)
            .unwrap();
        let y = (0..h)
            .find(|y| (f64::from(*y) - middle.y).abs() <= 0.5)
            .unwrap();
        let pixel = usize::try_from((y * w + x) * 4).unwrap();
        let before = renderer.render(driver.scene(), Color::TRANSPARENT);
        driver.touch(1, TouchPhase::Started, start.x, start.y);
        driver.touch(1, TouchPhase::Moved, middle.x, middle.y);
        driver.touch(1, TouchPhase::Moved, end.x, end.y);
        let during = renderer.render(driver.scene(), Color::TRANSPARENT);
        assert_ne!(
            &before[pixel..pixel + 4],
            &during[pixel..pixel + 4],
            "{name}: live trail must be visible"
        );
        driver.touch(1, TouchPhase::Cancelled, end.x, end.y);
        let after = renderer.render(driver.scene(), Color::TRANSPARENT);
        assert_eq!(
            &before[pixel..pixel + 4],
            &after[pixel..pixel + 4],
            "{name}: cancellation must remove the trail"
        );
    }
}
