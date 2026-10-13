//! A pan in a [`DocumentViewport`] offers the host a copy of what stays on
//! screen: presented over the last frame (copies, then damage) as a window
//! that copies presents it, every frame equals a full render, in dark,
//! light and high contrast, at 100 % and 200 %.

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

/// The window: 640×480 logical pixels at `scale`.
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

/// A tall sheet of switches in a document viewport: content whose widgets
/// report exactly what they change.
fn sheet(theme: &Theme) -> App {
    let mut column = crate::widget::Column::new().with_intrinsic_height();
    for row in 0..60 {
        column = column.push(Box::new(crate::widget::Toggle::new(
            &format!("switch-{row}"),
            "",
        )));
    }
    let mut viewport = DocumentViewport::new(Box::new(column));
    viewport.set_content_size(400.0, 60.0 * 44.0);
    let mut app = App::new_with_controlled_inputs(crate::state::State::new(), Box::new(viewport));
    app.set_theme(theme.clone());
    app
}

#[derive(Debug, Default)]
struct Run {
    frames: usize,
    copied_frames: usize,
    repainted: f64,
    whole: usize,
}

fn area(rects: &[Rect]) -> f64 {
    rects.iter().map(Rect::area).sum()
}

/// Pan the sheet `frames` frames of `step` logical pixels, presenting each
/// frame over the last on the GPU, and check it against a full render.
fn pan(theme: &Theme, scale: f64, (step, frames): (f64, usize)) -> Run {
    let window = Window { scale };
    let size = window.inner_size();
    let mut app = sheet(theme);
    let background = theme.tokens.colors.surfaces.content;
    let mut damage = SurfaceDamage::copying();
    let mut scene = Scene::new();
    app.surface_input(
        &window,
        crate::synthetic_input::cursor_moved(200.0 * scale, 200.0 * scale),
        false,
    );
    for _ in 0..5 {
        scene.reset();
        app.surface_frame_with_damage(&window, &mut scene, &mut damage);
    }
    let _ = damage.take_copying();
    let mut presented = OffscreenRenderer::try_new(size.width, size.height)
        .expect("a pan's copies are checked on the GPU the toolkit renders with");
    let mut full = OffscreenRenderer::new(size.width, size.height);
    let _ = presented.render(&scene, background);
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
                run.repainted += area(&rects);
            }
            let _ =
                presented.render_copies_and_damage_gpu_wait(&scene, background, &copies, &rects);
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
                "frame {frame} of a {step} px pan at {scale}x presented {differing} pixels a \
                 full render does not have ({run:?})"
            );
        }
    }
    run
}

#[test]
fn a_pan_in_a_document_viewport_copies_and_matches_a_full_render() {
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        for scale in [1.0, 2.0] {
            let run = pan(&theme, scale, (40.0, 16));
            eprintln!("document viewport {name} at {scale}x: {run:?}");
            assert!(
                run.copied_frames * 4 >= run.frames * 3,
                "{name} {scale}x: most frames copy ({run:?})"
            );
            let window = 640.0 * 480.0 * scale * scale;
            let per_frame = run.repainted / run.copied_frames.max(1) as f64;
            assert!(
                per_frame < window * 0.3,
                "{name} {scale}x: a frame that copies repaints the exposed strip and the \
                 bars, not the viewport ({per_frame} of {window} px, {run:?})"
            );
        }
    }
}
