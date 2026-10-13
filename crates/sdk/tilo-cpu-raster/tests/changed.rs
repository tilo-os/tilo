//! What [`CpuRasterizer::changed_regions`] reports between two scenes, and
//! that rendering only those regions over the old frame equals rendering the
//! new scene whole.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test scenes: failures are the assertion"
)]

use tilo_cpu_raster::{CpuRasterizer, Options, Region, Target};
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

const W: u32 = 128;
const H: u32 = 96;

fn scene(chip: Color, moved: bool) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(20, 20, 30),
        None,
        &Rect::new(0.0, 0.0, f64::from(W), f64::from(H)),
    );
    scene.push_clip_layer(
        Fill::NonZero,
        Affine::IDENTITY,
        &Rect::new(4.0, 4.0, 124.0, 92.0),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        chip,
        None,
        &RoundedRect::new(10.0, 10.0, 40.0, 30.0, 6.0),
    );
    let x = if moved { 70.0 } else { 60.0 };
    scene.stroke(
        &Stroke::new(2.0),
        Affine::IDENTITY,
        Color::from_rgb8(200, 200, 200),
        None,
        &Rect::new(x, 50.0, x + 20.0, 70.0),
    );
    scene.pop_layer();
    scene
}

fn render_full(scene: &Scene) -> Vec<u8> {
    render_sized(scene, W, H)
}

fn render_sized(scene: &Scene, width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; (width * height * 4) as usize];
    let mut target = Target::new(&mut out, width, height, width as usize * 4).unwrap();
    CpuRasterizer::new(Options::default()).render(
        scene.encoding(),
        &mut target,
        None,
        Color::BLACK,
    );
    out
}

fn covers(regions: &[Region], x: u32, y: u32) -> bool {
    regions
        .iter()
        .any(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
}

#[test]
fn a_changed_draw_is_the_only_region_and_repainting_it_matches_a_full_render() {
    let before = scene(Color::from_rgb8(40, 90, 200), false);
    let after = scene(Color::from_rgb8(200, 90, 40), false);
    let mut raster = CpuRasterizer::new(Options::default());
    let mut frame = vec![0u8; (W * H * 4) as usize];
    {
        let mut target = Target::new(&mut frame, W, H, W as usize * 4).unwrap();
        raster.render(before.encoding(), &mut target, None, Color::BLACK);
    }
    assert!(
        raster.changed_regions(W, H).is_none(),
        "nothing to compare with yet"
    );

    raster.prepare(after.encoding());
    let regions = raster.changed_regions(W, H).unwrap();
    assert!(covers(&regions, 25, 20), "the recoloured chip");
    assert!(!covers(&regions, 70, 60), "the untouched outline");
    {
        let mut target = Target::new(&mut frame, W, H, W as usize * 4).unwrap();
        raster.render_prepared(&mut target, Some(&regions), Color::BLACK);
    }
    assert_eq!(frame, render_full(&after));

    // Moving the outline reports where it was and where it is.
    let moved = scene(Color::from_rgb8(200, 90, 40), true);
    raster.prepare(moved.encoding());
    let regions = raster.changed_regions(W, H).unwrap();
    assert!(covers(&regions, 60, 60) && covers(&regions, 89, 60));
    assert!(!covers(&regions, 25, 20));
    {
        let mut target = Target::new(&mut frame, W, H, W as usize * 4).unwrap();
        raster.render_prepared(&mut target, Some(&regions), Color::BLACK);
    }
    assert_eq!(frame, render_full(&moved));

    // An identical scene changes nothing.
    raster.prepare(moved.encoding());
    assert_eq!(raster.changed_regions(W, H), Some(Vec::new()));
}

const PW: u32 = 256;
const PH: u32 = 160;

/// A grid that stays put, a clip that stays put, and inside it a window
/// that pans: by its transform, or by its own coordinates.
fn panned(offset: f64, by_transform: bool) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(20, 20, 30),
        None,
        &Rect::new(0.0, 0.0, f64::from(PW), f64::from(PH)),
    );
    for x in (0..PW).step_by(24) {
        let x = f64::from(x) + 0.5;
        scene.stroke(
            &Stroke::new(1.0),
            Affine::IDENTITY,
            Color::from_rgb8(40, 40, 60),
            None,
            &vello::kurbo::Line::new((x, 0.0), (x, f64::from(PH))),
        );
    }
    scene.push_clip_layer(
        Fill::NonZero,
        Affine::IDENTITY,
        &Rect::new(8.0, 8.0, 248.0, 140.0),
    );
    let (transform, origin) = if by_transform {
        (Affine::translate((offset, 0.0)), 0.0)
    } else {
        (Affine::IDENTITY, offset)
    };
    scene.fill(
        Fill::NonZero,
        transform,
        Color::from_rgb8(30, 32, 40),
        None,
        &RoundedRect::new(origin + 20.0, 20.0, origin + 180.0, 130.0, 10.0),
    );
    scene.stroke(
        &Stroke::new(1.5),
        transform,
        Color::from_rgb8(220, 220, 220),
        None,
        &Rect::new(origin + 40.3, 40.7, origin + 120.6, 60.2),
    );
    scene.pop_layer();
    // A static badge drawn over the panning window.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(200, 60, 60),
        None,
        &Rect::new(100.0, 100.0, 120.0, 112.0),
    );
    scene
}

fn tile_of(x: u32, y: u32) -> (u32, u32) {
    (x / 16, y / 16)
}

#[test]
fn a_pan_moves_the_opaque_pixels_and_draws_only_the_rest() {
    for (case, before, after) in [
        ("transform", panned(0.0, true), panned(24.0, true)),
        ("layout", panned(0.0, false), panned(24.0, false)),
        (
            "layout and transform",
            laid_out(40.0, 0.0),
            laid_out(64.0, 10.0),
        ),
    ] {
        let mut raster = CpuRasterizer::new(Options::default());
        let mut frame = vec![0u8; (PW * PH * 4) as usize];
        {
            let mut target = Target::new(&mut frame, PW, PH, PW as usize * 4).unwrap();
            raster.render(before.encoding(), &mut target, None, Color::BLACK);
        }
        raster.prepare(after.encoding());
        let changes = raster.changes(PW, PH).unwrap();
        let motion = changes.motion.clone().expect("the window moved as a whole");
        assert_eq!((motion.dx, motion.dy), (24, 0));
        let movable = motion.movable(PW, PH, 16);
        let moved_px: u32 = movable.iter().map(|r| r.width * r.height).sum();
        assert!(
            moved_px >= 16 * 16 * 8,
            "most of the window interior moves: {movable:?}"
        );
        assert!(
            !covers(&movable, 105, 105),
            "the static badge is drawn over the window"
        );

        // Tiles that changed and are not moved are drawn.
        let mut moved_tiles = std::collections::HashSet::new();
        for r in &movable {
            for y in (r.y..r.y + r.height).step_by(16) {
                for x in (r.x..r.x + r.width).step_by(16) {
                    moved_tiles.insert(tile_of(x, y));
                }
            }
        }
        let mut redraw = Vec::new();
        for ty in 0..PH.div_ceil(16) {
            for tx in 0..PW.div_ceil(16) {
                let (x, y) = (tx * 16, ty * 16);
                let tile = Region::new(x, y, 16.min(PW - x), 16.min(PH - y));
                let touched = changes.regions.iter().any(|r| {
                    r.x < tile.x + tile.width
                        && tile.x < r.x + r.width
                        && r.y < tile.y + tile.height
                        && tile.y < r.y + r.height
                });
                if touched && !moved_tiles.contains(&(tx, ty)) {
                    redraw.push(tile);
                }
            }
        }
        {
            let mut target = Target::new(&mut frame, PW, PH, PW as usize * 4).unwrap();
            tilo_cpu_raster::move_pixels(&mut target, &movable, motion.dx, motion.dy);
            raster.render_prepared(&mut target, Some(&redraw), Color::BLACK);
        }
        let full = {
            let mut out = vec![0u8; (PW * PH * 4) as usize];
            let mut target = Target::new(&mut out, PW, PH, PW as usize * 4).unwrap();
            CpuRasterizer::new(Options::default()).render(
                after.encoding(),
                &mut target,
                None,
                Color::BLACK,
            );
            out
        };
        let bad: Vec<(usize, usize, u8)> = frame
            .chunks_exact(4)
            .zip(full.chunks_exact(4))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| {
                let d = a
                    .iter()
                    .zip(b.iter())
                    .map(|(x, y)| x.abs_diff(*y))
                    .max()
                    .unwrap();
                (i % PW as usize, i / PW as usize, d)
            })
            .collect();
        // vello_cpu's anti-aliasing is translation invariant up to rounding:
        // an edge pixel may land one level apart after a whole-pixel move.
        assert!(
            bad.len() <= 4 && bad.iter().all(|&(_, _, d)| d <= 1),
            "moved and redrawn pixels equal a full render ({case}): {bad:?}"
        );
    }
}

#[test]
fn an_inserted_draw_does_not_repaint_the_draws_after_it() {
    let row = |extra: bool, last: Color| {
        let mut scene = Scene::new();
        for i in 0..8 {
            let x = 8.0 + f64::from(i) * 14.0;
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                if i == 7 {
                    last
                } else {
                    Color::from_rgb8(90, 90, 90)
                },
                None,
                &Rect::new(x, 8.0, x + 10.0, 18.0),
            );
            if extra && i == 0 {
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgb8(200, 0, 0),
                    None,
                    &Rect::new(10.0, 40.0, 14.0, 44.0),
                );
            }
        }
        scene
    };
    let mut raster = CpuRasterizer::new(Options::default());
    raster.prepare(row(false, Color::from_rgb8(90, 90, 90)).encoding());
    raster.prepare(row(true, Color::from_rgb8(0, 0, 200)).encoding());
    let regions = raster.changed_regions(W, H).unwrap();
    assert!(covers(&regions, 11, 41), "the inserted draw");
    assert!(
        covers(&regions, 8 + 7 * 14 + 2, 10),
        "the recoloured last draw"
    );
    assert!(
        !covers(&regions, 8 + 3 * 14 + 2, 10),
        "the unchanged draws between them: {regions:?}"
    );
}

/// A window drawn at `x` on screen, laid out at `layout` and moved there
/// through its transform by `x - layout`.
fn laid_out(x: f64, layout: f64) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(20, 20, 30),
        None,
        &Rect::new(0.0, 0.0, f64::from(PW), f64::from(PH)),
    );
    let transform = Affine::translate((x - layout, 0.0));
    scene.fill(
        Fill::NonZero,
        transform,
        Color::from_rgb8(30, 32, 40),
        None,
        &RoundedRect::new(layout + 20.0, 20.0, layout + 120.0, 100.0, 8.0),
    );
    scene.stroke(
        &Stroke::new(1.5),
        transform,
        Color::from_rgb8(220, 220, 220),
        None,
        &Rect::new(layout + 30.3, 30.7, layout + 90.6, 50.2),
    );
    scene
}

#[test]
fn a_draw_whose_path_and_transform_moved_against_each_other_is_unchanged() {
    let before = laid_out(40.0, 0.0);
    let after = laid_out(40.0, 40.0);
    assert_eq!(render_sized(&before, PW, PH), render_sized(&after, PW, PH));
    let mut raster = CpuRasterizer::new(Options::default());
    raster.prepare(before.encoding());
    raster.prepare(after.encoding());
    assert_eq!(
        raster.changed_regions(PW, PH),
        Some(Vec::new()),
        "the same pixels, laid out elsewhere and translated back"
    );
}

#[test]
fn a_pan_through_both_path_and_transform_is_one_motion() {
    let mut raster = CpuRasterizer::new(Options::default());
    raster.prepare(laid_out(40.0, 0.0).encoding());
    raster.prepare(laid_out(64.0, 10.0).encoding());
    let changes = raster.changes(PW, PH).unwrap();
    let motion = changes.motion.expect("the window moved as a whole");
    assert_eq!((motion.dx, motion.dy), (24, 0));
}
#[test]
fn only_a_layer_blending_other_than_source_over_reads_the_backdrop() {
    use vello::peniko::{BlendMode, Compose, Mix};
    let with_layer = |blend: Option<BlendMode>| {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(20, 20, 30),
            None,
            &Rect::new(0.0, 0.0, 64.0, 64.0),
        );
        let shape = Rect::new(8.0, 8.0, 56.0, 56.0);
        match blend {
            Some(blend) => scene.push_layer(Fill::NonZero, blend, 0.8, Affine::IDENTITY, &shape),
            None => scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &shape),
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(200, 100, 50),
            None,
            &Rect::new(0.0, 0.0, 64.0, 64.0),
        );
        scene.pop_layer();
        scene
    };
    for mix in [Mix::SoftLight, Mix::Multiply] {
        let scene = with_layer(Some(BlendMode::new(mix, Compose::SrcOver)));
        assert!(tilo_cpu_raster::reads_backdrop(scene.encoding()), "{mix:?}");
    }
    assert!(!tilo_cpu_raster::reads_backdrop(
        with_layer(Some(BlendMode::new(Mix::Normal, Compose::SrcOver))).encoding()
    ));
    assert!(!tilo_cpu_raster::reads_backdrop(
        with_layer(None).encoding()
    ));
    assert!(!tilo_cpu_raster::reads_backdrop(
        laid_out(0.0, 0.0).encoding()
    ));
}
