// One test binary instead of one per file: each tests/*.rs linked this whole
// crate separately. #[path] keeps the files where they are, and nextest runs
// each test in its own process, so the isolation separate binaries gave them
// stays. The content plane's filesystem half is what every one of them drives,
// so the one target carries the feature they were gated on.

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
#[path = "native_sync_bench.rs"]
mod native_sync_bench;
#[path = "native_tier1.rs"]
mod native_tier1;
#[path = "performance_gate.rs"]
mod performance_gate;
#[path = "performance_timing.rs"]
mod performance_timing;
#[path = "session_media.rs"]
mod session_media;
#[path = "store_threads.rs"]
mod store_threads;
#[path = "workspace_startup.rs"]
mod workspace_startup;
