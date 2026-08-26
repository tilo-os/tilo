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
fn render_three_spoke_disc(theme: &Theme, tr: &mut TextRenderer) -> (Scene, SidebarWheelDisc) {
    render_disc_at(theme, tr, 0.0)
}

/// One display slot, in degrees. Mirrors the widget's own step so the
/// rotation test can ask for "half a detent" without hardcoding it.
const SPOKE_STEP_DEG: f32 = 15.0;

/// Same disc, laid out and painted at `rotation_deg` of wheel rotation.
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
    let (scene, _disc) = render_three_spoke_disc(&Theme::high_contrast(), &mut tr);
    let rgba = renderer.render(&scene, Color::BLACK);
    // Written out as well as sampled. The pixel probe below can only say
    // the rim survived; whether the whole wheel is still legible under a
    // palette that grounds every surface on pure black is a question for
    // someone looking at it.
    renderer.render_png(
        &scene,
        Color::BLACK,
        &screenshots_dir().join("wheel-disc-03-high-contrast.png"),
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

/// Summoning lifts the disc off the strip and puts it wherever the pointer
/// is — which is usually on top of an app, and apps are frequently bright.
/// The band has to be a real surface there, or the shell's near-white
/// labels land on a near-white document.
///
/// It also has to END. A wheel that blacks out the whole screen passes
/// "is it a surface" just as happily, and is the other way to fail: the
/// disc is a slice of something large, so the app must still be plainly
/// visible beyond the disc's own footprint.
///
/// Where it ends moved. The body used to stop at an inner arc, leaving
/// the app showing through the left of the slot, and the probe for
/// "still a slice" looked there. The body is solid now — that hollow
/// was the thing the owner asked to close — so the surface runs off the
/// left edge of the footprint instead and the probe moved with it. Both
/// halves of the original claim are still checked, and the solid fill is
/// now checked too.
///
/// Sample points are derived from the band the widget actually paints
/// rather than hardcoded, so tuning the depth moves the probes with it
/// instead of silently testing open wallpaper.
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
        (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) as u32
    };

    let (r_in, r_out) = disc.label_band_radii();
    let c = disc.disc_centre();
    let footprint = disc.get_rect();
    // The band's vertical middle, where it is at full alpha and widest.
    let row_y = c.y.round() as u32;
    let on_band = ((c.x + (r_in + r_out) / 2.0).round() as u32).min(CANVAS_W - 1);
    // Well inside where the old inner arc used to cut the body off.
    let inside_arc = ((c.x + r_in - 24.0).round() as u32).min(CANVAS_W - 1);
    assert!(
        (inside_arc as f64) > footprint.x0 + 2.0,
        "probe at x={inside_arc} is outside the disc's own footprint {footprint:?} \
         — it would be testing open wallpaper"
    );

    let band = luma(on_band, row_y);
    assert!(
        band < 110,
        "the band let the bright app through (luma {band}/255 at ({on_band},{row_y})) \
         — the labels painted on it have nothing to read against; see {path:?}"
    );

    // Solid: the body reaches the centre, so a point that used to fall
    // in the hole is now surface too, and just as opaque.
    let inner = luma(inside_arc, row_y);
    assert!(
        inner < 110,
        "the disc is hollow again — the bright app shows through at luma {inner}/255 \
         at ({inside_arc},{row_y}), inside the footprint. The body is meant to be a \
         solid segment, filled to the centre; see {path:?}"
    );

    // ...and it still ends. The disc mounts flush to the left edge here,
    // exactly as it does in the shell's sidebar column, so the edge it
    // has to end at is the RIM: past that — clear of the rim's outward
    // shadow — the app must be plainly visible, or the wheel has taken
    // over the screen instead of occupying a slice of it.
    let outside = ((c.x + r_out + RIM_SHADOW_CLEARANCE).round() as u32).min(CANVAS_W - 1);
    assert!(
        (outside as f64) > footprint.x1 - 1.0 || (outside as f64) > c.x + r_out,
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

/// Pixels past the rim at which the app must be fully visible again —
/// clear of the rim's outward shadow, which is allowed to reach a little
/// beyond the rim by design.
const RIM_SHADOW_CLEARANCE: f64 = 36.0;

/// The body is ONE colour. It used to be a nine-stop radial ramp from
/// transparent at the inner end to a peak alpha at the rim, and on screen
/// that is a grey smear that never resolves into an object — the single
/// thing the owner named when they asked for a flat disc.
///
/// Walks the band's radial run at its vertical middle and requires every
/// sample to agree. A gradient of any consequence fails; so does a return
/// to painting the surface as a stack of concentric strokes, which left a
/// brighter seam wherever two translucent rings overlapped.
///
/// Rendered on a mid-grey ground so a translucent body has something to
/// differ against — over black an alpha ramp reads as almost no delta at
/// all and this would pass on a smear.
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
    // A row a third of the way down the footprint: inside a sector, clear
    // of the active wedge's tint at the middle and of the labels, which
    // sit on the sector centrelines.
    let y = (c.y - (r_out - r_in) * 0.75).round();
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
            (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) as u32
        })
        .collect();
    let lo = *samples.iter().min().unwrap();
    let hi = *samples.iter().max().unwrap();

    // The old ramp ran from 0 to 168/255 alpha across this run, which on a
    // mid-grey ground is a spread of well over 80 luma. A flat fill has
    // only dithering and the sector dividers to account for.
    assert!(
        hi - lo <= 8,
        "the band is shaded, not flat: luma runs {lo}..{hi} across its radial run \
         at y={y_u} (samples {samples:?})"
    );
}

/// The disc is a thing you spin, so its contents have to turn WITH it.
/// The design this replaced failed exactly there: workspaces were pills
/// in an upright column and the arc behind them was scenery, so turning
/// the wheel slid a list and left the disc sitting still.
///
/// A label lying on the surface tips as its sector comes round, and that
/// is the difference, so that is what this measures. The active label is
/// horizontal at rest — its box is exactly as tall as the label — and a
/// tilted one needs a taller box to hold the same text. Half a detent is
/// also the hardest frame to look at, two sectors equally near the
/// middle, so both renders are written out.
#[test]
fn spinning_the_wheel_turns_the_labels_with_it() {
    let Some(mut renderer) = try_new_renderer(CANVAS_W, CANVAS_H) else {
        eprintln!("skip: no wgpu adapter available for offscreen render");
        return;
    };
    let mut tr = TextRenderer::new();

    let mut placed = |deg: f32, name: &str| -> Vec<Rect> {
        let (scene, disc) = render_disc_at(&Theme::dark(), &mut tr, deg);
        renderer.render_png(
            &scene,
            Color::from_rgb8(0x02, 0x06, 0x14),
            &screenshots_dir().join(format!("wheel-disc-04-{name}.png")),
        );
        disc.spoke_hit_rects.clone()
    };

    let rest = placed(0.0, "rest");
    let half = placed(SPOKE_STEP_DEG / 2.0, "half-detent");

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

/// Renders the disc under every theme track the shell can be switched
/// into, and requires the surface to actually change with the theme.
///
/// The disc used to paint six literal Tailwind hex values for its
/// workspace colours, so the same maroon-tinted pink sector arrived
/// under the light theme and under high contrast alike — no theme could
/// reach it. Sampling the active wedge in three themes and requiring
/// three different answers is what keeps that from coming back: a
/// hardcoded constant cannot pass a test that asks the theme to move it.
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

        // Probe the active wedge against an inactive neighbour at the
        // same radius. Comparing the two isolates the wedge's own tint
        // from the body fill underneath, which is themed as well and
        // would otherwise be most of the difference.
        //
        // Both probes sit 4° off their sector's centreline, which is
        // clear of the label chip drawn there and well inside the
        // dividers at ±7.5°.
        let rgba = renderer.render(&scene, ground);
        let (r_in, r_out) = disc.label_band_radii();
        let c = disc.disc_centre();
        let radius = (r_in + r_out) / 2.0;
        let probe = |deg: f64| -> (i32, i32, i32) {
            let a = deg.to_radians();
            let x = ((c.x + radius * a.cos()).round().max(0.0) as u32).min(CANVAS_W - 1);
            let y = ((c.y + radius * a.sin()).round().max(0.0) as u32).min(CANVAS_H - 1);
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, CANVAS_W, x, y);
            (r as i32, g as i32, b as i32)
        };
        let lit = probe(4.0);
        let unlit = probe(19.0);
        assert_ne!(
            lit, unlit,
            "{name}: the active sector is painted the same as its neighbour — \
             nothing marks which workspace is selected"
        );

        // The channel the wedge pushes hardest must be the channel the
        // theme's own workspace colour leads with. This is the
        // assertion the hardcoded palette could not pass: it painted
        // `pink-500`, which leads with red, while every theme's derived
        // series leads with its accent.
        let delta = [lit.0 - unlit.0, lit.1 - unlit.1, lit.2 - unlit.2];
        let lead = |c: [i32; 3]| {
            (0..3).max_by_key(|&i| c[i]).expect("three channels")
        };
        let want = theme.tokens.colors.categorical(0);
        let want_ch = lead([want.r() as i32, want.g() as i32, want.b() as i32]);
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
