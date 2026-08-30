#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::system_monitor::{SparkSample, SystemMonitor};

#[test]
fn system_monitor_collects_samples() {
    let mut m = SystemMonitor::new(60);
    m.refresh();
    m.refresh();
    assert!(!m.samples().is_empty());
    let s: &SparkSample = m.samples().last().unwrap();
    assert!(s.cpu_pct >= 0.0 && s.cpu_pct <= 100.0);
    assert!(s.ram_pct >= 0.0 && s.ram_pct <= 100.0);
}
