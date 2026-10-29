//! A pan in a [`DocumentViewport`] drawn as layers
//! ([`crate::strip_layers::LayerRecording`]) and presented from retained
//! layer textures, as an app's render worker presents it: the backdrop is
//! never rasterised again, the content only where the pan uncovered it, the
//! chrome only where it changed, and every frame captured from the target
//! equals a full render of the frame.

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
use crate::strip_layers::{
    ChromeDamage, LayerPresenter, LayerRecording, QueuedRaster, WorkspaceWork, tiles,
};
use vello::wgpu;

struct Window;

const SIZE: (u32, u32) = (640, 480);

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(SIZE.0, SIZE.1)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

/// A tall canvas of coloured cells in a document viewport.
fn canvas(theme: &Theme) -> App {
    let viewport = DocumentViewport::from_draw(
        400.0,
        60.0 * 44.0,
        Box::new(
            |scene: &mut Scene, transform: Affine, _: &mut TextRenderer, _: &Theme, _: Rect| {
                for row in 0..60_u8 {
                    for column in 0..8_u8 {
                        let color = vello::peniko::Color::from_rgb8(
                            row.wrapping_mul(37),
                            column.wrapping_mul(53),
                            row.wrapping_add(column).wrapping_mul(11),
                        );
                        let cell = Rect::from_origin_size(
                            (f64::from(column) * 50.0, f64::from(row) * 44.0),
                            (46.0, 40.0),
                        );
                        scene.fill(Fill::NonZero, transform, color, None, &cell);
                    }
                }
            },
        ),
    );
    let mut app = App::new_with_controlled_inputs(crate::state::State::new(), Box::new(viewport));
    app.set_theme(theme.clone());
    app
}

/// A tall sheet of switches in a document viewport: content whose widgets
/// report exactly what they change, so the frame's damage bounds where the
/// chrome changed.
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

fn wheel(app: &mut App, dy: f64) {
    app.surface_input(
        &Window,
        WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, dy)),
            phase: winit::event::TouchPhase::Moved,
        },
        false,
    );
}

/// The output's pixels, RGBA rows.
fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let (width, height) = SIZE;
    let row = width * 4;
    let padded =
        row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("layers-test-readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |result| result.expect("readback maps"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the readback completes");
    let mapped = slice.get_mapped_range();
    mapped
        .chunks_exact(padded as usize)
        .flat_map(|line| &line[..row as usize])
        .copied()
        .collect()
}

#[test]
fn a_pan_drawn_as_layers_copies_the_content_and_keeps_the_rest() {
    let theme = Theme::dark();
    // A canvas drawn by a closure reports no damage of its own: its pans
    // still copy the content, and rasterise the chrome over the window.
    pan_as_layers(&theme, canvas(&theme), false);
    // Widgets bound what changes: the chrome is rasterised only there.
    pan_as_layers(&theme, sheet(&theme), true);
}

/// Pan `app` frame by frame, drawn as layers and presented from them on the
/// GPU, checking each frame's work and its capture against a full render.
fn pan_as_layers(theme: &Theme, mut app: App, bounded: bool) {
    let background = theme.tokens.colors.surfaces.content;
    let mut damage = SurfaceDamage::copying();
    let mut scene = Scene::new();
    app.surface_input(
        &Window,
        crate::synthetic_input::cursor_moved(200.0, 200.0),
        false,
    );
    for _ in 0..3 {
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    }
    let _ = damage.take_copying();

    let mut context = crate::gpu::RenderContext::new();
    let index = pollster::FutureExt::block_on(context.device(None))
        .expect("layer presenting is checked on a real adapter");
    let gpu = &context.devices[index];
    let (device, queue) = (&gpu.device, &gpu.queue);
    let mut renderer = vello::Renderer::new(
        device,
        vello::RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .expect("a renderer");
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("layers-test-output"),
        size: wgpu::Extent3d {
            width: SIZE.0,
            height: SIZE.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = output.create_view(&wgpu::TextureViewDescriptor::default());
    let mut full = OffscreenRenderer::new(SIZE.0, SIZE.1);
    let mut presenter = LayerPresenter::default();
    let window = u64::from(SIZE.0) * u64::from(SIZE.1);
    let mut holds = None;
    for frame in 0..12_u64 {
        wheel(&mut app, -24.0);
        let recording = LayerRecording::begin();
        scene.reset();
        app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
        let chrome_damage = damage.take_copying().map(|(_, rects)| ChromeDamage {
            since: frame,
            rects: tiles(&rects, SIZE),
        });
        let cache = recording
            .finish(
                &mut scene,
                SIZE.0,
                frame + 1,
                chrome_damage.filter(|_| frame > 0),
            )
            .expect("the viewport's content is drawn as the frame's strip");
        assert_eq!(cache.strips[0].scroll_y, 24 * (frame as u32 + 1));
        presenter
            .present(
                &mut QueuedRaster {
                    device,
                    queue,
                    renderer: &mut renderer,
                    output: &view,
                    size: SIZE,
                    base_color: background,
                },
                &cache,
                frame,
                holds,
            )
            .expect("the layers present");
        holds = Some(frame);
        let work = presenter.last_work().expect("a frame's work").clone();
        if frame > 0 {
            assert!(
                !work.backdrop,
                "frame {frame}: the backdrop is kept ({work:?})"
            );
            let WorkspaceWork::Pan { copies, uncovered } = &work.workspace else {
                panic!("frame {frame}: the content pans ({work:?})");
            };
            assert!(
                !copies.is_empty(),
                "frame {frame}: the pan copies ({work:?})"
            );
            let uncovered: u64 = uncovered
                .iter()
                .map(|rect| u64::from(rect.width) * u64::from(rect.height))
                .sum();
            assert!(
                uncovered <= u64::from(SIZE.0) * 24,
                "frame {frame}: only the uncovered rows are rasterised ({work:?})"
            );
            let (raster, _) = presenter.last_pixels(SIZE).expect("counted");
            assert!(
                !bounded || raster * 4 < window,
                "frame {frame}: {raster} of {window} pixels rasterised ({work:?})"
            );
        }
        let expected = full.render(&scene, background);
        let actual = read(device, queue, &output);
        let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected);
        assert_eq!(
            differing, 0,
            "frame {frame}: the composited capture differs from a full render in {differing} pixels"
        );
    }
}
