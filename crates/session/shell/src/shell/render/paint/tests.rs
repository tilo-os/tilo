//! Renderer gates require an actual wgpu adapter on every selected host.
use super::*;

use pollster::FutureExt;
use settings_store::hardware::OutputTransform;
use vello::{AaConfig, RenderParams, Scene};

use crate::shell::frame::FrameBuilder;
use crate::shell::render::icc_lut::IccLut;
use crate::shell::render::xrgb_history::XrgbWritePlan;
use crate::shell::test_support::PopulatedShellFixture;
use automerge::{AutoCommit, ROOT};
use ui::taffy::NodeId;
use vello::kurbo::Affine;
use vello::peniko::Fill;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

mod direct;
mod multi_output;
mod output_views;
mod recovery;
mod sparse_damage;
mod xrgb_history;

fn required_bridge(w: u32, h: u32) -> VelloDrmBridge {
    let bridge = VelloDrmBridge::new(w, h).expect("renderer gate requires a real wgpu adapter");
    let adapter = bridge.adapter_info().expect("bridge adapter identity");
    assert!(!adapter.name.is_empty());
    assert!(!adapter.backend.is_empty());
    assert!(!adapter.device_type.is_empty());
    eprintln!("renderer gate adapter: {adapter:?}");
    bridge
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> vello::FineDamageRect {
    vello::FineDamageRect {
        x,
        y,
        width,
        height,
    }
}

fn rect_scene(w: u32, h: u32, color: Color) -> Scene {
    let mut scene = Scene::new();
    let rect = vello::kurbo::Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
    scene
}

fn required_target(width: u32, height: u32) -> RetainedGpuTarget {
    // Device and renderer creation read renderer controls from the
    // environment; hold the lock so a sibling test's override of them
    // cannot leak into this target.
    let _env = tilo_test_env::lock();
    let mut context = RenderContext::new();
    let index = context
        .device(None)
        .block_on()
        .expect("target tests require a real adapter");
    eprintln!(
        "target adapter: {:?}",
        context.devices[index].adapter.get_info()
    );
    RetainedGpuTarget::new(&context.devices[index], width, height).expect("target")
}

/// A target created while `key` is `value`; the override ends once the
/// renderer has read it.
fn required_target_with_env(key: &str, value: &str, width: u32, height: u32) -> RetainedGpuTarget {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(key, value);
    required_target(width, height)
}

fn completion_clock() -> CompletionTest {
    CompletionTest {
        start: Instant::now(),
        elapsed: Duration::ZERO,
        encode_elapsed: Duration::ZERO,
        fence_elapsed: Duration::ZERO,
        callback_elapsed: Duration::ZERO,
        pending_elapsed: Duration::from_millis(200),
        waits: Vec::new(),
        counter_remaining: Vec::new(),
        counter_waits: Vec::new(),
        counters: std::collections::VecDeque::new(),
        failure: None,
        destroy_at_marker: false,
        disconnect_map_callback: false,
    }
}

fn version(
    revision: u64,
    previous_revision: Option<u64>,
    regions: Option<&[vello::FineDamageRect]>,
) -> SceneDamage<'_> {
    SceneDamage {
        revision,
        previous_revision,
        regions,
    }
}

fn target_pixels(target: &RetainedGpuTarget) -> Vec<u8> {
    assert!(
        target.completed_view().is_some(),
        "test-only framebuffer readback must follow completed render_until"
    );
    let (buffer, stride) =
        VelloDrmBridge::allocate_staging(&target.device, target.size.0, target.size.1);
    let mut encoder = target
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    VelloDrmBridge::append_full_copy(
        &mut encoder,
        &target.texture,
        &buffer,
        stride,
        target.size.0,
        target.size.1,
    );
    target.queue.submit([encoder.finish()]);
    let mapped = VelloDrmBridge::map_device_readback(&target.device, &buffer)
        .expect("explicit post-return pixel oracle");
    let mut result = Vec::new();
    for row in mapped.chunks(stride as usize).take(target.size.1 as usize) {
        result.extend_from_slice(
            &row[..(target.size.0 as usize)
                .checked_mul(4)
                .expect("target row fits usize")],
        );
    }
    drop(mapped);
    buffer.unmap();
    result
}

fn region_scene(rect: vello::FineDamageRect, color: Color) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        color,
        None,
        &vello::kurbo::Rect::new(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(
                rect.x
                    .checked_add(rect.width)
                    .expect("test rectangle x extent"),
            ),
            f64::from(
                rect.y
                    .checked_add(rect.height)
                    .expect("test rectangle y extent"),
            ),
        ),
    );
    scene
}

/// 8,192 overlapping fills confined to one native-pixel damage rectangle. 512 fills fit the
/// 96×96 sparse tile reservation, so this calibrated count is intentionally high enough to
/// exercise a genuine sparse bump overflow and the bounded dense recovery.
fn opaque_rect_stack_in_rect_scene(rect: vello::FineDamageRect) -> Scene {
    let mut scene = Scene::new();
    let bounds = vello::kurbo::Rect::new(
        f64::from(rect.x),
        f64::from(rect.y),
        f64::from(rect.x.saturating_add(rect.width)),
        f64::from(rect.y.saturating_add(rect.height)),
    );
    for index in 0..8_192_u32 {
        let red = u8::try_from(index & 0xFF).unwrap_or(u8::MAX);
        let green = u8::try_from((index.saturating_mul(3)) & 0xFF).unwrap_or(u8::MAX);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(red, green, 0xA5),
            None,
            &bounds,
        );
    }
    scene
}

/// A frame that renders in full.
fn full_frame(revision: u64) -> SceneDamage<'static> {
    SceneDamage {
        revision,
        previous_revision: None,
        regions: None,
    }
}

/// One output's frame through the production per-output path, into an offscreen
/// stand-in for its imported scanout target, read back as BGRA bytes.
fn paint_output_pixels(
    bridge: &mut VelloDrmBridge,
    scene: &Scene,
    view: &OutputView,
    revision: u64,
) -> Vec<u8> {
    paint_output_damage(bridge, scene, view, full_frame(revision)).1
}

/// [`paint_output_pixels`] for a frame carrying `damage`, with the frame's stats.
fn paint_output_damage(
    bridge: &mut VelloDrmBridge,
    scene: &Scene,
    view: &OutputView,
    damage: SceneDamage<'_>,
) -> (GpuFrameStats, Vec<u8>) {
    let (texture, stats) = paint_output_offscreen(bridge, scene, view, damage);
    (stats, read_target(bridge, &texture))
}

/// 512 individually-recorded opaque full-target fills. Their overlapping coverage overflows
/// tiles in the initial 384×256 reservation, exercising bounded recovery.
fn opaque_rect_stack_scene(w: u32, h: u32) -> Scene {
    let mut scene = Scene::new();
    let rect = vello::kurbo::Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
    for index in 0..512_u32 {
        let red = u8::try_from(index & 0xFF).unwrap_or(u8::MAX);
        let green = u8::try_from((index.saturating_mul(3)) & 0xFF).unwrap_or(u8::MAX);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(red, green, 0xA5),
            None,
            &rect,
        );
    }
    scene
}

/// The bytes of a 4-byte-per-pixel scanout stand-in, rows tight.
fn read_target(bridge: &VelloDrmBridge, texture: &wgpu::Texture) -> Vec<u8> {
    let (width, height) = (texture.width(), texture.height());
    let (device, queue) = bridge.gpu();
    let (device, queue) = (device.clone(), queue.clone());
    let (buffer, stride) = VelloDrmBridge::allocate_staging(&device, width, height);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    VelloDrmBridge::append_full_copy(&mut encoder, texture, &buffer, stride, width, height);
    queue.submit([encoder.finish()]);
    let mapped =
        VelloDrmBridge::map_device_readback(&device, &buffer).expect("scanout stand-in maps");
    let mut pixels = Vec::new();
    for row in mapped.chunks(stride as usize).take(height as usize) {
        pixels.extend_from_slice(&row[..(width as usize).saturating_mul(4)]);
    }
    drop(mapped);
    buffer.unmap();
    pixels
}

/// One output's frame through the production per-output path into an offscreen
/// stand-in for its imported scanout target, which is returned unread.
fn paint_output_offscreen(
    bridge: &mut VelloDrmBridge,
    scene: &Scene,
    view: &OutputView,
    damage: SceneDamage<'_>,
) -> (wgpu::Texture, GpuFrameStats) {
    paint_output_offscreen_as(
        bridge,
        scene,
        view,
        damage,
        (wgpu::TextureFormat::Bgra8Unorm, None),
    )
}

/// [`paint_output_offscreen`] into a stand-in of `format`: `Bgra8Unorm` for an SDR
/// scanout, `Rgb10a2Unorm` for an `XBGR2101010` HDR one; through `color_lut` when the
/// output has an ICC profile.
fn paint_output_offscreen_as(
    bridge: &mut VelloDrmBridge,
    scene: &Scene,
    view: &OutputView,
    damage: SceneDamage<'_>,
    encoding: (wgpu::TextureFormat, Option<&IccLut>),
) -> (wgpu::Texture, GpuFrameStats) {
    paint_output_offscreen_with(bridge, (scene, None), view, damage, encoding)
}

/// [`paint_output_offscreen_as`] for a frame that may also come as
/// layers (`FramePaint::cache`), as the DRM direct path passes it.
fn paint_output_offscreen_with(
    bridge: &mut VelloDrmBridge,
    (scene, layers): (&Scene, Option<&crate::shell::frame::FrameCache>),
    view: &OutputView,
    damage: SceneDamage<'_>,
    (format, color_lut): (wgpu::TextureFormat, Option<&IccLut>),
) -> (wgpu::Texture, GpuFrameStats) {
    let (width, height) = view.target_size();
    let (device, _) = bridge.gpu();
    let device = device.clone();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("output-view-test-scanout"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let stats = bridge
        .paint_output_into(
            scene,
            view,
            damage,
            OutputTarget {
                view: &target_view,
                format,
                size: (width, height),
                orientation: view.orientation,
            },
            Instant::now()
                .checked_add(tilo_test_env::hang_bound())
                .expect("the hang bound fits in an Instant"),
            OutputSignal {
                sdr_white_nits: crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
                hdr_layers: &[],
                color_lut,
                dim: None,
                layers,
            },
        )
        .expect("the output's frame renders");
    (texture, stats)
}
