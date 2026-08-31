// Minimal fixture: print hello, exit 0. Used by `hello_world_runs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

fn main() {
    println!("hello");
}
