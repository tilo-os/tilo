//! Screenshot harness for the `SidebarWheelDisc` widget: renders a 3-spoke
//! disc onto a canvas shaped like the live sidebar slot, pinned against a
//! committed golden. The golden catches visual regressions by eye; the
//! high-contrast test reads pixels back, since a dark-theme golden can't
//! see "the wheel went invisible under this palette". Skips when no wgpu
//! adapter is available, unless `TILO_REQUIRE_GPU=1` makes that a failure.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tilo_test_env::require_capability;

use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use tilo_desktop::sidebar_wheel_disc::{SidebarWheelDisc, SpokeLabel};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::support::snapshot::{SnapshotAssertion, pinned_golden_path};
use ui::offscreen::OffscreenRenderer;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("wheel_disc")
}

/// Wide enough to include the 66 px disc sliver and its compact labels, while
/// preserving the live shell's tall left-edge composition.
const CANVAS_W: u32 = 290;
const CANVAS_H: u32 = 700;
// `as f32` on a compile-time canvas-size literal: 290/700 are exactly
// representable, well under f32's 24-bit mantissa headroom.
#[allow(
    clippy::cast_precision_loss,
    reason = "CANVAS_W/CANVAS_H are small compile-time constants, exactly representable in f32"
)]
const CANVAS_W_F32: f32 = CANVAS_W as f32;
#[allow(
    clippy::cast_precision_loss,
    reason = "CANVAS_W/CANVAS_H are small compile-time constants, exactly representable in f32"
)]
const CANVAS_H_F32: f32 = CANVAS_H as f32;

/// Round a canvas-relative coordinate to the nearest non-negative pixel
/// index. Test-only pixel math: every caller works within the small
/// (290x700) canvas, never large enough to lose precision or need more
/// than `.max(0.0)` to land in range.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to >= 0.0 immediately above; canvas coordinates never approach u32::MAX"
)]
fn px(v: f64) -> u32 {
    v.round().max(0.0) as u32
}

/// Perceptual luma (BT.601) of an RGB triple. `r`/`g`/`b` are already `u8`,
/// so the weighted sum can't leave `0.0..=255.0` — the cast just narrows
/// the float sum back to an integer.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "0.299r + 0.587g + 0.114b with r,g,b in 0..=255 always lands in 0.0..=255.0"
)]
fn luma_u32(r: u8, g: u8, b: u8) -> u32 {
    (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) as u32
}

fn screenshots_dir() -> PathBuf {
    let root = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(3)
                .unwrap_or_else(|| Path::new("."))
                .join("target")
        },
        PathBuf::from,
    );
    root.join("tilo-screenshots")
}

/// Paint the three-workspace disc under `theme`, mounted as the live shell
/// mounts it (pinned to the slot's left edge, centre off-canvas). Shared
/// by the golden and high-contrast tests so they can only disagree about
/// the palette.
fn render_three_spoke_disc(theme: &Theme, tr: &mut TextRenderer) -> (Scene, SidebarWheelDisc) {
    render_disc_at(theme, tr, 0.0)
}

fn render_disc_at(
    theme: &Theme,
    tr: &mut TextRenderer,
    rotation_deg: f32,
) -> (Scene, SidebarWheelDisc) {
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
    let mut disc = SidebarWheelDisc::new(spokes);
    // Keep production geometry: a fixed 4400 px diameter and only 66 px of
    // protrusion. Rescaling this to the canvas width would turn the regression
    // test itself back into the rejected broad rail.

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let child = disc.build_layout(&mut taffy, theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(CANVAS_W_F32, CANVAS_H_F32),
                // Mirrors `left: 0` on the live shell's absolute-positioned sidebar slot:
                // the logical centre sits off-canvas, only the visible arc paints.
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
                width: AvailableSpace::Definite(CANVAS_W_F32),
                height: AvailableSpace::Definite(CANVAS_H_F32),
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

    disc.rotation_deg = rotation_deg;

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    disc.draw(&mut scene, Affine::IDENTITY, tr, theme, &presence);
    (scene, disc)
}

#[test]
fn sidebar_wheel_disc_snapshots_render_spokes_and_a_bounded_summoned_band() {
    // Phase `sidebar_wheel_disc_renders_a_spoke_per_workspace`.
    {
        let mut renderer = require_capability!(
            Gpu,
            OffscreenRenderer::try_new(CANVAS_W, CANVAS_H).ok(),
            "no wgpu adapter available for offscreen render"
        );
        let mut tr = TextRenderer::new();
        let (scene, _disc) = render_three_spoke_disc(&Theme::dark(), &mut tr);

        let path = screenshots_dir().join("wheel-disc-01-baseline.png");
        let bg = Color::from_rgb8(0x02, 0x06, 0x14);
        renderer.render_png(&scene, bg, &path);
        assert!(path.exists(), "snapshot PNG should be written to {path:?}");

        // Guarded on `path.exists()` to match the soft-skip-on-missing-GPU
        // convention.
        if path.exists() {
            SnapshotAssertion::new(
                path,
                pinned_golden_path(goldens_dir().join("three-spokes.png")),
            )
            .assert();
        }
    }

    // Phase `the_summoned_band_is_a_surface_over_a_bright_app_and_still_ends`.
    // Summoning lifts the disc onto the strip under the pointer — usually
    // over a bright app — so the band must be a real surface there, or
    // near-white labels land on a near-white document. It must also END: a
    // wheel blacking out the screen would pass "is it a surface" too. The
    // body is solid to the centre, so the "still a slice" probe sits off the
    // left edge; sample points come from the band the widget actually paints
    // so tuning its depth moves the probes with it.
    {
        let mut renderer = require_capability!(
            Gpu,
            OffscreenRenderer::try_new(CANVAS_W, CANVAS_H).ok(),
            "no wgpu adapter available for offscreen render"
        );
        let mut tr = TextRenderer::new();
        let (scene, disc) = render_three_spoke_disc(&Theme::dark(), &mut tr);
        let rgba = renderer.render(&scene, Color::WHITE);

        let path = screenshots_dir().join("wheel-disc-02-over-light.png");
        renderer.render_png(&scene, Color::WHITE, &path);

        let luma = |x: u32, y: u32| {
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, CANVAS_W, x, y);
            luma_u32(r, g, b)
        };

        let (r_in, r_out) = disc.label_band_radii();
        let c = disc.disc_centre();
        let footprint = disc.get_rect();
        // The band's vertical middle, where it is at full alpha and widest.
        let row_y = px(c.y);
        let on_band = px(c.x + f64::midpoint(r_in, r_out)).min(CANVAS_W - 1);
        // Well inside the inner radius, where a hollow body would show through.
        let inside_arc = px(c.x + r_in - 24.0).min(CANVAS_W - 1);
        assert!(
            f64::from(inside_arc) > footprint.x0 + 2.0,
            "probe at x={inside_arc} is outside the disc's own footprint {footprint:?} \
             — it would be testing open wallpaper"
        );

        let band = luma(on_band, row_y);
        assert!(
            band < 110,
            "the band let the bright app through (luma {band}/255 at ({on_band},{row_y})) \
             — the labels painted on it have nothing to read against; see {path:?}"
        );

        // The body reaches the centre, so this point is surface too, and as opaque.
        let inner = luma(inside_arc, row_y);
        assert!(
            inner < 110,
            "the disc is hollow again — the bright app shows through at luma {inner}/255 \
             at ({inside_arc},{row_y}), inside the footprint. The body is meant to be a \
             solid segment, filled to the centre; see {path:?}"
        );

        // ...and it still ends. Mounted flush left as in the sidebar column, so
        // past the rim — clear of its outward shadow — the app must be plainly
        // visible. Labels hang past the rim on their own plates, so the probe
        // takes the first row below the centre that none of the disc's
        // published targets covers.
        let outside = px(c.x + r_out + RIM_SHADOW_CLEARANCE).min(CANVAS_W - 1);
        assert!(
            f64::from(outside) > footprint.x1 - 1.0 || f64::from(outside) > c.x + r_out,
            "probe at x={outside} is not past the rim at {:.0}",
            c.x + r_out
        );
        let targets: Vec<_> = disc
            .accessibility()
            .map(|node| node.children)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|node| node.bounds)
            .collect();
        assert!(
            !targets.is_empty(),
            "the disc published no targets, so the probe row would be a guess"
        );
        let clear_y = (row_y..CANVAS_H)
            .find(|&y| {
                let y = f64::from(y);
                targets
                    .iter()
                    .all(|b| y < b.y0 - LABEL_PLATE_CLEARANCE || y > b.y1 + LABEL_PLATE_CLEARANCE)
            })
            .expect("a row past the rim that no label covers");
        let beyond = luma(outside, clear_y);
        assert!(
            beyond > 170,
            "the disc blacked out the app past its own rim (luma {beyond}/255 at \
             ({outside},{clear_y})) — it is meant to be a slice, not a panel; see {path:?}"
        );
    }
}

/// Pixels past the rim at which the app must be fully visible again,
/// clear of the rim's outward shadow.
const RIM_SHADOW_CLEARANCE: f64 = 36.0;

/// Rows above and below a label's target the probe keeps clear of, for the
/// plate's anti-aliased edge.
const LABEL_PLATE_CLEARANCE: f64 = 4.0;
