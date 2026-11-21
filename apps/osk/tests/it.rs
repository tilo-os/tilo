#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.


#[path = "render_smoke.rs"]
mod render_smoke;
#[path = "shots_smoke.rs"]
mod shots_smoke;
#[path = "terminal_row.rs"]
mod terminal_row;
#[path = "touch.rs"]
mod touch;
