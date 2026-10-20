//! What the interactions a person feels cost Settings on the UI thread: a wheel
//! notch, a keystroke, a view switch and a click on an item, each through
//! the production host path on the app's capture fixture at the Yoga's
//! logical size, with the frame passes and named scopes each step ran.
//! Reports only: the numbers say where the frame goes.
//!
//! `cargo test -p settings --profile fast --test it perf_interaction -- --nocapture`
#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

#[test]
fn interaction_costs() {
    ui::framework::testing::perf::profile::<settings::SettingsShell>(None)
        .expect("settings opens its capture");
}

/// What opening settings costs on the UI thread, warm (its record already on the
/// profile): from the host preparing the launch to the first frame drawn.
#[test]
fn start_cost() {
    ui::framework::testing::perf::start::<settings::SettingsShell>(8)
        .expect("settings opens")
        .print();
}

/// What launching settings costs cold, one process per launch as on a device:
/// from the process starting to its first frame drawn, fonts and every
/// other per-process cache empty. The first frame builds the window's
/// widget tree once, and composes it once more against the window's first
/// allocation (History presents by width): never a third time.
#[test]
fn cold_start() {
    let start = ui::framework::testing::perf::cold_start(
        "settings",
        std::path::Path::new(env!("CARGO_BIN_EXE_settings")),
        8,
    );
    start.print();
    assert_eq!(
        start.calls("frame", "ui.rebuild_structure"),
        2,
        "the first frame builds the tree once and composes it once for its size"
    );
}
