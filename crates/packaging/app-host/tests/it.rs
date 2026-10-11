// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "common/mod.rs"]
mod common;

#[path = "cap_n_proto_round_trip.rs"]
mod cap_n_proto_round_trip;
#[path = "gc_removes_tried_under_pressure.rs"]
mod gc_removes_tried_under_pressure;
#[path = "install_resolves_round_trip.rs"]
mod install_resolves_round_trip;
#[path = "pin_unpin_changes_state.rs"]
mod pin_unpin_changes_state;
#[path = "registry_roundtrip.rs"]
mod registry_roundtrip;
#[path = "socket_activation.rs"]
mod socket_activation;

#[path = "local_trust.rs"]
mod local_trust;
