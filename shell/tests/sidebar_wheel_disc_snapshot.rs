//! Screenshot harness for the `SidebarWheelDisc` widget.
//!
//! Renders a 3-spoke disc (one spoke per workspace) onto a canvas shaped
//! like the live sidebar slot and writes it to
//! `target/nirios-screenshots/wheel-disc-01-baseline.png`, pinned against
//! a committed golden.
//!
//! Two tests share one layout + draw helper. The golden one catches any
//! visual regression by eye at review time; the high-contrast one reads
//! pixels back, because "the wheel went invisible under this palette" is
//! not something a dark-theme golden can see.
//!
//! Soft-skips when no wgpu adapter is available (same pattern as
//! `e2e_shell_flows.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use shell::shell::sidebar_wheel_disc::{SidebarWheelDisc, SpokeLabel};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

mod support;

use support::offscreen::OffscreenRenderer;
use support::snapshot::SnapshotAssertion;

/// Directory under the crate root where golden PNGs are committed.
fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("wheel_disc")
}

/// Canvas proportions matter as much as the size here. The disc derives
/// its radius from the slot's *width* and then shows whatever slice of
/// that circle the slot's *height* reaches, so the height-to-width ratio
/// decides how many degrees of rim — and therefore how many workspaces —
/// land on screen. The live shell's sidebar slot is roughly 234 × 690,
/// a ratio near 2.9; at 400 × 700 with a 352 px disc the harness was
/// closer to 1.8, which put every neighbour out at the very edge of the
/// vertical fade and left the golden unable to show the rail it exists to
/// pin. These numbers keep the ratio and the frame compact.
const CANVAS_W: u32 = 290;
const CANVAS_H: u32 = 700;

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

/// Build and paint the three-workspace disc under `theme`, laid out the
/// way the live shell mounts it: pinned to the slot's left edge, with the
/// disc's centre off-canvas so only the right arc protrudes.
///
/// Shared by the golden test and the high-contrast one so both exercise
/// the same production layout + draw path and can only disagree about the
/// palette.
fn render_three_spoke_disc(theme: &Theme, tr: &mut TextRenderer) -> Scene {
    let spokes = vec![
        SpokeLabel {
            text: "Home".into(),
            workspace_id: Some(1),
        },
        SpokeLabel {
            text: "Research".into(),
            workspace_id: Some(2),
        },
        SpokeLabel {
            text: "Design".into(),
            workspace_id: Some(3),
        },
    ];
    // Size the disc so its visible sliver fits this canvas, the way the
    // live shell does — the overlay host overrides the wheel's slot to
    // a viewport-derived width rather than taking the widget's default.
    // Without this the sliver is wider than the canvas and the rim, the
    // markers and the label all land off the right edge, leaving the
    // snapshot pinned on empty background.
    //
    // `visible_width_px` is linear in the diameter, so one ratio solves
    // it exactly without the test needing the geometry constant. The
    // 48 px of slack keeps the active marker's halo — which is centred
    // on the rim, at the sliver's right edge — inside the frame.
    let mut disc = SidebarWheelDisc::new(spokes);
    let target_visible = CANVAS_W as f64 - 48.0;
    let scaled_diameter = disc.diameter_px * (target_visible / disc.visible_width_px());
    disc = disc.with_diameter(scaled_diameter);
    // Keep the default 4500 px diameter (post 2026-05-23 redesign). The
    // disc mounts at the canvas's LEFT edge so the partial-arc design
    // reads correctly: only the right ~66° of the rim protrudes into the
    // visible canvas, the rest sits off-screen-left. `align_items:
    // Center` keeps the sliver vertically centred.

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let child = disc.build_layout(&mut taffy, theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(CANVAS_W as f32),
                    height: Dimension::length(CANVAS_H as f32),
                },
                // Pin the disc to the left edge — the live shell mounts
                // it at `left: 0` of the absolute-positioned sidebar
                // slot, so we mirror that here. The disc's logical
                // centre then sits OFF-CANVAS to the left and only the
                // visible-arc sliver paints inside the canvas.
                justify_content: Some(taffy::JustifyContent::Start),
                align_items: Some(taffy::AlignItems::Center),
                ..Default::default()
            },
            &[child],
        )
        .unwrap();

    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(CANVAS_W as f32),
                height: AvailableSpace::Definite(CANVAS_H as f32),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    disc.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    disc.draw(&mut scene, Affine::IDENTITY, tr, theme, &presence);
    scene
}

#[test]
fn sidebar_wheel_disc_renders_a_spoke_per_workspace() {
    let Some(mut renderer) = try_new_renderer(CANVAS_W, CANVAS_H) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };
    let mut tr = TextRenderer::new();
    let scene = render_three_spoke_disc(&Theme::dark(), &mut tr);

    let path = screenshots_dir().join("wheel-disc-01-baseline.png");
    let bg = Color::from_rgb8(0x02, 0x06, 0x14);
    renderer.render_png(&scene, bg, &path);
    assert!(
        path.exists(),
        "snapshot PNG should be written to {:?}",
        path
    );

    // Pin against the committed golden. Guarded on `path.exists()` to
    // match the soft-skip-on-missing-GPU convention: if the renderer
    // couldn't produce the PNG we've already returned above, but belt-
    // and-braces here too.
    if path.exists() {
        SnapshotAssertion::new(path, goldens_dir().join("three-spokes.png")).assert();
    }
}


/// Every theme repaints this widget, and it used to take its rim and lip
/// from hardcoded white. That happened to look right on the dark desktop
/// and nowhere else — and the failure it invites is the one this codebase
/// keeps hitting: a surface that assumes its own background and disappears
/// when the palette changes underneath it.
///
/// High contrast is the sharp case. It grounds everything on pure black,
/// including `surfaces.overlay`, which is the disc body's own hue — so the
/// body genuinely is black on black there, and the rim is the only thing
/// left saying the wheel exists. Reading the pixels back is the only way
/// to hold that: a golden rendered in the dark theme cannot see it.
#[test]
fn sidebar_wheel_disc_keeps_a_visible_rim_under_high_contrast() {
    let Some(mut renderer) = try_new_renderer(CANVAS_W, CANVAS_H) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };
    let mut tr = TextRenderer::new();
    let scene = render_three_spoke_disc(&Theme::high_contrast(), &mut tr);
    let rgba = renderer.render(&scene, Color::BLACK);

    // A row a quarter of the way down: high enough to miss the active
    // marker's halo and the widest chip, so the only things that can paint
    // in the band to the right of the rail are the body and the rim.
    let y = CANVAS_H / 4;
    let band_start = CANVAS_W - 80;
    let brightest = (band_start..CANVAS_W)
        .map(|x| {
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, CANVAS_W, x, y);
            r.max(g).max(b)
        })
        .max()
        .unwrap_or(0);

    assert!(
        brightest >= 90,
        "the wheel has no visible edge under high contrast — the brightest pixel \
         across x={band_start}..{CANVAS_W} at y={y} was {brightest}/255, which is a \
         black disc on a black ground"
    );
}
