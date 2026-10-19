//! What the interactions a person feels cost Store on the UI thread: a wheel
//! notch, a keystroke, a view switch and a click on an item, each through
//! the production host path on the app's capture fixture at the Yoga's
//! logical size, with the frame passes and named scopes each step ran.
//! Reports only: the numbers say where the frame goes.
//!
//! `cargo test -p store --profile fast --test perf_interaction -- --nocapture`
#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

#[test]
fn interaction_costs() {
    ui::framework::testing::perf::profile::<store::StoreShell>(None)
        .expect("store opens its capture");
}
