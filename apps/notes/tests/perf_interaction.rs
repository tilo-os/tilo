//! What the interactions a person feels cost Notes on the UI thread: a wheel
//! notch, a keystroke, a view switch and a click on an item, each through
//! the production host path on the app's capture fixture at the Yoga's
//! logical size, with the frame passes and named scopes each step ran.
//! Reports only: the numbers say where the frame goes.
//!
//! `cargo test -p notes --profile fast --test it perf_interaction -- --nocapture`
#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

#[test]
fn interaction_costs() {
    ui::framework::testing::perf::profile::<notes::NotesShell>(None)
        .expect("notes opens its capture");
}

/// What opening notes costs on the UI thread, warm (its record already on the
/// profile): from the host preparing the launch to the first frame drawn.
#[test]
fn start_cost() {
    ui::framework::testing::perf::start::<notes::NotesShell>(8)
        .expect("notes opens")
        .print();
}

/// What launching notes costs cold, one process per launch as on a device:
/// from the process starting to its first frame drawn, fonts and every
/// other per-process cache empty. The first frame builds the window's
/// widget tree once: its first layout lands on the tree it built.
#[test]
fn cold_start() {
    let start = ui::framework::testing::perf::cold_start(
        "notes",
        std::path::Path::new(env!("CARGO_BIN_EXE_notes")),
        8,
    );
    start.print();
    assert_eq!(
        start.calls("frame", "ui.rebuild_structure"),
        1,
        "the first frame builds the tree once"
    );
}
