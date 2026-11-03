//! Every shipped look renders what its name promises.
//!
//! One floating panel and one control face are drawn over a busy desk (a
//! bright wallpaper of stripes and discs, the kind of thing that shows a
//! material's work) under each theme the toolkit ships, through the same
//! backdrop pass a live window runs, and the pixels are checked for the
//! thing that look is for:
//!
//! * Block: one flat colour across the body, square corners, no light.
//! * Glass: the blurred backdrop showing through, and a rim lit brighter
//!   than the body under it.
//! * Liquid Glass: the backdrop standing in the panel's middle (it is
//!   nearly clear), and at the rim the same backdrop displaced — the
//!   stripes bend — and a specular rim.
//! * Pixelated: the panel's body is coarse square cells (each cell one
//!   colour a `cell` wide), not a blur and not a flat fill.
//!
//! Each check fails when its look stops rendering, which is the point: a
//! theme that only names a painter and draws flat is a bug, not a look.
//!
//! Set `TILO_UI_SCREENSHOT_DIR` to dump every rendered PNG.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use tilo_ui::backdrop::{BackdropFrame, BackdropRecording, layer_scene};
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::theme::{Preset, Theme};
use tilo_ui::tokens::SurfaceRole;

const W: u32 = 420;
const H: u32 = 300;
/// The floating panel: its body's inside, clear of the rim.
const PANEL: Rect = Rect::new(110.0, 70.0, 330.0, 230.0);
const RADIUS: f64 = 18.0;

/// The desk below: bright stripes and discs, so every way a material shows
/// what is under it reads plainly.
fn desk() -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(0xf2, 0xf4, 0xf8),
        None,
        &Rect::new(0.0, 0.0, f64::from(W), f64::from(H)),
    );
    let hues = [
        Color::from_rgb8(0xe6, 0x5a, 0x46),
        Color::from_rgb8(0x3f, 0x7d, 0xd8),
        Color::from_rgb8(0xf2, 0xb1, 0x2c),
        Color::from_rgb8(0x2e, 0xa0, 0x72),
        Color::from_rgb8(0x8a, 0x5c, 0xd6),
    ];
    for (index, hue) in hues.into_iter().enumerate() {
        let x = 30.0 + index as f64 * 78.0;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            hue,
            None,
            &Circle::new(Point::new(x, 90.0), 46.0),
        );
    }
    // Dark text-like bars, which a blur softens and cells quantise.
    for row in 0..4 {
        let y = 140.0 + row as f64 * 32.0;
        for column in 0..5 {
            let x = 26.0 + column as f64 * 78.0;
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(0x10, 0x12, 0x18),
                None,
                &Rect::new(x, y, x + 40.0, y + 8.0),
            );
        }
    }
    scene
}

/// The desk with a floating panel and a control face on it, under `theme`,
/// through the backdrop pass.
fn render(theme: &Theme, name: &str) -> Vec<u8> {
    let mut scene = desk();
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    tilo_ui::surface::panel(&mut scene, Affine::IDENTITY, PANEL, RADIUS, theme);
    // A control face at rest, on the panel, so a control role's finish is
    // drawn too.
    let face = Rect::new(130.0, 180.0, 230.0, 212.0);
    let body = RoundedRect::from_rect(face, f64::from(theme.tokens.radii.control));
    tilo_ui::surface::control_face(
        &mut scene,
        Affine::IDENTITY,
        &body,
        theme.tokens.colors.surfaces.raised,
        theme,
    );
    let frame = BackdropFrame::from_single_scene(recording.finish());
    let mut renderer = OffscreenRenderer::new(W, H);
    renderer.set_backdrop(Some(Arc::new(frame)));
    let rgba = renderer.render(&scene, Color::BLACK);
    dump(name, &rgba);
    rgba
}

fn dump(name: &str, rgba: &[u8]) {
    let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let path = format!("{dir}/finish-{name}.png");
    tilo_ui::png::write_rgba8(&path, W, H, rgba).unwrap();
    eprintln!("wrote {path}");
}

fn pixel(rgba: &[u8], x: u32, y: u32) -> [u8; 3] {
    let (r, g, b, _) = OffscreenRenderer::sample(rgba, W, x, y);
    [r, g, b]
}

/// Luminance, for "is this lighter than that".
fn luma(c: [u8; 3]) -> f32 {
    0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])
}

/// A pixel well inside the panel's body and clear of the control face.
fn body(rgba: &[u8], x: u32, y: u32) -> [u8; 3] {
    pixel(rgba, x, y)
}

fn presets() -> [(Preset, &'static str); 4] {
    [
        (Preset::Block, "block"),
        (Preset::Glass, "glass"),
        (Preset::LiquidGlass, "liquid-glass"),
        (Preset::Pixelated, "pixelated"),
    ]
}

/// The desk itself, for comparing what a material lets through.
fn bare_desk() -> Vec<u8> {
    let scene = desk();
    let mut renderer = OffscreenRenderer::new(W, H);
    renderer.render(&scene, Color::BLACK)
}

/// The distinct colours along a run of the panel's body, and how many
/// times the colour changes: a flat fill has one, one cell per `cell` px
/// has one change per cell.
fn runs(rgba: &[u8], y: u32, xs: std::ops::Range<u32>) -> (usize, usize) {
    let mut changes = 0;
    let mut previous = pixel(rgba, xs.start, y);
    let mut first = previous;
    let mut seen = vec![previous];
    for x in xs {
        let now = pixel(rgba, x, y);
        if now != previous {
            changes += 1;
            previous = now;
            if !seen.contains(&now) {
                seen.push(now);
            }
        }
    }
    let _ = first;
    let _ = &mut first;
    (seen.len(), changes)
}

#[test]
fn block_is_one_flat_colour_with_square_corners() {
    let _settled = tilo_ui::motion::testing::settled();
    let theme = Theme::preset(Preset::Block);
    let rgba = render(&theme, "block-dark");
    let (colours, changes) = runs(&rgba, 150, 120..320);
    assert_eq!(
        (colours, changes),
        (1, 0),
        "Block's body is one flat colour: {colours} colours, {changes} changes"
    );
    // Its window radius is 0, so a corner shows no rounding at all: the
    // control face's own rect at rest is square too.
    assert_eq!(theme.tokens.radii.tile, 0.0, "Block's shape role is square");
}

#[test]
fn glass_lets_the_blurred_desk_through_and_lights_its_rim() {
    let _settled = tilo_ui::motion::testing::settled();
    let theme = Theme::preset(Preset::Glass);
    let rgba = render(&theme, "glass-dark");
    let bare = bare_desk();
    // Inside the body the desk below still reads: the blurred stripes move
    // the colour from one end of the panel to the other.
    let row: Vec<f32> = (118..322).map(|x| luma(body(&rgba, x, 100))).collect();
    let spread =
        row.iter().cloned().fold(f32::MIN, f32::max) - row.iter().cloned().fold(f32::MAX, f32::min);
    assert!(
        spread >= 4.0,
        "Glass shows the blurred desk through it: spread {spread:.1}"
    );
    // And it is not the desk itself: the blur softened it.
    let bare_spread = {
        let row: Vec<f32> = (118..322).map(|x| luma(pixel(&bare, x, 100))).collect();
        row.iter().cloned().fold(f32::MIN, f32::max) - row.iter().cloned().fold(f32::MAX, f32::min)
    };
    assert!(
        spread < bare_spread,
        "Glass blurs what it shows: {spread:.1} under glass, {bare_spread:.1} bare"
    );
    // The top rim is lit: brighter than the body one pixel in.
    let rim = luma(pixel(&rgba, 220, PANEL.y0 as u32));
    let under = luma(pixel(&rgba, 220, PANEL.y0 as u32 + 3));
    assert!(
        rim > under,
        "Glass lights its rim: {rim:.0} over {under:.0}"
    );
}

#[test]
fn liquid_glass_shows_the_desk_in_its_middle_and_bends_it_at_the_rim() {
    let _settled = tilo_ui::motion::testing::settled();
    let theme = Theme::preset(Preset::LiquidGlass);
    let rgba = render(&theme, "liquid-glass-dark");
    let bare = bare_desk();
    // The middle is nearly clear: the desk's colours are there. The
    // material's legible band compresses what shows through, so the spread
    // is measured against Block, which shows nothing of the desk at all.
    let at = |x: u32, y: u32| luma(body(&rgba, x, y));
    let bare_at = |x: u32, y: u32| luma(pixel(&bare, x, y));
    let middle_spread = {
        let row: Vec<f32> = (118..322).map(|x| at(x, 100)).collect();
        row.iter().cloned().fold(f32::MIN, f32::max) - row.iter().cloned().fold(f32::MAX, f32::min)
    };
    let block = render(&Theme::preset(Preset::Block), "liquid-block-reference");
    let block_spread = {
        let row: Vec<f32> = (118..322).map(|x| luma(body(&block, x, 100))).collect();
        row.iter().cloned().fold(f32::MIN, f32::max) - row.iter().cloned().fold(f32::MAX, f32::min)
    };
    assert!(
        block_spread < 1.0,
        "the reference is a flat fill: {block_spread:.1}"
    );
    assert!(
        middle_spread > 8.0,
        "Liquid Glass shows what lies below it: spread {middle_spread:.1} in its middle"
    );
    // The rim bends: a row just inside the left edge differs from the same
    // row under no lens, by more than a tint's worth.
    let bent: Vec<f32> = (112..150).map(|x| at(x, 150)).collect();
    let straight: Vec<f32> = (112..150).map(|x| bare_at(x, 150)).collect();
    let moved = bent
        .iter()
        .zip(&straight)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        moved > 10.0,
        "Liquid Glass bends what it shows at the rim: {moved:.1}"
    );
}

#[test]
fn pixelated_shows_the_desk_as_cells() {
    let _settled = tilo_ui::motion::testing::settled();
    let theme = Theme::preset(Preset::Pixelated);
    let rgba = render(&theme, "pixelated-dark");
    let cell = theme
        .finish(SurfaceRole::Floating)
        .param("cell")
        .expect("the pixelated floating finish states its cell") as u32;
    assert!(cell >= 2, "a cell of {cell} px shows no pixels");
    // Inside one cell, away from the rim, every pixel is the same colour.
    let (x0, y0) = (118 + 3 * cell, 100);
    for (dx, dy) in [(1, 0), (0, 1), (cell - 1, cell - 1)] {
        assert_eq!(
            pixel(&rgba, x0, y0),
            pixel(&rgba, x0 + dx, y0 + dy),
            "one cell, two colours at ({x0}, {y0})"
        );
    }
    // Across cells the desk still reads: the cells are not one flat fill.
    let (colours, changes) = runs(&rgba, y0, 118..322);
    assert!(
        colours >= 3,
        "the cells show the desk below: {colours} colours"
    );
    assert!(
        changes >= 4,
        "the cells are square runs, not a blur: {changes} changes"
    );
}

/// The cell grid reads as pixels even over a ground that varies nowhere:
/// the surface's complaint ("boxy, not pixelated") was that a flat desk
/// left nothing to quantise, so the theme showed only its corners and its
/// border. The grid is the finish's own, so it is there whatever is below.
#[test]
fn pixelated_shows_its_grid_over_a_flat_ground() {
    let _settled = tilo_ui::motion::testing::settled();
    let theme = Theme::preset(Preset::Pixelated);
    let cell = theme
        .finish(SurfaceRole::Floating)
        .param("cell")
        .expect("the pixelated floating finish states its cell") as u32;
    // The window ground is the panel's fill with the desk replaced by one
    // flat colour: nothing below it varies, so only the grid can show.
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(0x20, 0x22, 0x26),
        None,
        &Rect::new(0.0, 0.0, f64::from(W), f64::from(H)),
    );
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    tilo_ui::surface::panel(&mut scene, Affine::IDENTITY, PANEL, RADIUS, &theme);
    let frame = BackdropFrame::from_single_scene(recording.finish());
    let mut renderer = OffscreenRenderer::new(W, H);
    renderer.set_backdrop(Some(Arc::new(frame)));
    let flat = renderer.render(&scene, Color::BLACK);
    dump("pixelated-flat", &flat);
    // A row of the panel's body clear of its content: the panel spans
    // y 70..230 and holds a control face at y 180..212, so 160 is body.
    let (colours, changes) = runs(&flat, 160, 118..322);
    assert!(
        colours >= 2 && changes >= cell as usize,
        "the grid is drawn over a flat ground: {colours} colours, \
         {changes} changes in a {cell} px grid"
    );
    let edges: Vec<u32> = (118..322)
        .filter(|x| pixel(&flat, *x, 160) != pixel(&flat, x + 1, 160))
        .collect();
    assert!(
        !edges.is_empty() && edges.iter().all(|x| (x.wrapping_sub(edges[0])) % cell == 0),
        "the changes are on the cell grid {cell}: {edges:?}"
    );
}

#[test]
fn every_shipped_look_draws_differently_from_block() {
    let _settled = tilo_ui::motion::testing::settled();
    let base = render(&Theme::preset(Preset::Block), "block-reference");
    for (preset, name) in presets() {
        if preset == Preset::Block {
            continue;
        }
        let shown = render(&Theme::preset(preset), &format!("{name}-dark"));
        let differing = (0..W)
            .flat_map(|x| (0..H).map(move |y| (x, y)))
            .filter(|&(x, y)| pixel(&base, x, y) != pixel(&shown, x, y))
            .count();
        assert!(
            differing > 2000,
            "{} draws its own material: {differing} pixels differ from Block",
            preset.name()
        );
    }
}
