#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.

// A test that reads a whole document's changes loads it in a worker, which
// this test binary serves.
tilo_automerge_isolate::test_worker!();

#[path = "canonical.rs"]
mod canonical;
#[path = "capability_model.rs"]
mod capability_model;
#[path = "caps.rs"]
mod caps;
#[path = "compaction.rs"]
mod compaction;
#[path = "replay_defence.rs"]
mod replay_defence;
#[path = "signing.rs"]
mod signing;
#[path = "validator.rs"]
mod validator;
