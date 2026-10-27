//! What the interactions a person feels cost Calendar on the UI thread: a wheel
//! notch, a keystroke, a view switch and a click on an item, each through
//! the production host path on the app's capture fixture at the Yoga's
//! logical size, with the frame passes and named scopes each step ran.
//! These report: the numbers say where the frame goes. A sidebar hover is
//! held to the two rows it moved between and the pointer-motion budget.
//!
//! `cargo test -p calendar --profile fast --test perf_interaction -- --nocapture`
#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

#[test]
fn interaction_costs() {
    ui::framework::testing::perf::profile::<calendar::CalendarShell>(None)
        .expect("calendar opens its capture");
}

/// What opening calendar costs on the UI thread, warm (its record already on the
/// profile): from the host preparing the launch to the first frame drawn.
#[test]
fn start_cost() {
    ui::framework::testing::perf::start::<calendar::CalendarShell>(8)
        .expect("calendar opens")
        .print();
}

/// Moving the pointer down the sidebar and back: each motion moves the
/// hover from one row to the next, which repaints those two rows and
/// nothing else, asks for no layout, and costs no more than any other
/// pointer motion.
#[test]
fn sidebar_hover() {
    ui::framework::testing::perf::sidebar_sweep::<calendar::CalendarShell>(2)
        .expect("calendar opens")
        .print()
        .assert_each_hover_repaints_only_its_rows();
}
