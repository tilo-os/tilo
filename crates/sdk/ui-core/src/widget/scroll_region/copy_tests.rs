//! A painted host that scrolls through a [`ScrollRegion`] offers its scrolls
//! as copies ([`ScrollRegion::offer_copy`]): presented over the last frame
//! (copies, then damage) as a window that copies presents it, every frame
//! equals a full render, in dark, light and high contrast, at 100 % and
//! 200 %, with and without edge fades.

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
use crate::widget::Widget;

/// A transcript painted line by line inside a scroll region, as a chat or
/// a calculator's history paints its content.
struct Transcript {
    region: ScrollRegion,
    rect: Rect,
    /// Something the host paints changed besides the offset.
    dirty: bool,
    /// The offset moved since the frame on screen.
    scrolled: bool,
}

const LINES: usize = 120;
const LINE: f64 = 28.0;

impl Widget for Transcript {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        crate::widget::TaffyTreeExt::fill_leaf(taffy, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        self.region.set_viewport(self.rect, theme);
        self.region
            .set_content_size(self.rect.width(), LINES as f64 * LINE);
        let visible = self.region.visible_content();
        self.region
            .paint(scene, transform, theme, |scene, content| {
                for line in 0..LINES {
                    let top = line as f64 * LINE;
                    if top + LINE < visible.y0 || top > visible.y1 {
                        continue;
                    }
                    if line % 3 == 0 {
                        scene.fill(
                            Fill::NonZero,
                            content,
                            theme.tokens.colors.intent.selection,
                            None,
                            &RoundedRect::new(8.0, top + 2.0, 300.0, top + LINE - 2.0, 6.0),
                        );
                    }
                    text.draw_text(
                        scene,
                        &format!("Line {line}: the quick brown fox jumps over the lazy dog"),
                        15.0,
                        theme.text_primary,
                        content.pre_translate(Vec2::new(16.0, top + 6.0)),
                        None,
                    );
                }
            });
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let before = self.region.offset();
        let theme = Theme::dark();
        if self.region.handle_event(event, ctx, &theme) {
            if self.region.offset() == before {
                self.dirty = true;
            } else {
                self.scrolled = true;
            }
        }
    }

    fn name(&self) -> &'static str {
        "Transcript"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.scrolled
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if !self.needs_redraw() {
            return;
        }
        if !self.dirty && self.region.offer_copy(self, regions) {
            return;
        }
        regions.push(self.rect);
    }

    fn mark_drawn(&mut self) {
        self.region.mark_presented();
        self.dirty = false;
        self.scrolled = false;
    }
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
}

fn scroll(theme: &Theme, scale: f64, edge_fade: bool, runs: &[(f64, usize)]) -> Vec<Run> {
    let window = Window { scale };
    let size = window.inner_size();
    let region = if edge_fade {
        ScrollRegion::vertical().with_edge_fade()
    } else {
        ScrollRegion::vertical()
    };
    let mut app = App::new_with_controlled_inputs(
        crate::state::State::new(),
        Box::new(Transcript {
            region,
            rect: Rect::ZERO,
            dirty: true,
            scrolled: false,
        }),
    );
    app.set_theme(theme.clone());
    let background = theme.tokens.colors.surfaces.content;
    let mut damage = SurfaceDamage::copying();
    let mut scene = Scene::new();
    app.surface_input(
        &window,
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(200.0 * scale, 200.0 * scale),
        },
        false,
    );
    for _ in 0..5 {
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
                    run.repainted += rects.iter().map(Rect::area).sum::<f64>();
                }
                let _ = presented
                    .render_copies_and_damage_gpu_wait(&scene, background, &copies, &rects);
            } else {
                run.whole += 1;
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
fn a_painted_host_scrolling_its_region_copies_and_matches_a_full_render() {
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        for scale in [1.0, 2.0] {
            for edge_fade in [false, true] {
                let runs = scroll(&theme, scale, edge_fade, &[(40.0, 12), (-24.0, 8)]);
                eprintln!("scroll region {name} {scale}x fade={edge_fade}: {runs:?}");
                let window = 640.0 * 480.0 * scale * scale;
                for run in &runs {
                    assert!(
                        run.copied_frames * 4 >= run.frames * 3,
                        "{name} {scale}x fade={edge_fade}: most frames copy ({run:?})"
                    );
                    let per_frame = run.repainted / run.copied_frames.max(1) as f64;
                    assert!(
                        per_frame < window * 0.3,
                        "{name} {scale}x fade={edge_fade}: a frame that copies repaints the \
                         exposed strip, the bar and the fades ({per_frame} of {window} px)"
                    );
                }
            }
        }
    }
}

#[test]
fn a_step_of_part_of_a_device_pixel_repaints_the_region_and_matches() {
    let runs = scroll(&Theme::dark(), 1.0, false, &[(40.5, 8)]);
    assert_eq!(runs[0].copied_frames, 0, "{runs:?}");
}
