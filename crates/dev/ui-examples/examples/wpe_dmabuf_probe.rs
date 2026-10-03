//! Runtime probe for the DMA-BUF zero-copy import.
//!
//! Spawns the WPE engine in dmabuf mode, brings up a wgpu device on the
//! **same** GPU the engine exports from, imports an exported frame into a
//! wgpu texture with no copy, reads a few pixels back, and reports whether
//! the texture is live. This validates the whole export→import chain that
//! the browser's zero-copy path relies on.
//!
//! Run:
//!   TILO_WEBVIEW_DMABUF=1 TILO_WEBVIEW_DRM_NODE=/dev/dri/renderD128 \
//!   GALLIUM_DRIVER=radeonsi AMD_DEBUG=nodcc,notiling \
//!   cargo run -p ui-examples --example ui-wpe --features webview-wpe --release -- wpe_dmabuf_probe

use std::time::Duration;

use tilo_ui::webview::wpe::WpeEngine;
use tilo_ui::webview::wpe::import::import_dmabuf;
use tilo_ui::webview::{WebViewCommand, WebViewEngine};
use vello::wgpu;

pub fn run(_args: &[String]) {
    // wgpu on Vulkan, on the AMD adapter (must match the engine's export GPU).
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
    println!("[probe] wgpu adapter: {}", adapter.get_info().name);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("dmabuf-probe"),
        ..Default::default()
    }))
    .expect("request_device");

    // Bring up the engine and load a page.
    let engine = WpeEngine::new().expect("WpeEngine::new");
    engine.send(WebViewCommand::Resize {
        width: 800,
        height: 600,
        scale_factor: 1.0,
    });
    engine.send(WebViewCommand::Navigate("https://example.org".to_string()));

    // Poll for an importable dmabuf frame.
    let deadline = tilo_ui::time::now() + Duration::from_secs(25);
    let (texture, w, h) = loop {
        if tilo_ui::time::now() > deadline {
            eprintln!("[probe] FAIL: no importable dmabuf frame within 25s");
            std::process::exit(1);
        }
        if let Some(frame) = engine.take_dmabuf() {
            println!(
                "[probe] got dmabuf {}x{} modifier={:#x} planes={}",
                frame.width,
                frame.height,
                frame.modifier,
                frame.planes.len()
            );
            if let Some(tex) = import_dmabuf(&device, &frame) {
                break (tex, frame.width, frame.height);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    println!("[probe] imported wgpu texture {}x{}", w, h);

    // Copy the imported texture into a readback buffer.
    let bpr = ((w * 4).div_ceil(256)) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);

    // Map + inspect.
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    rx.recv().unwrap().expect("map_async");
    let data = slice.get_mapped_range();

    let mut non_zero = 0u64;
    let mut total = 0u64;
    for row in 0..h as usize {
        let base = row * bpr as usize;
        for px in 0..w as usize {
            let p = base + px * 4;
            total += 1;
            if data[p] != 0 || data[p + 1] != 0 || data[p + 2] != 0 {
                non_zero += 1;
            }
        }
    }
    // Sample the center pixel (BGRA in memory).
    let cx = (h as usize / 2) * bpr as usize + (w as usize / 2) * 4;
    println!(
        "[probe] center pixel BGRA = {} {} {} {}",
        data[cx],
        data[cx + 1],
        data[cx + 2],
        data[cx + 3]
    );
    let pct = non_zero as f64 / total as f64 * 100.0;
    println!("[probe] non-zero pixels: {non_zero}/{total} ({pct:.1}%)");
    if non_zero > total / 100 {
        println!("[probe] SUCCESS: zero-copy dmabuf import produced a live frame.");
    } else {
        eprintln!("[probe] FAIL: imported texture is blank.");
        std::process::exit(2);
    }
}
