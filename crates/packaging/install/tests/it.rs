// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "runtime_cli.rs"]
mod runtime_cli;
#[path = "trust.rs"]
mod trust;
