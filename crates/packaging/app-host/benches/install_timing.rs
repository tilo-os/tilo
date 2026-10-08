//! Install a real signed ~1 MiB tapp repeatedly and report wall-clock samples,
//! then measure accounting and a size-pressure sweep over a store of many
//! cached packages.
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

use app_host::install::install_tapp;
use app_host::{PackageStore, Pinning};

fn main() {
    const ITERATIONS: u32 = 10;

    // Build once: this benchmark measures installation rather than package
    // construction or signing.
    let (bytes, _id) = common::build_tapp(1024);
    println!("tapp size: {} bytes", bytes.len());

    let mut samples = Vec::with_capacity(ITERATIONS as usize);
    for _ in 0..ITERATIONS {
        let dir = tempfile::tempdir().expect("create benchmark store");
        let store = PackageStore::at(dir.path());
        let started = std::time::Instant::now();
        install_tapp(&store, &bytes, Pinning::Tried, None).expect("install benchmark package");
        samples.push(started.elapsed());
    }

    let sum: std::time::Duration = samples.iter().sum();
    let average = sum
        .checked_div(ITERATIONS)
        .expect("benchmark iteration count is non-zero");
    let minimum = samples.iter().min().expect("at least one sample");
    let maximum = samples.iter().max().expect("at least one sample");
    println!("install(1 MiB) x{ITERATIONS}: avg {average:?}, min {minimum:?}, max {maximum:?}");

    sweep_many();
}

/// A store of `PACKAGES` cached apps, each with its own blob and sharing
/// the rest: how long accounting takes, and a sweep that evicts `EVICTED`.
fn sweep_many() {
    const PACKAGES: u64 = 300;
    const EVICTED: u64 = 20;
    const PAYLOAD_KIB: usize = 4;

    let dir = tempfile::tempdir().expect("create benchmark store");
    let store = PackageStore::at(dir.path());
    for index in 0..PACKAGES {
        let app_id = format!("so.tilo.bench{index}");
        let (bytes, _) = common::build_tapp_for(&app_id, PAYLOAD_KIB);
        install_tapp(&store, &bytes, Pinning::Tried, None).expect("install cached package");
        store
            .update_row(&app_id, |row| row.last_used = index)
            .expect("order the cached package");
    }

    let started = std::time::Instant::now();
    let usage = store.disk_usage().expect("measure the store");
    let measured = started.elapsed();
    println!("disk_usage({PACKAGES} packages): {measured:?}");

    // Each package owns one PAYLOAD_KIB blob; everything else is shared.
    let own = u64::try_from(PAYLOAD_KIB * 1024).expect("payload size fits");
    let cap = usage
        .total_bytes
        .saturating_sub(own.saturating_mul(EVICTED))
        .saturating_add(own.checked_div(2).expect("non-zero divisor"));
    let started = std::time::Instant::now();
    let removed = store.sweep(cap).expect("sweep the store");
    let swept = started.elapsed();
    assert_eq!(
        u64::try_from(removed.len()).expect("count fits"),
        EVICTED,
        "{removed:?}"
    );
    println!("sweep({PACKAGES} packages, evicting {EVICTED}): {swept:?}");
}
