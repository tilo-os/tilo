#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.

// The worker an isolate starts is this binary, run as this test.
tilo_automerge_isolate::test_worker!();

#[path = "admission_cost.rs"]
mod admission_cost;
#[path = "deferred.rs"]
mod deferred;
#[path = "fair_turns.rs"]
mod fair_turns;
#[path = "isolation.rs"]
mod isolation;
// Workers are processes only on Linux.
#[cfg(target_os = "linux")]
#[path = "program.rs"]
mod program;
