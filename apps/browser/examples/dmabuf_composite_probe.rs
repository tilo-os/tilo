//! End-to-end probe for the zero-copy composite through the multi-layer
//! registry: bring up a WPE engine in dmabuf mode, clear a target texture
//! to marker grey, run the shared `MultiLayerCompositor` with a
//! `LayerSnapshot` into a content sub-rect, read the target back, and
//! assert the content rect shows the live page while the reserved top
//! "toolbar" strip stays grey.
//!
//! The compositor is fully multi-layer (it composites a `Vec` of layers,
//! each into its own rect), but WPE-FDO supports only **one nested host /
//! EGL display per process** — so a single process can drive at most one
//! dmabuf-exporting engine. In qOS each web-viewing app is its own
//! process, so that's the real topology. This probe therefore exercises
//! the supported single-engine path through the new registry API.
//!
//! Run:
//!   QOS_WEBVIEW_DMABUF=1 QOS_WEBVIEW_DRM_NODE=/dev/dri/renderD128 \
//!   GALLIUM_DRIVER=radeonsi AMD_DEBUG=nodcc,notiling \
//!   cargo run -p browser --example dmabuf_composite_probe --features wpe --release

use std::time::{Duration, Instant};

use ui::render_thread::LayerSnapshot;
use ui::webview::compositor::MultiLayerCompositor;
use ui::webview::wpe::WpeEngine;
use ui::webview::{SharedEngine, WebViewCommand};
use vello::wgpu;

const W: u32 = 1000;
const H: u32 = 700;
const TOOLBAR: u32 = 70; // top strip reserved for "chrome" (stays grey)

fn boot(url: &str, w: u32, h: u32) -> SharedEngine {
    let engine: SharedEngine = WpeEngine::new().expect("WpeEngine::new");
    engine.send(WebViewCommand::Resize {
        width: w,
        height: h,
        scale_factor: 1.0,
    });
    engine.send(WebViewCommand::Navigate(url.into()));
    engine
}

fn main() {
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(idesc);
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
    let adapter = adapters
        .iter()
        .find(|a| {
            let n = a.get_info().name.to_lowercase();
            n.contains("amd") || n.contains("radeon")
        })
        .or_else(|| adapters.first())
        .expect("no Vulkan adapter");
    println!("[composite] adapter: {}", adapter.get_info().name);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("composite-probe"),
        ..Default::default()
    }))
    .expect("request_device");

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        // Mirrors vello's intermediate (STORAGE_BINDING) that the live path
        // uses; RENDER_ATTACHMENT for the grey clear, COPY_SRC for readback.
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    // One engine (WPE-FDO caps it at one per process), composited into the
    // content region below a reserved toolbar strip.
    let content_h = H - TOOLBAR;
    let engine = boot("https://example.org", W, content_h);
    let layers = vec![LayerSnapshot {
        engine: engine.clone(),
        dev_rect: [0.0, TOOLBAR as f32, W as f32, H as f32],
    }];

    let mut comp = MultiLayerCompositor::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() > deadline {
            eprintln!("[composite] FAIL: composited content not seen within 30s");
            std::process::exit(1);
        }
        // Clear to marker grey, then composite both layers.
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let _clear = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.15,
                            g: 0.15,
                            b: 0.15,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit([enc.finish()]);

        comp.composite(&device, &queue, &view, W, H, &layers);

        let (toolbar, content) = readback(&device, &queue, &target);
        let bright = |c: (u8, u8, u8)| c.0 > 200 && c.1 > 200 && c.2 > 200;
        if bright(content) && toolbar.0 < 80 {
            println!(
                "[composite] toolbar {:?} (grey), content {:?} (page)",
                toolbar, content
            );
            println!(
                "[composite] SUCCESS: zero-copy WebView composited into its rect via the registry."
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
}

/// Read back a toolbar pixel plus one content pixel.
fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
) -> ((u8, u8, u8), (u8, u8, u8)) {
    let bpr = ((W * 4).div_ceil(256)) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (bpr * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range();
    let at = |x: u32, y: u32| {
        let p = (y * bpr + x * 4) as usize;
        (data[p], data[p + 1], data[p + 2])
    };
    let toolbar = at(W / 2, TOOLBAR / 2);
    let content = at(W / 2, H * 3 / 4);
    (toolbar, content)
}
