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
