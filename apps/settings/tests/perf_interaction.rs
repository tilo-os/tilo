//! What the interactions a person feels cost Settings on the UI thread: a wheel
//! notch, a keystroke, a view switch and a click on an item, each through
//! the production host path on the app's capture fixture at the Yoga's
//! logical size, with the frame passes and named scopes each step ran.
//! These report: the numbers say where the frame goes. A sidebar hover is
//! held to the two rows it moved between and the pointer-motion budget.
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
/// widget tree once.
#[test]
fn cold_start() {
    let start = ui::framework::testing::perf::cold_start(
        "settings",
        &tilo_test_env::package_binary!("settings"),
        8,
    );
    start.print();
    assert_eq!(
        start.calls("frame", "ui.rebuild_structure"),
        1,
        "the first frame builds the tree once"
    );
}

/// Moving the pointer down the sidebar and back: each motion moves the
/// hover from one row to the next, which repaints those two rows and
/// nothing else, asks for no layout, and costs no more than any other
/// pointer motion.
#[test]
fn sidebar_hover() {
    ui::framework::testing::perf::sidebar_sweep::<settings::SettingsShell>(2)
        .expect("settings opens")
        .print()
        .assert_each_hover_repaints_only_its_rows();
}
