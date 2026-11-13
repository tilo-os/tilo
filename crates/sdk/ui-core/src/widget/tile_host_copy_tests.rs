//! A scroll in an app a window shows, drawn for a host that keeps what it presented and can move it
//! (the web's canvas, [`crate::scroll_copy::HostCopies`]): the window hands over what moved, the
//! host copies it ([`crate::retained_raster::RetainedFrames::repaint_copying`]) and repaints the
//! strip the scroll exposed, and every presented frame equals a full render of the scene, the desk
//! under the window included.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test scenes and pixel tallies: failures are the assertion"
)]

use super::*;
use crate::offscreen::OffscreenRenderer;
use crate::retained_raster::{Repaint, RetainedFrames, TargetKey};
use std::cell::Cell;
use std::rc::Rc;
use taffy::prelude::TaffyMaxContent;

/// The scene, in logical pixels.
const SCENE: (f64, f64) = (720.0, 540.0);
const WINDOW: Rect = Rect::new(40.0, 24.0, 680.0, 504.0);

/// An app that is one long list in a scroller, scrolled by `scroll` logical
/// pixels at its next layout.
struct List {
    inner: crate::widget::Scrollable,
    scroll: Rc<Cell<f32>>,
}

impl List {
    fn new(scroll: Rc<Cell<f32>>) -> Self {
        let mut column = crate::widget::Column::new().with_intrinsic_height();
        for row in 0..160 {
            column = column.push(Box::new(crate::widget::Text::new(&format!(
                "Row {row}: the quick brown fox jumps over the lazy dog"
            ))));
        }
        Self {
            inner: crate::widget::Scrollable::new_vertical().with_child(Box::new(column)),
            scroll,
        }
    }
}

impl Widget for List {
    crate::testing::delegate_widget_methods!(inner: build_layout, draw, get_rect);

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let step = self.scroll.replace(0.0);
        if step != 0.0 {
            self.inner.scroll_by(0.0, step);
        }
        self.inner.apply_layout(taffy, node, origin);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.inner]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.inner]
    }
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.inner.collect_damage(regions);
    }
    fn name(&self) -> &'static str {
        "List"
    }
}

crate::testing::test_app_widget!(List, "List");

#[derive(Debug, Default)]
struct Run {
    frames: usize,
    copied: usize,
    /// Device pixels repainted by the frames that copied, summed.
    repainted: f64,
    whole: usize,
}

/// The device pixels `rects` cover, each once, in a `size` target.
fn covered(rects: &[Rect], size: (u32, u32)) -> f64 {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let target = Rect::new(0.0, 0.0, width as f64, height as f64);
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

/// Scroll the window's list through `runs` of frames of `step` logical
/// pixels each (down for a positive step) at `scale`, presenting each frame
/// over the last as the web's canvas does; every few frames and after the
/// last of a run, what was presented must equal a full render.
fn scroll(theme: &Theme, scale: f64, runs: &[(f32, usize)]) -> Vec<Run> {
    let _settled = crate::motion::testing::settled();
    let scroll = Rc::new(Cell::new(0.0));
    let mut host = TileHost::new(TileContent::App(Box::new(List::new(Rc::clone(&scroll)))));
    let mut tree = TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    let tile = host.build_layout(&mut tree, theme, &mut nodes);
    let mut style = tree.style(tile).unwrap().clone();
    style.size = Size {
        width: Dimension::length(WINDOW.width() as f32),
        height: Dimension::length(WINDOW.height() as f32),
    };
    style.inset = taffy::Rect {
        left: taffy::LengthPercentageAuto::length(WINDOW.x0 as f32),
        top: taffy::LengthPercentageAuto::length(WINDOW.y0 as f32),
        right: taffy::LengthPercentageAuto::auto(),
        bottom: taffy::LengthPercentageAuto::auto(),
    };
    style.position = taffy::Position::Absolute;
    tree.set_style(tile, style).unwrap();
    let root = tree.parent_node(
        Style {
            size: Size {
                width: Dimension::length(SCENE.0 as f32),
                height: Dimension::length(SCENE.1 as f32),
            },
            ..Default::default()
        },
        &[tile],
    );
    let mut measure = TextRenderer::new();
    tree.compute_layout_with_measure(root, Size::MAX_CONTENT, |known, available, id, _, _| {
        nodes.get(&id).map_or(Size::ZERO, |&widget| {
            // SAFETY: every node maps to a widget of `host`, which outlives
            // the layout and is not otherwise borrowed while it runs.
            unsafe { (*widget).measure(known, available, &mut measure) }
        })
    })
    .unwrap();
    // The host draws for a target of `scale` pixels per unit of its scene.
    let _copies = crate::scroll_copy::HostCopies::begin_scaled(true, scale);
    let mut text = TextRenderer::new();
    let size = (
        (SCENE.0 * scale).round() as u32,
        (SCENE.1 * scale).round() as u32,
    );
    let key = TargetKey {
        width: size.0,
        height: size.1,
        to_device: Affine::scale(scale),
        base_color: theme.tokens.colors.surfaces.content,
        generation: 0,
    };
    let mut frame = |host: &mut TileHost| {
        host.apply_layout(&tree, tile, Point::ORIGIN);
        let recording = crate::scene_parts::Recording::begin();
        let mut scene = Scene::new();
        crate::scene_parts::track(&scene);
        // The desk under the window: something a copy must not move.
        scene.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            &vello::peniko::Gradient::new_linear((0.0, 0.0), SCENE)
                .with_stops([theme.border_focus, theme.tokens.colors.surfaces.content]),
            None,
            &Rect::new(0.0, 0.0, SCENE.0, SCENE.1),
        );
        host.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            &PresenceCtx::empty(),
        );
        host.mark_drawn();
        let parts = recording.finish().of(&scene).expect("well nested parts");
        let mut device = Scene::new();
        device.append(&scene, Some(key.to_device));
        (scene, parts, device)
    };
    let mut frames_seen = RetainedFrames::default();
    let mut presented = OffscreenRenderer::try_new(size.0, size.1)
        .expect("a host's copies are checked on the GPU the toolkit renders with");
    let mut full = OffscreenRenderer::new(size.0, size.1);
    let mut shown = frame(&mut host);
    for _ in 0..4 {
        shown = frame(&mut host);
    }
    let _ = presented.render(&shown.2, key.base_color);
    frames_seen.presented(&shown.0, Some(&shown.1), key);
    let mut done = Vec::new();
    for &(step, frames) in runs {
        let mut run = Run::default();
        for at in 0..frames {
            scroll.set(step);
            let drawn = frame(&mut host);
            run.frames += 1;
            let (repaint, copies) = frames_seen.repaint_copying(&drawn.0, Some(&drawn.1), key);
            match repaint {
                Repaint::Full(_) => {
                    run.whole += 1;
                    let _ = presented.render(&drawn.2, key.base_color);
                }
                Repaint::Regions(raster) => {
                    if !copies.is_empty() {
                        run.copied += 1;
                        run.repainted += covered(&raster, size);
                    }
                    let _ = presented.render_copies_and_damage_gpu_wait(
                        &drawn.2,
                        key.base_color,
                        &copies,
                        &raster,
                    );
                }
            }
            frames_seen.presented(&drawn.0, Some(&drawn.1), key);
            shown = drawn;
            if at % 4 == 3 || at + 1 == frames {
                let expected = full.render(&shown.2, key.base_color);
                let actual = presented.readback_current();
                let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected);
                assert_eq!(
                    differing, 0,
                    "frame {at} of a {step} px scroll at {scale}x presented {differing} pixels a \
                 full render does not have ({run:?})"
                );
            }
        }
        done.push(run);
    }
    done
}

/// A wheel scrolling a window's list copies what stays on screen and
/// repaints the strip it exposed, the bar and the band along the window's
/// edges and corners (which do not move with the list): well under the
/// window, in every theme and at 100 % and 200 %, scrolling down and back
/// up.
#[test]
fn a_scrolled_window_copies_what_stays_and_repaints_the_exposed_strip() {
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        for scale in [1.0, 2.0] {
            let runs = scroll(&theme, scale, &[(24.0, 12), (-16.0, 8)]);
            for (run, way) in runs.iter().zip(["down", "back up"]) {
                let label = format!("{name} at {scale}x, scrolling {way}");
                assert_eq!(run.whole, 0, "{label}: every frame is compared ({run:?})");
                assert!(
                    run.copied * 4 >= run.frames * 3,
                    "{label}: most frames copy ({run:?})"
                );
                let window = WINDOW.width() * WINDOW.height() * scale * scale;
                let per_frame = run.repainted / run.copied.max(1) as f64;
                assert!(
                    per_frame < window / 3.0,
                    "{label}: a frame that copies repaints {per_frame} device pixels, not the \
                     window's {window} ({run:?})"
                );
            }
        }
    }
}
