// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "common/mod.rs"]
mod common;

#[path = "double_click_open.rs"]
mod double_click_open;
#[path = "drag_drop_move.rs"]
mod drag_drop_move;
#[path = "parity.rs"]
mod parity;
#[path = "scroll.rs"]
mod scroll;
#[path = "shots_smoke.rs"]
mod shots_smoke;
