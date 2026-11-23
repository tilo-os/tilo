#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "common/mod.rs"]
mod common;
#[path = "../src/local_package/fixture.rs"]
mod fixture;

#[path = "gc_removes_tried_under_pressure.rs"]
mod gc_removes_tried_under_pressure;
#[path = "install_entries_agree.rs"]
mod install_entries_agree;
#[path = "install_resolves_round_trip.rs"]
mod install_resolves_round_trip;
#[path = "language_packs.rs"]
mod language_packs;
#[path = "person_changes.rs"]
mod person_changes;
#[path = "pin_unpin_changes_state.rs"]
mod pin_unpin_changes_state;
#[path = "socket_activation.rs"]
mod socket_activation;
#[path = "wire_round_trip.rs"]
mod wire_round_trip;

#[path = "local_trust.rs"]
mod local_trust;
