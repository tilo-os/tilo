#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file: each tests/*.rs linked the whole
// app separately. #[path] keeps the files where they are. Nextest runs each
// test in its own process; under `cargo test` the files' tests are threads of
// one, so `perf_budget`, which needs the process to itself (the first window it
// times is its process's first, and it scopes the process's environment),
// takes one through `tilo_test_env::in_own_process`.

#[path = "chrome_alignment.rs"]
mod chrome_alignment;
#[path = "perf_budget.rs"]
mod perf_budget;
#[path = "private_launch.rs"]
mod private_launch;
