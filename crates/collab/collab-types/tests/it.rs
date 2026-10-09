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

#[path = "canonical.rs"]
mod canonical;
#[path = "capability_model.rs"]
mod capability_model;
#[path = "caps.rs"]
mod caps;
#[path = "compaction.rs"]
mod compaction;
#[path = "invite.rs"]
mod invite;
#[path = "replay_defence.rs"]
mod replay_defence;
#[path = "role_table.rs"]
mod role_table;
#[path = "signing.rs"]
mod signing;
#[path = "validator.rs"]
mod validator;
#[path = "wire_stability.rs"]
mod wire_stability;
