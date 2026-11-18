//! Launching Notes in a fresh process, as on a device: every per-process cache
//! empty, the first frame drawn.
#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

/// The first frame builds the window's widget tree once: its first layout lands
/// on the tree it built, rather than building it again for each pass.
#[test]
fn cold_start_builds_the_widget_tree_once() {
    let start = ui::framework::testing::perf::cold_start(
        "notes",
        &tilo_test_env::package_binary!("notes"),
        2,
    );
    start.print();
    assert_eq!(
        start.calls("frame", "ui.rebuild_structure"),
        1,
        "the first frame builds the tree once"
    );
}
