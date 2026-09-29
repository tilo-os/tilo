//! End-to-end probe for the zero-copy composite: bring up the WPE engine
//! in dmabuf mode, clear a target texture to a marker grey, run the
//! `WebViewCompositor` (import + blit) into a content sub-rect, read the
//! target back, and assert the content rect shows the live page while the
//! reserved "toolbar" strip stays grey.
//!
//! Run:
//!   TILO_WEBVIEW_DMABUF=1 TILO_WEBVIEW_DRM_NODE=/dev/dri/renderD128 \
//!   GALLIUM_DRIVER=radeonsi AMD_DEBUG=nodcc,notiling \
//!   cargo run -p browser --example dmabuf_composite_probe --features wpe --release

use std::sync::{Arc, Mutex};
use ui::time::Duration;

use browser::compositor::WebViewCompositor;
use ui::render_thread::FramePostProcess;
use ui::webview::wpe::WpeEngine;
use ui::webview::{SharedEngine, WebViewCommand};
use vello::wgpu;

const W: u32 = 1000;
const H: u32 = 700;
const TOOLBAR_FRAC: f32 = 0.1; // top 10% reserved for "chrome"

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
        // RENDER_ATTACHMENT for the grey clear, STORAGE_BINDING for the
        // compositor's compute write, COPY_SRC for readback — mirroring
        // vello's intermediate (STORAGE_BINDING) that the live path uses.
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let engine: SharedEngine = WpeEngine::new().expect("WpeEngine::new");
    engine.send(WebViewCommand::Resize {
        width: W,
        height: (H as f32 * (1.0 - TOOLBAR_FRAC)) as u32,
        scale_factor: 1.0,
    });
    engine.send(WebViewCommand::Navigate("https://example.org".into()));

    let rect = Arc::new(Mutex::new(Some([0.0, TOOLBAR_FRAC, 1.0, 1.0])));
    let mut comp = WebViewCompositor::new(engine, rect);

    let deadline = ui::time::now() + Duration::from_secs(25);
    loop {
        if ui::time::now() > deadline {
            eprintln!("[composite] FAIL: no composited frame within 25s");
            std::process::exit(1);
        }
        // Clear the target to marker grey, then let the compositor blit.
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

        comp.post_render(&device, &queue, &view, W, H);

        let (toolbar, content) = readback(&device, &queue, &target);
        // Toolbar strip should stay grey (~38); content should be the live
        // page (bright, ~white background of example.org).
        if content.0 > 200 && content.1 > 200 && content.2 > 200 && toolbar.0 < 80 {
            println!(
                "[composite] toolbar RGB {:?} (grey), content RGB {:?} (page)",
                toolbar, content
            );
            println!("[composite] SUCCESS: zero-copy WebView composited into its rect.");
            return;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
}

/// Read back the target; return the average RGB of a pixel in the toolbar
/// strip and one in the content region.
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
    let toolbar = at(W / 2, (H as f32 * TOOLBAR_FRAC * 0.5) as u32);
    let content = at(W / 2, H * 3 / 4);
    (toolbar, content)
}
