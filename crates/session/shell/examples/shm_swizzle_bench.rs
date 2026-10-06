//! CPU-only benchmark of the production SHM row conversion, excluding mapping,
//! capture queues, GPU work and display presentation. Reuses frame storage.
//! Run with `cargo run --locked -p shell --release --example shm_swizzle_bench` in the benchmark environment.

use std::hint::black_box;
use std::time::Instant;

use shell::compositor::surface_readback::swizzle_row_to_rgba;
use smithay::reexports::wayland_server::protocol::wl_shm;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for (width, height) in [(801, 601), (1920, 1080), (3840, 2160)] {
        for format in [wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888] {
            measure(width, height, format)?;
        }
    }
    Ok(())
}

fn convert(
    src: &[u8],
    width: usize,
    format: wl_shm::Format,
    out: &mut Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    out.clear();
    for row in src.chunks_exact(width.saturating_mul(4)) {
        swizzle_row_to_rgba(format, black_box(row), width, out)?;
    }
    black_box(&*out);
    Ok(())
}

fn measure(
    width: usize,
    height: usize,
    format: wl_shm::Format,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = width.saturating_mul(height).saturating_mul(4);
    let src: Vec<u8> = (0..bytes)
        .map(|index| {
            let [byte, ..] = index.wrapping_mul(131).wrapping_add(17).to_le_bytes();
            byte
        })
        .collect();
    let mut out = Vec::with_capacity(bytes);
    for _ in 0..8 {
        convert(&src, width, format, &mut out)?;
    }
    let capacity = out.capacity();
    let mut samples = Vec::with_capacity(100);
    for _ in 0..100 {
        let began = Instant::now();
        convert(&src, width, format, &mut out)?;
        samples.push(began.elapsed().as_nanos());
    }
    if out.len() != bytes || out.capacity() != capacity {
        return Err("conversion changed frame length or grew steady storage".into());
    }
    for (source, target) in src.chunks_exact(4).zip(out.chunks_exact(4)) {
        let [blue, green, red, alpha] = source else {
            return Err("fixture has a partial pixel".into());
        };
        let alpha = if format == wl_shm::Format::Xrgb8888 {
            255
        } else {
            *alpha
        };
        if target != [*red, *green, *blue, alpha] {
            return Err("conversion changed a fixture pixel".into());
        }
    }
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({
            "workload": "shm-row-conversion", "width": width, "height": height,
            "format": format!("{format:?}"), "arch": std::env::consts::ARCH,
            "debug_assertions": cfg!(debug_assertions), "bytes": bytes,
            "destination_capacity": capacity, "samples_ns": samples,
            "p50_ns": samples.get(49).ok_or("missing median")?,
            "p95_ns": samples.get(94).ok_or("missing p95")?,
            "p99_ns": samples.get(98).ok_or("missing p99")?,
        })
    );
    Ok(())
}
