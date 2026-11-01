// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "common/mod.rs"]
mod common;

#[path = "gc_removes_tried_under_pressure.rs"]
mod gc_removes_tried_under_pressure;
#[path = "install_resolves_round_trip.rs"]
mod install_resolves_round_trip;
#[path = "pin_unpin_changes_state.rs"]
mod pin_unpin_changes_state;
#[path = "socket_activation.rs"]
mod socket_activation;
#[path = "wire_round_trip.rs"]
mod wire_round_trip;

#[path = "local_trust.rs"]
mod local_trust;
