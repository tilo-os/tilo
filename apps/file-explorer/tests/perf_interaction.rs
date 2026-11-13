#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

//! What the interactions a person feels cost Files on its UI thread: a wheel notch, a
//! keystroke, a view switch, a click.

#[test]
fn interaction_costs() {
    ui::framework::testing::perf::profile::<file_explorer::FileExplorerShell>(None)
        .expect("file-explorer opens its capture");
}

#[test]
fn start_cost() {
    ui::framework::testing::perf::start::<file_explorer::FileExplorerShell>(8)
        .expect("file-explorer opens")
        .print();
}

#[test]
fn cold_start() {
    ui::framework::testing::perf::cold_start(
        "file-explorer",
        &tilo_test_env::package_binary!("file-explorer"),
        8,
    )
    .print();
}

#[test]
fn sidebar_hover() {
    let sweep = ui::framework::testing::perf::sidebar_sweep::<file_explorer::FileExplorerShell>(4)
        .expect("file-explorer opens");
    sweep.print();
    sweep.assert_each_hover_repaints_only_its_rows();
}
