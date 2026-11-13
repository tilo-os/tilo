// One test binary instead of one per file: each tests/*.rs linked this whole
// crate separately. #[path] keeps the files where they are; nextest runs each
// test in its own process, so the isolation separate binaries gave them stays.
//
// The workspace engine's integration tests join a workspace through its host's
// enrollment answer, which the fixtures and internals behind `test-support`
// carry: the one target is gated on it.

#[path = "common/mod.rs"]
mod common;

// Workspace documents are decoded on a worker of this executable: this binary is that worker.
tilo_automerge_isolate::test_worker!();

#[path = "bootstrap_growth.rs"]
mod bootstrap_growth;
#[path = "direct_actions.rs"]
mod direct_actions;
#[path = "direct_content.rs"]
mod direct_content;
#[path = "direct_held_writes.rs"]
mod direct_held_writes;
#[path = "direct_host_link.rs"]
mod direct_host_link;
#[path = "direct_lost_history.rs"]
mod direct_lost_history;
#[path = "direct_two_devices.rs"]
mod direct_two_devices;
#[path = "offers.rs"]
mod offers;
#[path = "personal_two_devices.rs"]
mod personal_two_devices;
#[path = "renewed_enrollment.rs"]
mod renewed_enrollment;
#[path = "renewed_membership.rs"]
mod renewed_membership;
#[path = "three_devices.rs"]
mod three_devices;
