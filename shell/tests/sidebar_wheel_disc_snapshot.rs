//! Screenshot harness for the `SidebarWheelDisc` widget: renders a 3-spoke
//! disc onto a canvas shaped like the live sidebar slot, pinned against a
//! committed golden. The golden catches visual regressions by eye; the
//! high-contrast test reads pixels back, since a dark-theme golden can't
//! see "the wheel went invisible under this palette". Soft-skips when no
//! wgpu adapter is available.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use shell::shell::sidebar_wheel_disc::{SidebarWheelDisc, SpokeLabel};
use ui::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::support::offscreen::OffscreenRenderer;
use crate::support::snapshot::SnapshotAssertion;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("wheel_disc")
}

/// Canvas proportions matter as much as size: the disc takes its radius
/// from the slot's *width* and shows whatever slice the slot's *height*
/// reaches, so the ratio decides how many workspaces land on screen.
/// The live slot is ~234x690 (~2.9); these numbers keep that ratio.
const CANVAS_W: u32 = 290;
const CANVAS_H: u32 = 700;

fn screenshots_dir() -> PathBuf {
    let root = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap_or(Path::new("."))
                .join("target")
        },
        PathBuf::from,
    );
    root.join("nirios-screenshots")
}

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
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
    // Size the disc so its visible sliver fits the canvas, as the live shell
    // does (viewport-derived slot width, not the widget default), or the
    // sliver runs off the right edge and the snapshot pins empty background.
    // `visible_width_px` is linear in the diameter, so one ratio solves it;
    // the 48px of slack keeps the active marker's halo in frame.
    let mut disc = SidebarWheelDisc::new(spokes);
    let target_visible = f64::from(CANVAS_W) - 48.0;
    let scaled_diameter = disc.diameter_px * (target_visible / disc.visible_width_px());
    disc = disc.with_diameter(scaled_diameter);
    // Keep the default 4500px diameter. Mounted at the canvas's LEFT edge,
    // only the right ~66° of rim protrudes; `align_items: Center` keeps the
    // sliver vertically centred.

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

    disc.rotation_deg = rotation_deg;

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    disc.draw(&mut scene, Affine::IDENTITY, tr, theme, &presence);
    (scene, disc)
}

#[test]
fn sidebar_wheel_disc_renders_a_spoke_per_workspace() {
    let Some(mut renderer) = try_new_renderer(CANVAS_W, CANVAS_H) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };
    let mut tr = TextRenderer::new();
    let (scene, _disc) = render_three_spoke_disc(&Theme::dark(), &mut tr);

    let path = screenshots_dir().join("wheel-disc-01-baseline.png");
    let bg = Color::from_rgb8(0x02, 0x06, 0x14);
    renderer.render_png(&scene, bg, &path);
    assert!(path.exists(), "snapshot PNG should be written to {path:?}");

    // Guarded on `path.exists()` to match the soft-skip-on-missing-GPU
    // convention.
    if path.exists() {
        SnapshotAssertion::new(path, goldens_dir().join("three-spokes.png")).assert();
    }
}

/// High contrast is the sharp case: it grounds everything on pure black,
/// including the disc body's own hue, so the body is black on black and
/// the rim is all that says the wheel exists — only reading pixels back
/// can see that.
#[test]
fn sidebar_wheel_disc_keeps_a_visible_rim_under_high_contrast() {
    let Some(mut renderer) = try_new_renderer(CANVAS_W, CANVAS_H) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };
    let mut tr = TextRenderer::new();
    let (scene, _disc) = render_three_spoke_disc(&Theme::high_contrast(), &mut tr);
    let rgba = renderer.render(&scene, Color::BLACK);
    // The pixel probe can only say the rim survived; whether the whole wheel
    // still reads under an all-black palette needs a human.
    renderer.render_png(
        &scene,
        Color::BLACK,
        screenshots_dir().join("wheel-disc-03-high-contrast.png"),
    );

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

/// Summoning lifts the disc onto the strip under the pointer — usually
/// over a bright app — so the band must be a real surface there, or
/// near-white labels land on a near-white document. It must also END: a
/// wheel blacking out the screen would pass "is it a surface" too. The
/// body is solid to the centre, so the "still a slice" probe sits off the
/// left edge; sample points come from the band the widget actually paints
/// so tuning its depth moves the probes with it.
#[test]
fn the_summoned_band_is_a_surface_over_a_bright_app_and_still_ends() {
    let Some(mut renderer) = try_new_renderer(CANVAS_W, CANVAS_H) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };
    let mut tr = TextRenderer::new();
    let (scene, disc) = render_three_spoke_disc(&Theme::dark(), &mut tr);
    let rgba = renderer.render(&scene, Color::WHITE);

    let path = screenshots_dir().join("wheel-disc-02-over-light.png");
    renderer.render_png(&scene, Color::WHITE, &path);

    let luma = |x: u32, y: u32| {
        let (r, g, b, _) = OffscreenRenderer::sample(&rgba, CANVAS_W, x, y);
        (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) as u32
    };

    let (r_in, r_out) = disc.label_band_radii();
    let c = disc.disc_centre();
    let footprint = disc.get_rect();
    // The band's vertical middle, where it is at full alpha and widest.
    let row_y = c.y.round() as u32;
    let on_band = ((c.x + f64::midpoint(r_in, r_out)).round() as u32).min(CANVAS_W - 1);
    // Well inside the inner radius, where a hollow body would show through.
    let inside_arc = ((c.x + r_in - 24.0).round() as u32).min(CANVAS_W - 1);
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
    // visible.
    let outside = ((c.x + r_out + RIM_SHADOW_CLEARANCE).round() as u32).min(CANVAS_W - 1);
    assert!(
        f64::from(outside) > footprint.x1 - 1.0 || f64::from(outside) > c.x + r_out,
        "probe at x={outside} is not past the rim at {:.0}",
        c.x + r_out
    );
    let beyond = luma(outside, row_y);
    assert!(
        beyond > 170,
        "the disc blacked out the app past its own rim (luma {beyond}/255 at \
         ({outside},{row_y})) — it is meant to be a slice, not a panel; see {path:?}"
    );
}

/// Pixels past the rim at which the app must be fully visible again,
/// clear of the rim's outward shadow.
const RIM_SHADOW_CLEARANCE: f64 = 36.0;

/// The body is one colour — a gradient reads as a grey smear that never
/// resolves into an object. Walks the band's radial run at its vertical
/// middle and requires every sample to agree (a gradient or a ring-fill
/// seam both fail). Rendered on mid-grey so a translucent body has
/// something to differ against; over black an alpha ramp reads as almost
/// no delta and would pass on a smear.
#[test]
fn the_band_is_filled_flat_with_a_single_colour() {
    let Some(mut renderer) = try_new_renderer(CANVAS_W, CANVAS_H) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };
    let mut tr = TextRenderer::new();
    let (scene, disc) = render_three_spoke_disc(&Theme::dark(), &mut tr);
    let rgba = renderer.render(&scene, Color::from_rgb8(0x80, 0x80, 0x80));

    let (r_in, r_out) = disc.label_band_radii();
    let c = disc.disc_centre();
    // A row inside a sector but off its label. Measured in sectors, not in
    // band thicknesses: labels sit on the centrelines, so the right offset is
    // a fraction of the angular step, and a fixed pixel drop lands wherever
    // the current geometry happens to put it. The band-thickness figure this
    // used to carry came to exactly one sector when the step widened, which
    // put the sample straight through the next label.
    let mid_r = f64::midpoint(r_in, r_out);
    let off = f64::from(disc.spoke_gap_deg()).to_radians() * 0.3;
    let y = (c.y - mid_r * off.sin()).round();
    let dy = y - c.y;
    // Stay off both edges so the rim strokes and the inner hairline are
    // not in the sample.
    let x_from = (c.x + ((r_in * r_in - dy * dy).max(0.0)).sqrt() + 10.0).round() as u32;
    let x_to = (c.x + ((r_out * r_out - dy * dy).max(0.0)).sqrt() - 10.0).round() as u32;
    assert!(
        x_to > x_from + 20,
        "the band is too thin at y={y} to sample: x {x_from}..{x_to}"
    );

    let y_u = y as u32;
    let samples: Vec<u32> = (x_from..=x_to)
        .step_by(4)
        .map(|x| {
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, CANVAS_W, x, y_u);
            (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) as u32
        })
        .collect();
    let lo = *samples.iter().min().unwrap();
    let hi = *samples.iter().max().unwrap();

    // A gradient of any consequence spans well over 80 luma on this
    // mid-grey ground; a flat fill has only dithering and the sector
    // dividers to account for.
    assert!(
        hi - lo <= 8,
        "the band is shaded, not flat: luma runs {lo}..{hi} across its radial run \
         at y={y_u} (samples {samples:?})"
    );
}

/// The disc is a thing you spin, so its contents must turn WITH it: the
/// active label is horizontal at rest (box exactly as tall as the label)
/// and needs a taller box once tipped. Half a detent is the hardest
/// frame, two sectors equally near the middle, so both are written out.
#[test]
fn spinning_the_wheel_turns_the_labels_with_it() {
    let Some(mut renderer) = try_new_renderer(CANVAS_W, CANVAS_H) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };
    let mut tr = TextRenderer::new();

    let mut placed = |deg: f32, name: &str| -> (Vec<Rect>, f32) {
        let (scene, disc) = render_disc_at(&Theme::dark(), &mut tr, deg);
        renderer.render_png(
            &scene,
            Color::from_rgb8(0x02, 0x06, 0x14),
            screenshots_dir().join(format!("wheel-disc-04-{name}.png")),
        );
        (disc.spoke_hit_rects.clone(), disc.spoke_gap_deg())
    };

    // Half a detent, asked of the disc rather than copied. The step is derived
    // per layout — the band's angular height divided across the sectors that
    // fit — so a constant here goes stale silently. The 15.0 this test carried
    // is three detents at the current geometry, which rotated the sector it
    // meant to tip clean out of the band and read as the disc having lost it.
    let (rest, detent) = placed(0.0, "rest");
    let (half, _) = placed(detent / 2.0, "half-detent");

    let active = 0usize;
    let rest_box = rest[active];
    let half_box = half[active];
    assert!(
        rest_box.area() > 0.0 && half_box.area() > 0.0,
        "the active workspace has to be on screen in both frames: {rest_box:?} / {half_box:?}"
    );

    // Upright at rest: the box is the label, so it is far wider than tall.
    // Tipped by half a detent: the same label needs a taller box.
    assert!(
        half_box.height() > rest_box.height() + 8.0,
        "the active label did not tip with its sector — box was {:.0} px tall at rest \
         and {:.0} px tall half a detent on. An upright label beside a turning disc \
         is the rail this design replaced.",
        rest_box.height(),
        half_box.height()
    );

    // And it travelled: a positive rotation carries a sector downward.
    let moved = half_box.center().y - rest_box.center().y;
    assert!(
        moved > 10.0,
        "half a detent moved the active label by {moved:.0} px — the sectors are not \
         turning with the disc"
    );
}

/// Renders under every theme track and requires the surface to change
/// with it — three themes giving three different answers is what a
/// hardcoded palette cannot do.
#[test]
fn the_disc_takes_its_colours_from_the_theme() {
    let Some(mut renderer) = try_new_renderer(CANVAS_W, CANVAS_H) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };
    let mut tr = TextRenderer::new();

    let mut wedges = Vec::new();
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let (scene, disc) = render_three_spoke_disc(&theme, &mut tr);
        let path = screenshots_dir().join(format!("wheel-disc-theme-{name}.png"));
        let ground = theme.tokens.colors.surfaces.base;
        renderer.render_png(&scene, ground, &path);
        assert!(path.exists(), "snapshot PNG should be written to {path:?}");

        // Probe the active wedge against an inactive neighbour at the same
        // radius, isolating the wedge's own tint from the (also themed) body
        // fill underneath.
        let rgba = renderer.render(&scene, ground);
        let (r_in, r_out) = disc.label_band_radii();
        let c = disc.disc_centre();
        let radius = f64::midpoint(r_in, r_out);
        let probe = |deg: f64| -> (i32, i32, i32) {
            let a = deg.to_radians();
            let x = ((c.x + radius * a.cos()).round().max(0.0) as u32).min(CANVAS_W - 1);
            let y = ((c.y + radius * a.sin()).round().max(0.0) as u32).min(CANVAS_H - 1);
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, CANVAS_W, x, y);
            (i32::from(r), i32::from(g), i32::from(b))
        };
        // Offset as a fraction of the disc's own detent, not a fixed angle: the
        // step is derived per layout, so 4 deg was inside the active sector
        // when a sector was 15 deg wide and is two sectors away now. Both
        // probes sit the same way off their sector's centreline, so the
        // difference between them is the active wedge's tint and nothing else.
        let gap = f64::from(disc.spoke_gap_deg());
        let lit = probe(gap * 0.3);
        let unlit = probe(gap * 1.3);
        assert_ne!(
            lit, unlit,
            "{name}: the active sector is painted the same as its neighbour — \
             nothing marks which workspace is selected"
        );

        // The channel the wedge pushes hardest must be the channel the
        // theme's own workspace colour leads with.
        let delta = [lit.0 - unlit.0, lit.1 - unlit.1, lit.2 - unlit.2];
        let lead = |c: [i32; 3]| (0..3).max_by_key(|&i| c[i]).expect("three channels");
        let want = theme.tokens.colors.categorical(0);
        let want_ch = lead([
            i32::from(want.r()),
            i32::from(want.g()),
            i32::from(want.b()),
        ]);
        assert_eq!(
            lead(delta),
            want_ch,
            "{name}: the active wedge shifts channel {} hardest but the theme's \
             workspace colour {want:?} leads with channel {want_ch} — the disc is \
             not painting the themed colour (delta {delta:?})",
            lead(delta)
        );

        wedges.push((name, lit));
    }

    for i in 0..wedges.len() {
        for j in (i + 1)..wedges.len() {
            assert_ne!(
                wedges[i].1, wedges[j].1,
                "the disc painted identical pixels under {} and {} — its colours \
                 are not coming from the theme",
                wedges[i].0, wedges[j].0
            );
        }
    }
}
