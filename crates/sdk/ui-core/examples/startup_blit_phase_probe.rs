//! Attribute first blit latency using the SDK GPU context and upstream blitter.
//! No display, font system or app state. Each process has a private Mesa cache.
//! The optional wait shifts queued raster work for diagnosis, not optimization.

use std::num::NonZeroUsize;
use std::path::PathBuf;

pub mod startup_cache_fixture;
use startup_cache_fixture::{populate_seed, publish_cache};
use tilo_ui::vello::{self, kurbo, peniko, wgpu};

const WIDTH: u32 = 664;
const HEIGHT: u32 = 592;
const ROW_BYTES: u32 = (WIDTH * 4).div_ceil(256) * 256;
const BUFFER_BYTES: u64 = ROW_BYTES as u64 * HEIGHT as u64;

enum CacheMode {
    Private,
    SingleFile,
    Publish(PathBuf),
    Seed(PathBuf),
}

impl CacheMode {
    fn label(&self) -> &'static str {
        match self {
            Self::Private => "private-default",
            Self::SingleFile => "private-single-file",
            Self::Publish(_) => "publish-single-file",
            Self::Seed(_) => "private-with-read-only-seed",
        }
    }
}

fn options() -> Result<(bool, CacheMode), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut wait = false;
    let mut mode = CacheMode::Private;
    while let Some(arg) = args.next() {
        if arg == "--wait-after-raster" {
            wait = true;
            continue;
        }
        if !matches!(mode, CacheMode::Private) {
            return Err("choose exactly one cache mode".into());
        }
        mode = if arg == "--single-file-cache" {
            CacheMode::SingleFile
        } else if arg == "--export-cache" {
            CacheMode::Publish(args.next().ok_or("missing export directory")?.into())
        } else if arg == "--seed-cache" {
            CacheMode::Seed(args.next().ok_or("missing seed directory")?.into())
        } else {
            return Err(format!("unknown argument: {}", arg.display()).into());
        };
    }
    Ok((wait, mode))
}

fn prepare_cache(
    home: &mut tilo_test_env::TestHome,
    mode: &CacheMode,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cache = home.cache().join("mesa-phase-probe");
    std::fs::create_dir(&cache)?;
    home.env()
        .set("MESA_SHADER_CACHE_DIR", &cache)
        .set("MESA_SHADER_CACHE_SHOW_STATS", "true")
        .remove("MESA_SHADER_CACHE_DISABLE")
        .remove("MESA_DISK_CACHE_SINGLE_FILE")
        .remove("MESA_DISK_CACHE_MULTI_FILE")
        .remove("MESA_DISK_CACHE_DATABASE")
        .remove("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS")
        .remove("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS_DYNAMIC_LIST")
        .remove("MESA_DISK_CACHE_COMBINE_RW_WITH_RO_FOZ")
        .remove("DISPLAY")
        .remove("WAYLAND_DISPLAY");
    if matches!(mode, CacheMode::SingleFile | CacheMode::Publish(_)) {
        home.env().set("MESA_DISK_CACHE_SINGLE_FILE", "1");
    }
    if let CacheMode::Seed(dir) = mode {
        let dir = dir.canonicalize()?;
        populate_seed(&dir, &cache)?;
        home.env()
            // Retain the ordinary size-limited writable cache. Only the
            // separate precompiled archive uses the read-only FOZ backend.
            .set("MESA_DISK_CACHE_COMBINE_RW_WITH_RO_FOZ", "1")
            .set("MESA_DISK_CACHE_READ_ONLY_FOZ_DBS", "tilo-seed");
    }
    Ok(cache)
}

fn phase<T>(
    events: &mut Vec<(&'static str, f64)>,
    name: &'static str,
    run: impl FnOnce() -> T,
) -> T {
    let start = tilo_ui::time::now();
    let value = run();
    events.push((name, start.elapsed().as_secs_f64() * 1000.0));
    value
}

fn texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("startup phase probe"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn draw_frame(
    handle: &tilo_ui::gpu::DeviceHandle,
    renderer: &mut vello::Renderer,
    source: &wgpu::TextureView,
    destination: &wgpu::Texture,
    blitter: &wgpu::util::TextureBlitter,
    wait_after_raster: bool,
) -> Result<Vec<(&'static str, f64)>, Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    let mut scene = vello::Scene::new();
    scene.fill(
        peniko::Fill::NonZero,
        kurbo::Affine::IDENTITY,
        peniko::Color::from_rgb8(255, 0, 0),
        None,
        &kurbo::Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
    );
    phase(&mut events, "raster-submit", || {
        renderer.render_to_texture(
            &handle.device,
            &handle.queue,
            &scene,
            source,
            &vello::RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: WIDTH,
                height: HEIGHT,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
    })?;
    if wait_after_raster {
        phase(&mut events, "wait-after-raster", || {
            handle.device.poll(wgpu::PollType::wait_indefinitely())
        })?;
    }
    let mut encoder = phase(&mut events, "blit-encoder", || {
        handle
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default())
    });
    let target = phase(&mut events, "destination-view", || {
        destination.create_view(&wgpu::TextureViewDescriptor::default())
    });
    phase(&mut events, "blitter-copy", || {
        blitter.copy(&handle.device, &mut encoder, source, &target);
    });
    phase(&mut events, "blit-submit", || {
        handle.queue.submit([encoder.finish()])
    });
    phase(&mut events, "wait-after-blit", || {
        handle.device.poll(wgpu::PollType::wait_indefinitely())
    })?;
    Ok(events)
}

fn verify_pixels(
    handle: &tilo_ui::gpu::DeviceHandle,
    destination: &wgpu::Texture,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read every real destination pixel after the timed intervals. BGRA red
    // is [0,0,255,255]; row padding is excluded from the comparison.
    let buffer = handle.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("startup phase readback"),
        size: BUFFER_BYTES,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = handle
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        destination.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ROW_BYTES),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    handle.queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    handle.device.poll(wgpu::PollType::wait_indefinitely())?;
    receiver.recv()??;
    let mapping = buffer.slice(..).get_mapped_range();
    if mapping.len() != usize::try_from(BUFFER_BYTES)? {
        return Err("incomplete destination readback".into());
    }
    let mut differences = 0_usize;
    for row in mapping.chunks_exact(usize::try_from(ROW_BYTES)?) {
        let visible = row
            .get(..usize::try_from(WIDTH * 4)?)
            .ok_or("short destination row")?;
        let mismatches = visible
            .chunks_exact(4)
            .filter(|pixel| *pixel != [0, 0, 255, 255])
            .count();
        differences = differences
            .checked_add(mismatches)
            .ok_or("pixel count overflow")?;
    }
    if differences != 0 {
        return Err(
            format!("{differences} destination pixels differ from the rendered red").into(),
        );
    }
    drop(mapping);
    buffer.unmap();
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (wait_after_raster, mode) = options()?;
    let mut home = tilo_test_env::TestHome::new()?;
    let mut setup = Vec::new();
    let cache = phase(&mut setup, "cache-fixture", || {
        prepare_cache(&mut home, &mode)
    })?;
    let mut context = phase(&mut setup, "context", tilo_ui::gpu::RenderContext::new);
    let device_id = phase(&mut setup, "device", || {
        pollster::block_on(context.device(None))
    })
    .ok_or("no drawing device")?;
    let handle = context
        .devices
        .get(device_id)
        .ok_or("device index missing")?;
    let info = handle.adapter.get_info();
    let source = phase(&mut setup, "source-texture", || {
        texture(
            &handle.device,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        )
    });
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let destination = phase(&mut setup, "destination-texture", || {
        texture(
            &handle.device,
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        )
    });
    let blitter = phase(&mut setup, "blitter-construct", || {
        wgpu::util::TextureBlitter::new(&handle.device, wgpu::TextureFormat::Bgra8Unorm)
    });
    let mut renderer = phase(&mut setup, "vello-construct", || {
        vello::Renderer::new(
            &handle.device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: Some(NonZeroUsize::MIN.saturating_add(1)),
                pipeline_cache: None,
            },
        )
    })?;
    let mut frames = Vec::new();
    for _ in 0..3 {
        frames.push(draw_frame(
            handle,
            &mut renderer,
            &source_view,
            &destination,
            &blitter,
            wait_after_raster,
        )?);
        verify_pixels(handle, &destination)?;
    }
    // Close every GPU object before copying the driver's completed disk cache.
    drop(renderer);
    drop(blitter);
    drop(source_view);
    drop(source);
    drop(destination);
    drop(context);
    let exported_bytes = if let CacheMode::Publish(dir) = &mode {
        Some(publish_cache(&cache, dir)?)
    } else {
        None
    };
    println!(
        "{}",
        serde_json::json!({ "profile": if cfg!(debug_assertions) {"debug"} else {"release"},
        "adapter": format!("{info:?}"), "size": [WIDTH, HEIGHT], "private_mesa_cache": true,
        "fonts_or_surface": false, "wait_after_raster": wait_after_raster, "setup_ms": setup,
        "frames_ms": frames, "destination_pixels_verified": WIDTH * HEIGHT,
        "cache_mode": mode.label(), "exported_cache_bytes": exported_bytes,
        "seed_scope": "read-only private copies; production shared-seed sandbox not exercised" })
    );
    Ok(())
}
