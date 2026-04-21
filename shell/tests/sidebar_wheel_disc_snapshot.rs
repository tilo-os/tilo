//! Screenshot harness for the new `SidebarWheelDisc` widget.
//!
//! Renders a 3-spoke disc onto a 280×280 canvas and writes it to
//! `target/nirios-screenshots/wheel-disc-01-baseline.png` for eyeball
//! verification. No pixel asserts — per the widget's scope (build +
//! test + snapshot, no live-shell wiring yet) the goal is to catch
//! visual regressions by hand during review.
//!
//! Soft-skips when no wgpu adapter is available (same pattern as
//! `e2e_shell_flows.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use quick_desktop::shell::sidebar_wheel_disc::{SidebarWheelDisc, SpokeLabel};
use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;

mod support;

use support::offscreen::OffscreenRenderer;

const CANVAS: u32 = 280;

fn screenshots_dir() -> PathBuf {
    let root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap_or(Path::new("."))
                .join("target")
        });
    root.join("nirios-screenshots")
}

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

#[test]
fn sidebar_wheel_disc_renders_a_spoke_per_strip() {
    let Some(mut renderer) = try_new_renderer(CANVAS, CANVAS) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };

    let strips = vec![
        SpokeLabel { text: "Home".into(), strip_id: Some(1) },
        SpokeLabel { text: "Research".into(), strip_id: Some(2) },
        SpokeLabel { text: "Design".into(), strip_id: Some(3) },
    ];
    let mut disc = SidebarWheelDisc::new(strips);
    // Keep the default 240 px diameter; canvas has 20 px of margin on
    // each side so the rim isn't flush with the edge.

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let child = disc.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(CANVAS as f32),
                    height: Dimension::length(CANVAS as f32),
                },
                justify_content: Some(taffy::JustifyContent::Center),
                align_items: Some(taffy::AlignItems::Center),
                ..Default::default()
            },
            &[child],
        )
        .unwrap();

    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(CANVAS as f32),
                height: AvailableSpace::Definite(CANVAS as f32),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    disc.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    disc.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let path = screenshots_dir().join("wheel-disc-01-baseline.png");
    let bg = Color::rgb8(0x02, 0x06, 0x14);
    renderer.render_png(&scene, bg, &path);
    assert!(path.exists(), "snapshot PNG should be written to {:?}", path);
}
