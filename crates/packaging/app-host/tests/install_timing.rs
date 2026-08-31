//! Timing probe (opt-in): install a ~1 MB qpkg N times and print
//! average wall-clock. Run with:
//!
//!     APP_HOST_TIMING=1 cargo test -p app-host --test install_timing --release -- --nocapture

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common;
use app_host::install::install_qpkg;
use app_host::registry::Pinning;
use app_host::store::Store;

#[test]
fn install_1mb_qpkg_timing() {
    if std::env::var_os("APP_HOST_TIMING").is_none() {
        eprintln!("APP_HOST_TIMING not set — skipping timing probe");
        return;
    }
    // ~1 MB payload → ~1 MB qpkg (headers add < 1 KB).
    let (bytes, _id) = common::build_qpkg(1024);
    eprintln!("qpkg size: {} bytes", bytes.len());

    let iters = 10;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let t = std::time::Instant::now();
        install_qpkg(&store, &bytes, Pinning::Tried).unwrap();
        samples.push(t.elapsed());
    }
    let sum: std::time::Duration = samples.iter().sum();
    let avg = sum / u32::try_from(iters).unwrap_or(u32::MAX);
    let min = samples.iter().min().unwrap();
    let max = samples.iter().max().unwrap();
    eprintln!("install(1 MB) x{iters}: avg {avg:?}, min {min:?}, max {max:?}");
}
