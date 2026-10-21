//! A window's renderer builds only what its first frame dispatches
//! (`vello::Renderer::new_deferring`); these pin that it still draws exactly
//! what an eagerly built renderer draws, whichever pipelines a frame needs.

use vello::kurbo::{Affine, Circle, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill, Mix};
use vello::wgpu;
use vello::{AaConfig, PipelineDeferral, RenderParams, Renderer, Scene};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 160;

fn options() -> vello::RendererOptions {
    vello::RendererOptions {
        use_cpu: false,
        antialiasing_support: vello::AaSupport::area_only(),
        num_init_threads: std::num::NonZeroUsize::new(2),
        pipeline_cache: None,
    }
}

/// A window's ordinary content: panels, text-sized shapes, a stroke and a
/// clipped scroll area.
fn window_scene() -> Scene {
    let mut scene = Scene::new();
    let bounds = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT));
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(28, 30, 38),
        None,
        &bounds,
    );
    scene.push_layer(
        Fill::NonZero,
        Mix::Normal,
        1.0,
        Affine::IDENTITY,
        &bounds.inset(-8.0),
    );
    for row in 0..8_u8 {
        let y = 10.0 + f64::from(row) * 18.0;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(60 + row * 10, 64, 90),
            None,
            &RoundedRect::new(12.0, y, 240.0, y + 14.0, 4.0),
        );
    }
    scene.pop_layer();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(220, 90, 70),
        None,
        &Circle::new((200.0, 120.0), 24.0),
    );
    scene.stroke(
        &Stroke::new(1.5),
        Affine::IDENTITY,
        Color::from_rgb8(230, 230, 240),
        None,
        &RoundedRect::new(4.0, 4.0, 252.0, 156.0, 8.0),
    );
    scene
}

/// More clips than one clip-reduce workgroup holds, so the frame dispatches
/// the clip reduce, which a deferring renderer does not build up front.
fn many_clips_scene() -> Scene {
    let mut scene = Scene::new();
    for index in 0..320_u32 {
        let x = f64::from(index % 32) * 8.0;
        let y = f64::from(index / 32) * 16.0;
        let cell = Rect::new(x, y, x + 8.0, y + 16.0);
        scene.push_layer(Fill::NonZero, Mix::Normal, 1.0, Affine::IDENTITY, &cell);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(u8::try_from(index % 256).unwrap(), 120, 200),
            None,
            &Circle::new((x + 4.0, y + 8.0), 6.0),
        );
        scene.pop_layer();
    }
    scene
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

fn target(device: &wgpu::Device) -> Target {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("deferred-pipelines-test-target"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Target { texture, view }
}

fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let row = (WIDTH * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("deferred-pipelines-test-readback"),
        size: u64::from(row * HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the readback completes");
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for line in mapped.chunks(row as usize).take(HEIGHT as usize) {
        pixels.extend_from_slice(&line[..(WIDTH * 4) as usize]);
    }
    pixels
}

fn draw(gpu: &crate::offscreen::SharedGpu, renderer: &mut Renderer, scene: &Scene) -> Vec<u8> {
    let target = target(&gpu.device);
    renderer
        .render_to_texture(
            &gpu.device,
            &gpu.queue,
            scene,
            &target.view,
            &RenderParams {
                base_color: Color::from_rgb8(10, 10, 12),
                width: WIDTH,
                height: HEIGHT,
                antialiasing_method: AaConfig::Area,
            },
        )
        .expect("the scene renders");
    read(&gpu.device, &gpu.queue, &target.texture)
}

fn assert_same(what: &str, got: &[u8], expected: &[u8]) {
    let differing = crate::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(got, expected);
    assert_eq!(
        differing, 0,
        "{what}: {differing} pixels differ from the eager renderer"
    );
}

#[test]
fn a_window_s_first_frame_needs_none_of_the_pipelines_it_defers() {
    let gpu = crate::offscreen::shared_gpu().expect("shared GPU context");
    let mut eager = Renderer::new(&gpu.device, options()).unwrap();
    assert_eq!(
        eager.deferred_pipelines().pending,
        0,
        "Renderer::new builds every pipeline"
    );
    let window = window_scene();
    let clips = many_clips_scene();
    let expected_window = draw(&gpu, &mut eager, &window);
    let expected_clips = draw(&gpu, &mut eager, &clips);

    let mut deferring =
        Renderer::new_deferring(&gpu.device, options(), PipelineDeferral::Unused).unwrap();
    let deferred = deferring.deferred_pipelines().pending;
    assert!(deferred > 0, "the default path's spare pipelines wait");
    assert_same(
        "a window frame",
        &draw(&gpu, &mut deferring, &window),
        &expected_window,
    );
    assert_eq!(
        deferring.deferred_pipelines(),
        vello::DeferredPipelines {
            pending: deferred,
            built_inline: 0
        },
        "a full frame on the default path dispatches no deferred pipeline"
    );

    // A frame that does need one builds it before encoding.
    assert_same(
        "many clips",
        &draw(&gpu, &mut deferring, &clips),
        &expected_clips,
    );
    let after_clips = deferring.deferred_pipelines();
    assert!(
        after_clips.built_inline >= 1 && after_clips.pending < deferred,
        "the clip reduce was built for the frame that dispatched it: {after_clips:?}"
    );

    // Started, the background thread builds the rest; frames install them.
    let (built, finished) = std::sync::mpsc::channel();
    deferring.start_deferred_pipelines(&gpu.device, move || {
        let _ = built.send(());
    });
    let started = crate::time::now();
    while deferring.deferred_pipelines().pending > 0 {
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "the deferred pipelines never arrived"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(5));
    }
    finished
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the builder reports it built every deferred pipeline");
    assert_same(
        "after the build",
        &draw(&gpu, &mut deferring, &clips),
        &expected_clips,
    );
    assert_eq!(
        deferring.deferred_pipelines().built_inline,
        after_clips.built_inline,
        "once built in the background, nothing is built inline again"
    );
}

#[test]
fn a_renderer_that_defers_everything_builds_what_a_frame_dispatches() {
    let gpu = crate::offscreen::shared_gpu().expect("shared GPU context");
    let mut eager = Renderer::new(&gpu.device, options()).unwrap();
    let window = window_scene();
    let expected = draw(&gpu, &mut eager, &window);

    let mut lazy = Renderer::new_deferring(&gpu.device, options(), PipelineDeferral::All).unwrap();
    let unused = Renderer::new_deferring(&gpu.device, options(), PipelineDeferral::Unused)
        .unwrap()
        .deferred_pipelines()
        .pending;
    let pending = lazy.deferred_pipelines().pending;
    assert!(pending > unused, "everything waits: {pending} > {unused}");
    assert_same("a window frame", &draw(&gpu, &mut lazy, &window), &expected);
    let after = lazy.deferred_pipelines();
    assert!(
        after.built_inline > 0
            && after.pending + usize::try_from(after.built_inline).unwrap() == pending,
        "the frame built exactly what it dispatched: {after:?}"
    );
}

/// What a window on a software adapter does: everything deferred, the build
/// started at once. A first frame drawn meanwhile waits for the build rather
/// than compiling pipelines itself, and draws what an eager renderer draws.
#[test]
fn a_build_started_at_once_serves_the_first_frame() {
    let gpu = crate::offscreen::shared_gpu().expect("shared GPU context");
    let mut eager = Renderer::new(&gpu.device, options()).unwrap();
    let window = window_scene();
    let expected = draw(&gpu, &mut eager, &window);

    let mut early = Renderer::new_deferring(&gpu.device, options(), PipelineDeferral::All).unwrap();
    let (built, finished) = std::sync::mpsc::channel();
    early.start_deferred_pipelines(&gpu.device, move || {
        let _ = built.send(());
    });
    assert_same(
        "a window frame",
        &draw(&gpu, &mut early, &window),
        &expected,
    );
    assert_eq!(
        early.deferred_pipelines().built_inline,
        0,
        "with the build running, a frame waits for it and builds nothing itself"
    );
    finished
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the build reports it built every deferred pipeline");
    assert_eq!(early.deferred_pipelines().pending, 0);
    assert_same(
        "many clips after the build",
        &draw(&gpu, &mut early, &many_clips_scene()),
        &draw(&gpu, &mut eager, &many_clips_scene()),
    );
}

/// The web's renderer (`Renderer::new_async_deferring`): only the default
/// path's pipelines are awaited, the rest compile asynchronously once the
/// first frame is out. A frame that dispatches one before its compile is in
/// creates it itself; none is ever drawn without its pipeline, and every
/// frame draws what an eager renderer draws.
#[test]
fn the_web_renderer_awaits_only_the_first_frame_s_pipelines() {
    let gpu = crate::offscreen::shared_gpu().expect("shared GPU context");
    let mut eager = Renderer::new(&gpu.device, options()).unwrap();
    let window = window_scene();
    let clips = many_clips_scene();
    let expected_window = draw(&gpu, &mut eager, &window);
    let expected_clips = draw(&gpu, &mut eager, &clips);

    let mut web = pollster::block_on(Renderer::new_async_deferring(
        &gpu.device,
        options(),
        PipelineDeferral::Unused,
    ))
    .unwrap();
    let deferred = web.deferred_pipelines().pending;
    assert_eq!(
        deferred,
        Renderer::new_deferring(&gpu.device, options(), PipelineDeferral::Unused)
            .unwrap()
            .deferred_pipelines()
            .pending,
        "the web defers what a native window defers"
    );
    assert!(deferred > 0);
    assert_same(
        "a first frame",
        &draw(&gpu, &mut web, &window),
        &expected_window,
    );
    assert_eq!(web.deferred_pipelines().built_inline, 0);

    // Right after the first frame, before any compile is in: a frame that
    // dispatches a deferred pipeline creates it.
    assert_same("many clips", &draw(&gpu, &mut web, &clips), &expected_clips);
    let after_clips = web.deferred_pipelines();
    assert!(
        after_clips.built_inline >= 1 && after_clips.pending < deferred,
        "the clip reduce was created for the frame that dispatched it: {after_clips:?}"
    );

    web.start_deferred_pipelines_async(&gpu.device);
    web.start_deferred_pipelines_async(&gpu.device);
    let started = crate::time::now();
    while web.deferred_pipelines().pending > 0 {
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "the deferred pipelines never arrived"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(5));
    }
    assert_same(
        "after the compiles",
        &draw(&gpu, &mut web, &clips),
        &expected_clips,
    );
    assert_eq!(
        web.deferred_pipelines().built_inline,
        after_clips.built_inline,
        "compiled asynchronously, nothing is created inline again"
    );
}
