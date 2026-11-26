#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file: each tests/*.rs linked this whole
// crate separately. #[path] keeps the files where they are; nextest runs each
// test in its own process, so the isolation separate binaries gave them stays.
//
// The workspace engine's integration tests join a workspace through its host's
// enrollment answer, which the fixtures and internals behind `test-support`
// carry: the one target depends on it.

#[path = "base_generations.rs"]
mod base_generations;
#[path = "common/mod.rs"]
mod common;

// Workspace documents are decoded on a worker of this executable: this binary is that worker.
tilo_automerge_isolate::test_worker!();

#[path = "adhoc_group.rs"]
mod adhoc_group;
#[path = "direct_access.rs"]
mod direct_access;
#[path = "direct_actions.rs"]
mod direct_actions;
#[path = "direct_held_writes.rs"]
mod direct_held_writes;
#[path = "direct_lost_history.rs"]
mod direct_lost_history;
#[path = "direct_opened.rs"]
mod direct_opened;
#[path = "direct_reach.rs"]
mod direct_reach;
#[path = "direct_two_devices.rs"]
mod direct_two_devices;
#[path = "renewed_enrollment.rs"]
mod renewed_enrollment;
#[path = "renewed_membership.rs"]
mod renewed_membership;
