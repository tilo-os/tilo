#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary: each tests/*.rs file linked the whole crate before. nextest
// runs each test in its own process, so #[path] keeps the files' isolation.

#[path = "cloud_mount.rs"]
mod cloud_mount;
#[path = "collab_release_matrix.rs"]
mod collab_release_matrix;
#[path = "corruption_matrix.rs"]
mod corruption_matrix;
// The licensed simulator is never part of an ordinary filesystem run.
#[cfg(feature = "rystic-tests")]
#[path = "gdrive_rystic_volume.rs"]
mod gdrive_rystic_volume;
#[path = "held_memory.rs"]
mod held_memory;
#[path = "integration_test.rs"]
mod integration_test;
#[path = "iroh_idle.rs"]
mod iroh_idle;
#[path = "performance_check.rs"]
mod performance_check;
#[path = "performance_timing.rs"]
mod performance_timing;
#[path = "session_media.rs"]
mod session_media;
#[path = "store_threads.rs"]
mod store_threads;
#[path = "workspace_ladder.rs"]
mod workspace_ladder;
#[path = "workspace_startup.rs"]
mod workspace_startup;
