//! What the interactions a person feels cost Monitor on the UI thread: a wheel
//! notch, a keystroke, a view switch and a click, each through the production host
//! path on the capture fixture. Reports only.
#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

#[test]
fn interaction_costs() {
    ui::framework::testing::perf::profile::<monitor::MonitorShell>(None)
        .expect("monitor opens its capture");
}

#[test]
fn start_cost() {
    ui::framework::testing::perf::start::<monitor::MonitorShell>(8)
        .expect("monitor opens")
        .print();
}
