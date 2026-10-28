// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "render_smoke.rs"]
mod render_smoke;
#[path = "shots_smoke.rs"]
mod shots_smoke;
#[path = "terminal_row.rs"]
mod terminal_row;
#[path = "touch.rs"]
mod touch;
