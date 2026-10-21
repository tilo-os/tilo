//! CPU-only benchmark for the DRM readback fallback's RGBA-to-XRGB conversion.
//!
//! This isolates the phase that runs after GPU readback has mapped, so it does
//! not need a display, DRM master, or a working wgpu adapter. Run both profiles
//! when changing this loop; developer-session performance is intentionally part
//! of the contract because physical candidate testing uses that profile:
//!
//! ```text
//! cargo run -p shell --example xrgb_swizzle_bench
//! cargo run -p shell --release --example xrgb_swizzle_bench
//! ```

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "a fixed-size benchmark fixture and its reporting-only throughput figures"
)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use shell::shell::render::paint::VelloDrmBridge;

const WIDTH: usize = 1_920;
const HEIGHT: usize = 1_080;
const BYTES: usize = WIDTH * HEIGHT * 4;
const MIN_SAMPLE_TIME: Duration = Duration::from_secs(2);

fn main() {
    let mut rgba = vec![0_u8; BYTES];
    for (index, byte) in rgba.iter_mut().enumerate() {
        *byte = index.wrapping_mul(131).wrapping_add(17) as u8;
    }
    let mut xrgb = vec![0_u8; BYTES];

    for _ in 0..4 {
        VelloDrmBridge::swizzle_xrgb_row(black_box(&mut xrgb), black_box(&rgba));
    }

    let started = Instant::now();
    let mut frames = 0_u64;
    while started.elapsed() < MIN_SAMPLE_TIME {
        VelloDrmBridge::swizzle_xrgb_row(black_box(&mut xrgb), black_box(&rgba));
        frames = frames.saturating_add(1);
    }
    let elapsed = started.elapsed();
    let elapsed_s = elapsed.as_secs_f64();
    let frame_ms = elapsed_s * 1_000.0 / frames as f64;
    let gib_s = BYTES as f64 * frames as f64 / elapsed_s / 1024_f64.powi(3);

    assert_eq!(&xrgb[..4], &[rgba[2], rgba[1], rgba[0], 0xff]);
    println!(
        "1920x1080 RGBA->XRGB: {frames} frames in {elapsed_s:.3}s, {frame_ms:.3} ms/frame, {gib_s:.2} GiB/s"
    );
}
