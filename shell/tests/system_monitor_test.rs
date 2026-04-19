use quick_desktop::shell::system_monitor::{SparkSample, SystemMonitor};

#[test]
fn system_monitor_collects_samples() {
    let mut m = SystemMonitor::new(60);
    m.refresh();
    m.refresh();
    assert!(m.samples().len() >= 1);
    let s: &SparkSample = m.samples().last().unwrap();
    assert!(s.cpu_pct >= 0.0 && s.cpu_pct <= 100.0);
    assert!(s.ram_pct >= 0.0 && s.ram_pct <= 100.0);
}
