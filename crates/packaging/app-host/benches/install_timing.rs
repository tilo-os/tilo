//! Install a real signed ~1 MiB qpkg repeatedly and report wall-clock samples.
//!
//! Run with:
//!
//!     cargo bench -p app-host --bench install_timing

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "the benchmark fixture fails closed and does not process user input"
)]

#[path = "../tests/common/mod.rs"]
mod common;

use app_host::install::install_qpkg;
use app_host::registry::Pinning;
use app_host::store::Store;

fn main() {
    const ITERATIONS: u32 = 10;

    // Build once: this benchmark measures installation rather than package
    // construction or signing.
    let (bytes, _id) = common::build_qpkg(1024);
    println!("qpkg size: {} bytes", bytes.len());

    let mut samples = Vec::with_capacity(ITERATIONS as usize);
    for _ in 0..ITERATIONS {
        let dir = tempfile::tempdir().expect("create benchmark store");
        let store = Store::open(dir.path()).expect("open benchmark store");
        let started = std::time::Instant::now();
        install_qpkg(&store, &bytes, Pinning::Tried).expect("install benchmark package");
        samples.push(started.elapsed());
    }

    let sum: std::time::Duration = samples.iter().sum();
    let average = sum
        .checked_div(ITERATIONS)
        .expect("benchmark iteration count is non-zero");
    let minimum = samples.iter().min().expect("at least one sample");
    let maximum = samples.iter().max().expect("at least one sample");
    println!("install(1 MiB) x{ITERATIONS}: avg {average:?}, min {minimum:?}, max {maximum:?}");
}
