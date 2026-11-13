//! What the interactions a person feels cost Terminal on the UI thread, through the production host
//! path.
//!
//! `cargo test -p terminal --profile fast --test perf_interaction -- --nocapture`
#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

#[test]
fn interaction_costs() {
    ui::framework::testing::perf::profile::<terminal::TerminalShell>(None)
        .expect("terminal opens its capture");
}

#[test]
fn start_cost() {
    ui::framework::testing::perf::start::<terminal::TerminalShell>(8)
        .expect("terminal opens")
        .print();
}
