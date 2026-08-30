// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

#[path = "render_smoke.rs"]
mod render_smoke;
#[path = "shots_smoke.rs"]
mod shots_smoke;
