#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.

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

#[path = "perf_interaction.rs"]
mod perf_interaction;

#[path = "a11y_row_activation.rs"]
mod a11y_row_activation;

#[path = "page_window_surface.rs"]
mod page_window_surface;
