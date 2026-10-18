//! HDR surfaces composited at full precision, on a real adapter.
//!
//! A scene holds an SDR desktop, a PQ surface under a rounded clip and a
//! clip rectangle, an HLG surface drawn at twice its size overlapping it, and
//! a half-transparent SDR scrim over the PQ surface. The direct presenter's
//! GPU composite and the readback presenter's CPU composite are both compared
//! against the expected light of each region, worked out from the scene in
//! linear light; the SDR output variant against the tone-mapper. Without the
//! composite the same scene shows the tone-mapped SDR rendition.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; fixture sizes are small constants"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use settings_store::hardware::OutputTransform;
use shell::collab::colorimetry::Colorimetry;
use shell::compositor::backend::drm::paint::{
    OutputSignal, OutputTarget, OutputView, SceneDamage, VelloDrmBridge,
};
use shell::shell::color_pipeline::{pq_eotf, pq_inverse_eotf, srgb_eotf};
use shell::shell::hdr_surface::{
    CompositeTarget, HdrLayer, HdrLayerRecording, HdrPlane, SurfaceSignal, composite_hdr_pixel,
    record, signal_to_output_nits,
};
use shell::shell::hdr_tonemap::{
    HdrTonemapConfig, tonemap_rgb_signal_to_sdr_linear, tonemap_rgb_signal_to_srgb8,
};
use shell::shell::render::OutputId;
use ui::dmabuf_import::ImportedRenderTarget;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill, ImageBrush, ImageData};
use vello::wgpu;

const WIDTH: u32 = 96;
const HEIGHT: u32 = 64;
const SDR_WHITE: f32 = 203.0;
/// The desktop's grey, sRGB.
const GREY: u8 = 128;
/// The PQ surface's light, cd/m² per BT.2020 channel.
const PQ_NITS: [f32; 3] = [800.0, 400.0, 150.0];
/// The HLG surface's signal.
const HLG_SIGNAL: [f32; 3] = [0.75, 0.5, 0.25];

fn pq_signal() -> [f32; 3] {
    PQ_NITS.map(pq_inverse_eotf)
}

/// A uniform plane and its tone-mapped SDR rendition.
fn surface(
    width: u32,
    height: u32,
    rgb: [f32; 3],
    colorimetry: Colorimetry,
) -> (Arc<HdrPlane>, ImageData) {
    let signal = SurfaceSignal::for_colorimetry(colorimetry, HdrTonemapConfig::default())
        .expect("an HDR description");
    let count = (width * height) as usize;
    let plane = HdrPlane::from_signal_rgb(width, height, std::iter::repeat_n(rgb, count), signal)
        .expect("plane");
    let [r, g, b] = tonemap_rgb_signal_to_srgb8(rgb, colorimetry, HdrTonemapConfig::default());
    let image = ui::raster::rgba8_premultiplied([r, g, b, 255].repeat(count), width, height);
    (Arc::new(plane), image)
}

fn draw_surface(
    scene: &mut Scene,
    plane: &Arc<HdrPlane>,
    image: &ImageData,
    image_to_scene: Affine,
    clip: Rect,
) {
    let extent = Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height));
    record(
        plane,
        image,
        image_to_scene,
        image_to_scene.transform_rect_bbox(extent).intersect(clip),
    );
    scene.fill(
        Fill::NonZero,
        image_to_scene,
        &ImageBrush::new(image.clone()),
        None,
        &extent,
    );
}

/// The test scene, with the HDR layers it records.
fn scene() -> (Scene, Vec<HdrLayer>) {
    let recording = HdrLayerRecording::begin();
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(GREY, GREY, GREY),
        None,
        &Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
    );
    // PQ: 48×48 at (8, 8), rounded by 12, and cut at x = 48.
    let (pq, pq_image) = surface(48, 48, pq_signal(), Colorimetry::hdr10());
    let cut = Rect::new(0.0, 0.0, 48.0, f64::from(HEIGHT));
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &cut,
    );
    let rounded = RoundedRect::new(8.0, 8.0, 56.0, 56.0, 12.0);
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &rounded,
    );
    draw_surface(
        &mut scene,
        &pq,
        &pq_image,
        Affine::translate((8.0, 8.0)),
        cut,
    );
    scene.pop_layer();
    scene.pop_layer();
    // A half-transparent white scrim over part of it.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(255, 255, 255, 128),
        None,
        &Rect::new(12.0, 40.0, 28.0, 52.0),
    );
    // HLG: 16×16 drawn at twice its size at (40, 20), over the PQ surface.
    let (hlg, hlg_image) = surface(16, 16, HLG_SIGNAL, Colorimetry::hdr_hlg());
    draw_surface(
        &mut scene,
        &hlg,
        &hlg_image,
        Affine::translate((40.0, 20.0)).pre_scale(2.0),
        Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
    );
    (scene, recording.finish())
}

/// A pixel to check and the light expected there.
struct Probe {
    name: &'static str,
    at: (u32, u32),
    hdr: [f32; 3],
    sdr: [f32; 3],
}

fn probes() -> Vec<Probe> {
    let grey = srgb_eotf(f32::from(GREY) / 255.0);
    let pq_sdr = tonemap_rgb_signal_to_sdr_linear(
        pq_signal(),
        Colorimetry::hdr10(),
        HdrTonemapConfig::default(),
    );
    let hlg_signal =
        SurfaceSignal::for_colorimetry(Colorimetry::hdr_hlg(), HdrTonemapConfig::default())
            .expect("HLG");
    let hlg = signal_to_output_nits(HLG_SIGNAL, &hlg_signal, SDR_WHITE);
    let hlg_sdr = tonemap_rgb_signal_to_sdr_linear(
        HLG_SIGNAL,
        Colorimetry::hdr_hlg(),
        HdrTonemapConfig::default(),
    );
    // The scrim covers 128/255 of the pixel; the PQ surface shows through
    // the rest.
    let scrim = 128.0 / 255.0;
    vec![
        Probe {
            name: "desktop",
            at: (2, 2),
            hdr: [grey * SDR_WHITE; 3],
            sdr: [grey; 3],
        },
        Probe {
            name: "desktop right of the HLG surface",
            at: (80, 60),
            hdr: [grey * SDR_WHITE; 3],
            sdr: [grey; 3],
        },
        Probe {
            name: "PQ surface",
            at: (24, 20),
            hdr: PQ_NITS,
            sdr: pq_sdr,
        },
        Probe {
            name: "outside the PQ surface's rounded corner",
            at: (9, 9),
            hdr: [grey * SDR_WHITE; 3],
            sdr: [grey; 3],
        },
        Probe {
            name: "PQ surface past its clip",
            at: (52, 12),
            hdr: [grey * SDR_WHITE; 3],
            sdr: [grey; 3],
        },
        Probe {
            name: "HLG surface over the PQ surface",
            at: (44, 30),
            hdr: hlg,
            sdr: hlg_sdr,
        },
        Probe {
            name: "HLG surface",
            at: (60, 30),
            hdr: hlg,
            sdr: hlg_sdr,
        },
        Probe {
            name: "scrim over the PQ surface",
            at: (20, 46),
            hdr: [0, 1, 2].map(|c| scrim * SDR_WHITE + (1.0 - scrim) * PQ_NITS[c]),
            sdr: [0, 1, 2].map(|c| scrim + (1.0 - scrim) * pq_sdr[c]),
        },
    ]
}

fn assert_close(what: &str, got: [f32; 3], want: [f32; 3], relative: f32, absolute: f32) {
    for (channel, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(
            (g - w).abs() <= relative * w.abs() + absolute,
            "{what}, channel {channel}: got {got:?}, want {want:?}"
        );
    }
}

fn target(bridge: &VelloDrmBridge, width: u32, height: u32) -> ImportedRenderTarget {
    let (device, _) = bridge.gpu();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hdr-layer-composite-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgb10a2Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    ImportedRenderTarget {
        texture,
        view,
        format: wgpu::TextureFormat::Rgb10a2Unorm,
        width,
        height,
        stride: width * 4,
    }
}

/// Light in the 10-bit PQ target, cd/m² per channel, row-major.
fn read_nits(bridge: &VelloDrmBridge, target: &ImportedRenderTarget) -> Vec<[f32; 3]> {
    let (device, queue) = bridge.gpu();
    let padded = (target.width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hdr-layer-composite-readback"),
        size: u64::from(padded) * u64::from(target.height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        target.texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(target.height),
            },
        },
        wgpu::Extent3d {
            width: target.width,
            height: target.height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).ok();
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(tilo_test_env::hang_bound()),
        })
        .expect("GPU finishes the copy");
    rx.recv().expect("map callback").expect("map succeeds");
    let mapped = slice.get_mapped_range();
    let mut nits = Vec::new();
    for row in mapped.chunks(padded as usize).take(target.height as usize) {
        for word in row[..(target.width * 4) as usize].chunks_exact(4) {
            let word = u32::from_le_bytes(word.try_into().unwrap());
            nits.push(
                [0, 10, 20]
                    .map(|shift| pq_eotf(f32::from(((word >> shift) & 0x3FF) as u16) / 1023.0)),
            );
        }
    }
    nits
}

fn paint_direct(
    bridge: &mut VelloDrmBridge,
    scene: &Scene,
    target: &ImportedRenderTarget,
    layers: &[HdrLayer],
    revision: u64,
) {
    bridge
        .paint_imported_until(
            scene,
            SceneDamage {
                revision,
                previous_revision: None,
                regions: None,
            },
            target,
            Instant::now() + Duration::from_secs(30),
            SDR_WHITE,
            layers,
        )
        .expect("the frame renders");
}

#[test]
fn hdr_layers_composite_in_linear_light_on_every_output_path() {
    let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).expect("a real wgpu adapter");
    let (scene, layers) = scene();
    assert_eq!(layers.len(), 2, "both HDR surfaces are recorded");
    assert_ne!(layers[0].channel, layers[1].channel);
    let probes = probes();

    // Phase `direct_presenter_composites_on_the_gpu`.
    {
        let target = target(&bridge, WIDTH, HEIGHT);
        paint_direct(&mut bridge, &scene, &target, &layers, 1);
        let nits = read_nits(&bridge, &target);
        for probe in &probes {
            let (x, y) = probe.at;
            assert_close(
                &format!("GPU composite, {}", probe.name),
                nits[(y * WIDTH + x) as usize],
                probe.hdr,
                0.04,
                0.5,
            );
        }
    }

    // Phase `without_the_composite_the_sdr_rendition_shows`.
    // Captures, SDR outputs and any pass that does not composite render the
    // scene as it is: the tone-mapped rendition at SDR white, not a hole.
    {
        let target = target(&bridge, WIDTH, HEIGHT);
        paint_direct(&mut bridge, &scene, &target, &[], 2);
        let nits = read_nits(&bridge, &target);
        let [r, g, b] = nits[(20 * WIDTH + 24) as usize];
        assert!(
            r <= SDR_WHITE * 1.01 && r > 1.0 && g > 1.0 && b > 0.1,
            "the PQ surface shows its SDR rendition: {:?}",
            [r, g, b]
        );
    }

    // Phase `readback_presenter_composites_on_the_cpu_for_hdr_and_sdr_outputs`.
    {
        let (first, second) = bridge
            .paint_with_hdr_layers(&scene, &layers)
            .expect("both renders complete");
        let first = first.to_vec();
        for probe in &probes {
            let (x, y) = probe.at;
            let offset = ((y * WIDTH + x) * 4) as usize;
            let pixel =
                |buffer: &[u8]| -> [u8; 4] { buffer[offset..offset + 4].try_into().unwrap() };
            let hdr = composite_hdr_pixel(
                pixel(&first),
                pixel(&second),
                x,
                y,
                &layers,
                CompositeTarget::Pq {
                    sdr_white_nits: SDR_WHITE,
                },
            );
            assert_close(
                &format!("CPU composite, {}", probe.name),
                hdr,
                probe.hdr,
                0.02,
                0.5,
            );
            let sdr = composite_hdr_pixel(
                pixel(&first),
                pixel(&second),
                x,
                y,
                &layers,
                CompositeTarget::Sdr,
            );
            assert_close(
                &format!("SDR output tone-map, {}", probe.name),
                sdr,
                probe.sdr,
                0.02,
                0.004,
            );
        }
    }
}

#[test]
fn the_hdr_composite_fits_the_frame_budget() {
    const FRAME_BUDGET: Duration = Duration::from_micros(16_667);
    let (width, height) = (1920_u32, 1080_u32);
    let mut bridge = VelloDrmBridge::new(width, height).expect("a real wgpu adapter");
    let recording = HdrLayerRecording::begin();
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(30, 32, 38),
        None,
        &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
    );
    for row in 0..12_u32 {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(60, 64, 80),
            None,
            &RoundedRect::new(
                20.0,
                40.0 + f64::from(row) * 80.0,
                300.0,
                100.0 + f64::from(row) * 80.0,
                12.0,
            ),
        );
    }
    let (plane, image) = surface(1280, 720, pq_signal(), Colorimetry::hdr10());
    let video = RoundedRect::new(400.0, 200.0, 1680.0, 920.0, 16.0);
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &video,
    );
    draw_surface(
        &mut scene,
        &plane,
        &image,
        Affine::translate((400.0, 200.0)),
        video.rect(),
    );
    scene.pop_layer();
    let layers = recording.finish();
    let target = target(&bridge, width, height);

    let mut time = |layers: &[HdrLayer], revision: u64| {
        let started = Instant::now();
        paint_direct(&mut bridge, &scene, &target, layers, revision);
        started.elapsed()
    };
    // Pipelines and the plane upload are paid once, not per frame.
    time(&[], 1);
    time(&layers, 2);
    // Alternate the two so load on a shared runner lands on both, and keep
    // each one's fastest frame: noise only ever adds time.
    let (mut plain, mut composited) = (Duration::MAX, Duration::MAX);
    for i in 0..5 {
        plain = plain.min(time(&[], 10 + 2 * i));
        composited = composited.min(time(&layers, 11 + 2 * i));
    }
    let extra = composited.saturating_sub(plain);
    let adapter = bridge.adapter_info().expect("adapter");
    eprintln!(
        "{} ({}): plain HDR frame {plain:?}, with the HDR composite {composited:?}, extra {extra:?}",
        adapter.name, adapter.device_type
    );
    // A software rasteriser cannot hold any frame budget; there the extra
    // work must stay within one more render of the scene.
    let allowance = if adapter.device_type == "Cpu" {
        plain.max(FRAME_BUDGET) * 2
    } else {
        FRAME_BUDGET
    };
    assert!(
        extra <= allowance,
        "the HDR composite adds {extra:?} per frame on {} (allowance {allowance:?})",
        adapter.name
    );
}

/// The scanout pixel that shows scene pixel `(x, y)` of a `WIDTH`×`HEIGHT` output turned by
/// `transform` (`scanout::logical_to_physical_point`).
fn scanout_pixel((x, y): (u32, u32), transform: OutputTransform) -> (u32, u32) {
    match transform {
        OutputTransform::Normal => (x, y),
        OutputTransform::Rotate90 => (HEIGHT - 1 - y, x),
        OutputTransform::Rotate180 => (WIDTH - 1 - x, HEIGHT - 1 - y),
        OutputTransform::Rotate270 => (y, WIDTH - 1 - x),
    }
}

#[test]
fn a_rotated_output_composites_hdr_layers_where_the_scene_puts_them() {
    // A rotated HDR output renders direct: its surface is upright and the composite reads
    // each scanout pixel from the surface pixel (and the layer texel) it shows. Every probe
    // keeps its light at its turned position, so a layer tested or sampled at the unturned
    // position shows up as the wrong light.
    let mut bridge = VelloDrmBridge::new(WIDTH, HEIGHT).expect("a real wgpu adapter");
    let (scene, layers) = scene();
    let probes = probes();
    for (revision, transform) in (1..).zip([
        OutputTransform::Normal,
        OutputTransform::Rotate90,
        OutputTransform::Rotate180,
        OutputTransform::Rotate270,
    ]) {
        let view = OutputView::at(OutputId(0), (0, 0), (WIDTH, HEIGHT)).oriented(transform);
        let (width, height) = view.target_size();
        let target = target(&bridge, width, height);
        bridge
            .paint_output_into(
                &scene,
                &view,
                SceneDamage {
                    revision,
                    previous_revision: None,
                    regions: None,
                },
                OutputTarget {
                    view: &target.view,
                    format: target.format,
                    size: (width, height),
                    orientation: transform,
                },
                Instant::now() + Duration::from_secs(30),
                OutputSignal {
                    sdr_white_nits: SDR_WHITE,
                    hdr_layers: &layers,
                    dim: None,
                    color_lut: None,
                    layers: None,
                },
            )
            .expect("the frame renders");
        let nits = read_nits(&bridge, &target);
        for probe in &probes {
            let (x, y) = scanout_pixel(probe.at, transform);
            assert_close(
                &format!("{transform:?} GPU composite, {}", probe.name),
                nits[(y * width + x) as usize],
                probe.hdr,
                0.04,
                0.5,
            );
        }
    }
}
