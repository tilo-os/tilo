//! A scroll in an app a host shows in a region of its scene ([`EmbeddedApp`])
//! hands the host what moved, and a host that presents each frame over the
//! last ([`crate::scene_parts::damage_with_copies`],
//! [`crate::scene_parts::present_copies`]) copies it and repaints the rest:
//! every presented frame equals a full render of the host's scene, in dark,
//! light and high contrast, at 100 % and 200 %, with a collaborator's mark
//! and a badge the host draws over the app, and under a translucent layer
//! (which copies nothing).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test scenes and pixel tallies: failures are the assertion"
)]

use super::*;
use crate::offscreen::OffscreenRenderer;
use crate::scene_parts::{PartKey, SectionParts};
use vello::kurbo::{Circle, RoundedRect};
use vello::peniko::{BlendMode, Fill, Gradient};

/// Where the host shows the app, in its logical pixels.
const REGION: KurboRect = KurboRect::new(40.0, 24.0, 680.0, 504.0);
/// The host's scene, in logical pixels.
const SCENE: (f64, f64) = (720.0, 540.0);

/// A long list in a scroller, as an app's body.
fn list_app(theme: &Theme) -> App {
    let mut column = crate::widget::Column::new().with_intrinsic_height();
    for row in 0..160 {
        column = column.push(Box::new(crate::widget::Text::new(&format!(
            "Row {row}: the quick brown fox jumps over the lazy dog"
        ))));
    }
    let mut app = App::new_with_controlled_inputs(
        crate::state::State::new(),
        Box::new(crate::widget::Scrollable::new_vertical().with_child(Box::new(column))),
    );
    app.set_theme(theme.clone());
    app
}

/// What the host draws besides the app.
#[derive(Clone, Copy, Default)]
struct Host {
    /// A collaborator's pointer over the list, in host pixels.
    peer: Option<Point>,
    /// A badge drawn over the app outside every damage part.
    badge: bool,
    /// The tile drawn inside a translucent group.
    translucent: bool,
}

fn lay_out(tile: &mut EmbeddedApp, theme: &Theme) {
    let mut taffy = TaffyTree::new();
    let node = tile.build_layout(&mut taffy, theme, &mut HashMap::new());
    taffy
        .compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(REGION.width() as f32),
                height: taffy::AvailableSpace::Definite(REGION.height() as f32),
            },
        )
        .unwrap();
    tile.apply_layout(&taffy, node, REGION.origin());
}

/// The host's section, tracked for damage parts as the shell's are: a
/// wallpaper, the tile (its ground, its clip, the app, its edge), a
/// collaborator's mark and, outside every part, a badge.
fn section(
    tile: &mut EmbeddedApp,
    theme: &Theme,
    host: Host,
    text: &mut TextRenderer,
) -> (Box<Scene>, SectionParts) {
    let recording = crate::scene_parts::Recording::begin();
    let mut scene = Box::new(Scene::new());
    crate::scene_parts::track(&scene);
    let colors = &theme.tokens.colors;
    let wallpaper = Gradient::new_linear((0.0, 0.0), SCENE)
        .with_stops([colors.surfaces.content, theme.border_focus]);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &wallpaper,
        None,
        &KurboRect::new(0.0, 0.0, SCENE.0, SCENE.1),
    );
    let shape = RoundedRect::from_rect(REGION, 12.0);
    if host.translucent {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            0.9,
            Affine::IDENTITY,
            &KurboRect::new(0.0, 0.0, SCENE.0, SCENE.1),
        );
    }
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        colors.surfaces.content,
        None,
        &shape,
    );
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &shape,
    );
    crate::scene_parts::part(
        &mut scene,
        PartKey::new("test.tile-content", ()),
        vec![REGION],
        |scene| tile.draw(scene, Affine::IDENTITY, text, theme, &PresenceCtx::empty()),
    );
    scene.pop_layer();
    if host.translucent {
        scene.pop_layer();
    }
    // The edge is a band along the tile's outline, as a tile's is.
    let (outer, inner) = (REGION.inflate(2.0, 2.0), REGION.inflate(-14.0, -14.0));
    let bands = vec![
        KurboRect::new(outer.x0, outer.y0, outer.x1, inner.y0),
        KurboRect::new(outer.x0, inner.y1, outer.x1, outer.y1),
        KurboRect::new(outer.x0, inner.y0, inner.x0, inner.y1),
        KurboRect::new(inner.x1, inner.y0, outer.x1, inner.y1),
    ];
    crate::scene_parts::part(
        &mut scene,
        PartKey::new("test.tile-edge", ()),
        bands,
        |scene| {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                Affine::IDENTITY,
                theme.window_edge(),
                None,
                &shape,
            );
        },
    );
    crate::scene_parts::part_drawn(&mut scene, PartKey::new("test.peer", ()), |scene| {
        let Some(at) = host.peer else {
            return Vec::new();
        };
        let dot = Circle::new(at, 9.0);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.border_focus,
            None,
            &dot,
        );
        vec![KurboRect::from_center_size(at, (20.0, 20.0))]
    });
    if host.badge {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.border_focus,
            None,
            &RoundedRect::new(600.0, 420.0, 700.0, 460.0, 8.0),
        );
    }
    let parts = recording.finish().of(&scene).expect("well nested parts");
    (scene, parts)
}

/// What a run of scroll frames did.
#[derive(Debug, Default)]
struct Run {
    frames: usize,
    copied_frames: usize,
    /// Repainted device pixels of the frames that copied, summed.
    repainted: f64,
    /// Frames the host could not compare and repainted whole.
    whole: usize,
}

/// The device pixels `rects` cover, each once, in a `size` target.
fn covered(rects: &[KurboRect], size: (u32, u32)) -> f64 {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let target = KurboRect::new(0.0, 0.0, width as f64, height as f64);
    let mut cells = vec![false; width * height];
    for rect in rects {
        let rect = rect.intersect(target).expand();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        for y in rect.y0 as usize..rect.y1 as usize {
            cells[y * width + rect.x0 as usize..y * width + rect.x1 as usize].fill(true);
        }
    }
    cells.iter().filter(|cell| **cell).count() as f64
}

/// Scroll the app through `runs` of frames of `step` logical pixels each
/// (down for a positive step) with the pointer over the list, and present
/// each frame over the last at `scale`, as a host that copies does; every
/// few frames and after the last of a run, the presented pixels must equal a
/// full render.
fn scroll(theme: &Theme, host: Host, scale: f64, runs: &[(f64, usize)]) -> Vec<Run> {
    let mut tile = EmbeddedApp::new(list_app(theme), |_| {}).unwrap();
    tile.set_accepts_scroll_copies(true);
    let mut text = TextRenderer::new();
    lay_out(&mut tile, theme);
    let size = (
        (SCENE.0 * scale).round() as u32,
        (SCENE.1 * scale).round() as u32,
    );
    let to_device = Affine::scale(scale);
    let ground = theme.tokens.colors.surfaces.content;
    let device_scene = |section: &Scene| {
        let mut scene = Scene::new();
        scene.append(section, Some(to_device));
        scene
    };
    let pointer = WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(300.0, 200.0),
    };
    tile.app.surface_input(&tile.region, pointer, false);
    let mut shown = section(&mut tile, theme, host, &mut text);
    for _ in 0..6 {
        tile.wants_frame.set(true);
        shown = section(&mut tile, theme, host, &mut text);
    }
    let mut presented = OffscreenRenderer::try_new(size.0, size.1)
        .expect("a host's copies are checked on the GPU the toolkit renders with");
    let mut full = OffscreenRenderer::new(size.0, size.1);
    let _ = presented.render(&device_scene(&shown.0), ground);
    let mut bounds = tilo_cpu_raster::DrawBounds::default();
    let mut done = Vec::new();
    for &(step, frames) in runs {
        let mut run = Run::default();
        for frame in 0..frames {
            let wheel = WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: winit::event::MouseScrollDelta::PixelDelta(
                    winit::dpi::PhysicalPosition::new(0.0, -step),
                ),
                phase: winit::event::TouchPhase::Moved,
            };
            tile.app.surface_input(&tile.region, wheel, false);
            tile.wants_frame.set(true);
            let drawn = section(&mut tile, theme, host, &mut text);
            let scene = device_scene(&drawn.0);
            run.frames += 1;
            if let Ok(damage) = crate::scene_parts::damage_with_copies(
                &shown.0,
                &shown.1,
                &drawn.0,
                &drawn.1,
                &mut bounds,
            ) {
                let rects = damage
                    .rects
                    .iter()
                    .map(|rect| to_device.transform_rect_bbox(*rect).expand())
                    .collect();
                let plan =
                    crate::scene_parts::present_copies(&damage.copies, rects, &[], to_device, size);
                if !plan.copies.is_empty() {
                    run.copied_frames += 1;
                    run.repainted += covered(&plan.raster, size);
                }
                let _ = presented.render_copies_and_damage_gpu_wait(
                    &scene,
                    ground,
                    &plan.copies,
                    &plan.raster,
                );
            } else {
                run.whole += 1;
                let _ = presented.render(&scene, ground);
            }
            shown = drawn;
            if frame % 5 == 4 || frame + 1 == frames {
                let expected = full.render(&scene, ground);
                let actual = presented.readback_current();
                let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected);
                assert_eq!(
                    differing, 0,
                    "frame {frame} of a {step} px scroll at {scale}x presented {differing} pixels \
                 a full render does not have ({run:?})"
                );
            }
        }
        done.push(run);
    }
    done
}

type ThemeCase = (&'static str, fn() -> Theme);

const THEMES: [ThemeCase; 3] = [
    ("dark", Theme::dark),
    ("light", Theme::light),
    ("high contrast", Theme::high_contrast),
];

/// Most frames copy, and a frame that copies repaints the strip the scroll
/// exposed and what is drawn over the list, not the app.
fn assert_copies(run: &Run, scale: f64, label: &str) {
    assert_eq!(run.whole, 0, "{label}: every frame is compared ({run:?})");
    assert!(
        run.copied_frames * 4 >= run.frames * 3,
        "{label}: most frames copy ({run:?})"
    );
    let app = REGION.area() * scale * scale;
    let per_frame = run.repainted / run.copied_frames.max(1) as f64;
    assert!(
        per_frame < app * 0.3,
        "{label}: a frame that copies repaints {per_frame} of the app's {app} px ({run:?})"
    );
}

#[test]
fn a_scroll_in_an_embedded_app_copies_and_matches_a_full_render() {
    let host = Host {
        peer: Some(Point::new(320.0, 260.0)),
        ..Host::default()
    };
    for (name, theme) in THEMES {
        for scale in [1.0, 2.0] {
            let runs = scroll(&theme(), host, scale, &[(40.0, 20), (-24.0, 10)]);
            eprintln!("{name} at {scale}x: down {:?}, up {:?}", runs[0], runs[1]);
            assert_copies(&runs[0], scale, &format!("{name} {scale}x down"));
            assert_copies(&runs[1], scale, &format!("{name} {scale}x up"));
        }
    }
}

#[test]
fn what_the_host_draws_over_the_app_outside_its_parts_is_repainted_and_matches() {
    let host = Host {
        badge: true,
        ..Host::default()
    };
    let runs = scroll(&Theme::light(), host, 1.0, &[(40.0, 15)]);
    assert_copies(&runs[0], 1.0, "badge");
}

#[test]
fn a_step_of_part_of_a_device_pixel_repaints_and_matches() {
    let runs = scroll(&Theme::dark(), Host::default(), 1.0, &[(40.5, 10)]);
    assert_eq!(
        runs[0].copied_frames, 0,
        "half a pixel cannot be copied ({runs:?})"
    );
    // At 200 % the same half logical pixel is a whole device pixel, but the
    // app plans its copies a logical pixel apart: still repainted.
    let doubled = scroll(&Theme::dark(), Host::default(), 2.0, &[(40.5, 10)]);
    assert_eq!(doubled[0].whole, 0, "{doubled:?}");
}

#[test]
fn a_tile_under_a_translucent_layer_copies_nothing_and_matches() {
    let host = Host {
        translucent: true,
        ..Host::default()
    };
    let runs = scroll(&Theme::high_contrast(), host, 1.0, &[(40.0, 10)]);
    assert_eq!(runs[0].copied_frames, 0, "{runs:?}");
}

#[test]
fn a_host_that_does_not_accept_copies_gets_damage_only() {
    let theme = Theme::light();
    let mut tile = EmbeddedApp::new(list_app(&theme), |_| {}).unwrap();
    let mut text = TextRenderer::new();
    lay_out(&mut tile, &theme);
    for _ in 0..6 {
        tile.wants_frame.set(true);
        let _ = section(&mut tile, &theme, Host::default(), &mut text);
    }
    let wheel = WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
            0.0, -40.0,
        )),
        phase: winit::event::TouchPhase::Moved,
    };
    let pointer = WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(300.0, 200.0),
    };
    tile.app.surface_input(&tile.region, pointer, false);
    tile.app.surface_input(&tile.region, wheel, false);
    tile.wants_frame.set(true);
    let _ = section(&mut tile, &theme, Host::default(), &mut text);
    assert!(tile.painted_damage.copies.is_empty());
    assert!(
        tile.painted_damage.rects.is_some(),
        "the scroll is damage: {}",
        tile.damage_report_for_test()
    );
}
