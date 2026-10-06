//! CPU-only SHM conversion, frame-store publication and snapshot costs.
//! Excludes guarded pool copying, queues, GPU upload, drawing and presentation.
//! Run with `cargo run --locked -p shell --release --example shm_pipeline_bench` in the benchmark environment.

use std::hint::black_box;
use std::time::Instant;

use shell::compositor::foreign::store;
use shell::compositor::surface_readback::swizzle_row_to_rgba;
use smithay::reexports::wayland_server::protocol::wl_shm;

const SURFACE: u64 = 0x53_48_4d_03;
type BenchResult<T> = Result<T, Box<dyn std::error::Error>>;

struct FrameSlot;

impl Drop for FrameSlot {
    fn drop(&mut self) {
        store().forget(SURFACE);
    }
}

#[derive(serde::Serialize)]
struct Sample {
    #[serde(rename = "conversion_ns")]
    conversion: u128,
    #[serde(rename = "publication_ns")]
    publication: u128,
    #[serde(rename = "snapshot_ns")]
    snapshot: u128,
    #[serde(rename = "total_ns")]
    total: u128,
}

fn main() -> BenchResult<()> {
    for (width, height) in [(1920, 1080), (3840, 2160)] {
        for format in [wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888] {
            measure(width, height, format)?;
        }
    }
    Ok(())
}

fn sample(
    source: &[u8],
    width: u32,
    height: u32,
    format: wl_shm::Format,
    rgba: &mut Vec<u8>,
) -> BenchResult<Sample> {
    let began = Instant::now();
    rgba.clear();
    let row_width = usize::try_from(width)?;
    for row in source.chunks_exact(row_width.saturating_mul(4)) {
        swizzle_row_to_rgba(format, black_box(row), row_width, rgba)?;
    }
    let converted = Instant::now();
    store().publish_rgba(SURFACE, black_box(rgba), width, height);
    let published = Instant::now();
    let snapshot = store().snapshot(SURFACE).ok_or("missing published frame")?;
    black_box(&snapshot.rgba);
    let captured = Instant::now();
    if (snapshot.width, snapshot.height) != (width, height) || snapshot.rgba != *rgba {
        return Err("publication or snapshot changed frame pixels".into());
    }
    Ok(Sample {
        conversion: converted.duration_since(began).as_nanos(),
        publication: published.duration_since(converted).as_nanos(),
        snapshot: captured.duration_since(published).as_nanos(),
        total: captured.duration_since(began).as_nanos(),
    })
}

fn percentiles(mut values: Vec<u128>) -> BenchResult<serde_json::Value> {
    values.sort_unstable();
    Ok(serde_json::json!({
        "p50_ns": values.get(49).ok_or("missing median")?,
        "p95_ns": values.get(94).ok_or("missing p95")?,
        "p99_ns": values.get(98).ok_or("missing p99")?,
    }))
}

fn measure(width: u32, height: u32, format: wl_shm::Format) -> BenchResult<()> {
    store().forget(SURFACE);
    let _slot = FrameSlot;
    let bytes = usize::try_from(width)?
        .saturating_mul(usize::try_from(height)?)
        .saturating_mul(4);
    let source: Vec<u8> = (0..bytes)
        .map(|index| {
            let [byte, ..] = index.wrapping_mul(131).wrapping_add(17).to_le_bytes();
            byte
        })
        .collect();
    let mut rgba = Vec::with_capacity(bytes);
    for _ in 0..8 {
        sample(&source, width, height, format, &mut rgba)?;
    }
    let capacity = rgba.capacity();
    let mut samples = Vec::with_capacity(100);
    for _ in 0..100 {
        samples.push(sample(&source, width, height, format, &mut rgba)?);
    }
    if rgba.len() != bytes || rgba.capacity() != capacity || store().frame_seq(SURFACE) != 108 {
        return Err("pipeline did not retain capacity or publish every sampled frame".into());
    }
    println!(
        "{}",
        serde_json::json!({
            "workload": "shm-conversion-publication-snapshot", "width": width, "height": height,
            "format": format!("{format:?}"), "arch": std::env::consts::ARCH,
            "debug_assertions": cfg!(debug_assertions), "bytes": bytes,
            "destination_capacity": capacity,
            "conversion": percentiles(samples.iter().map(|s| s.conversion).collect())?,
            "publication": percentiles(samples.iter().map(|s| s.publication).collect())?,
            "snapshot": percentiles(samples.iter().map(|s| s.snapshot).collect())?,
            "total": percentiles(samples.iter().map(|s| s.total).collect())?,
            "samples": samples,
        })
    );
    Ok(())
}
