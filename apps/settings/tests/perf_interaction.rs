//! What the interactions a person feels cost Settings on the UI thread: a wheel
//! notch, a keystroke, a view switch and a click on an item, each through the
//! production host path on the app's capture fixture at the Yoga's logical
//! size, with the frame passes and named scopes each step ran.
#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

#[test]
fn interaction_costs() {
    ui::framework::testing::perf::profile::<settings::SettingsShell>(None)
        .expect("settings opens its capture");
}

#[test]
fn start_cost() {
    ui::framework::testing::perf::start::<settings::SettingsShell>(8)
        .expect("settings opens")
        .print();
}

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

#[test]
fn sidebar_hover() {
    ui::framework::testing::perf::sidebar_sweep::<settings::SettingsShell>(2)
        .expect("settings opens")
        .print()
        .assert_each_hover_repaints_only_its_rows();
}
