//! A scroll in a [`VirtualList`] moves its rows without rebuilding them,
//! mounts the rows it brings near in a rebuild the frame's damage describes,
//! and offers the host a copy of the rows that stay on screen: presented
//! over the last frame (copies, then damage) as a window that copies
//! presents it, every frame equals a full render, in dark, light and high
//! contrast, at 100 % and 200 %, scrolling down and back up.

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
use crate::app::{App, AppSurface, SurfaceDamage};
use crate::offscreen::OffscreenRenderer;
use winit::event::MouseScrollDelta;

const ROW: f32 = 36.0;

/// One message: a lift every third row and its text, which reports the
/// damage it paints exactly (none once drawn).
struct Message {
    label: String,
    rect: Rect,
    drawn: bool,
}

impl Widget for Message {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: Dimension::length(ROW),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.label.ends_with('0') || self.label.ends_with('3') || self.label.ends_with('6') {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.intent.selection,
                None,
                &RoundedRect::from_rect(self.rect.inset(-3.0), 6.0),
            );
        }
        text.draw_text(
            scene,
            &self.label,
            15.0,
            theme.text_primary,
            transform.pre_translate(vello::kurbo::Vec2::new(
                self.rect.x0 + 12.0,
                self.rect.y0 + 9.0,
            )),
            None,
        );
        self.drawn = true;
    }

    fn name(&self) -> &'static str {
        "Message"
    }

    fn needs_redraw(&self) -> bool {
        !self.drawn
    }

    fn mark_drawn(&mut self) {}
}

fn transcript(theme: &Theme) -> App {
    let model = VecListModel::new((0..400_u32).collect(), |key| *key).unwrap();
    let list = VirtualList::new(model, ROW, |_, _, key| {
        Box::new(Message {
            label: format!("Message {key}: the quick brown fox jumps over the lazy dog"),
            rect: Rect::ZERO,
            drawn: false,
        })
    })
    .with_viewport_height(480.0);
    let mut app = App::new_with_controlled_inputs(crate::state::State::new(), Box::new(list));
    app.set_theme(theme.clone());
    app
}

struct Window {
    scale: f64,
}

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        self.scale
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(
            (640.0 * self.scale).round() as u32,
            (480.0 * self.scale).round() as u32,
        )
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

#[derive(Debug, Default)]
struct Run {
    frames: usize,
    copied_frames: usize,
    repainted: f64,
    whole: usize,
    why: Option<&'static str>,
}

/// The device pixels `rects` cover, each once, in a `size` window.
fn covered(rects: &[Rect], size: winit::dpi::PhysicalSize<u32>) -> f64 {
    let (width, height) = (size.width as usize, size.height as usize);
    let window = Rect::new(0.0, 0.0, width as f64, height as f64);
    let mut cells = vec![false; width * height];
    for rect in rects {
        let rect = rect.intersect(window).expand();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        for y in rect.y0 as usize..rect.y1 as usize {
            cells[y * width + rect.x0 as usize..y * width + rect.x1 as usize].fill(true);
        }
    }
    cells.iter().filter(|cell| **cell).count() as f64
}

fn scroll(theme: &Theme, scale: f64, runs: &[(f64, usize)]) -> Vec<Run> {
    let window = Window { scale };
    let size = window.inner_size();
    let mut app = transcript(theme);
    let background = theme.tokens.colors.surfaces.content;
    let mut damage = SurfaceDamage::copying();
    let mut scene = Scene::new();
    for at in 0..6 {
        // The pointer comes to rest over the laid-out list.
        if at == 3 {
            app.surface_input(
                &window,
                crate::synthetic_input::cursor_moved(200.0 * scale, 200.0 * scale),
                false,
            );
        }
        scene.reset();
        app.surface_frame_with_damage(&window, &mut scene, &mut damage);
    }
    let _ = damage.take_copying();
    let mut presented = OffscreenRenderer::try_new(size.width, size.height)
        .expect("a scroll's copies are checked on the GPU the toolkit renders with");
    let mut full = OffscreenRenderer::new(size.width, size.height);
    let _ = presented.render(&scene, background);
    let mut done = Vec::new();
    for &(step, frames) in runs {
        let mut run = Run::default();
        for frame in 0..frames {
            app.surface_input(
                &window,
                WindowEvent::MouseWheel {
                    device_id: winit::event::DeviceId::dummy(),
                    delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                        0.0,
                        -step * scale,
                    )),
                    phase: winit::event::TouchPhase::Moved,
                },
                false,
            );
            scene.reset();
            app.surface_frame_with_damage(&window, &mut scene, &mut damage);
            run.frames += 1;
            if let Some((copies, rects)) = damage.take_copying() {
                if !copies.is_empty() {
                    run.copied_frames += 1;
                    run.repainted += covered(&rects, size);
                }
                let _ = presented
                    .render_copies_and_damage_gpu_wait(&scene, background, &copies, &rects);
            } else {
                run.whole += 1;
                run.why = run.why.or_else(|| damage.repainted());
                let _ = presented.render(&scene, background);
            }
            if frame % 4 == 3 || frame + 1 == frames {
                let expected = full.render(&scene, background);
                let actual = presented.readback_current();
                let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected);
                assert_eq!(
                    differing, 0,
                    "frame {frame} of a {step} px scroll at {scale}x presented {differing} \
                     pixels a full render does not have ({run:?})"
                );
            }
        }
        done.push(run);
    }
    done
}

#[test]
fn scrolling_a_virtual_list_copies_the_rows_and_matches_a_full_render() {
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        for scale in [1.0, 2.0] {
            let runs = scroll(&theme, scale, &[(40.0, 16), (-24.0, 8)]);
            eprintln!("virtual list {name} {scale}x: {runs:?}");
            let window = 640.0 * 480.0 * scale * scale;
            for run in &runs {
                assert_eq!(run.whole, 0, "{name} {scale}x: {run:?}");
                assert!(
                    run.copied_frames * 4 >= run.frames * 3,
                    "{name} {scale}x: most frames copy ({run:?})"
                );
                let per_frame = run.repainted / run.copied_frames.max(1) as f64;
                assert!(
                    per_frame < window * 0.3,
                    "{name} {scale}x: a frame that copies repaints the exposed strip and the \
                     bar ({per_frame} of {window} px, {run:?})"
                );
            }
        }
    }
}

#[test]
fn a_step_of_part_of_a_device_pixel_repaints_the_list_and_matches() {
    let runs = scroll(&Theme::dark(), 1.0, &[(40.5, 8)]);
    assert_eq!(runs[0].copied_frames, 0, "{runs:?}");
}
