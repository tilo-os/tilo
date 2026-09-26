// Minimal fixture: print hello, exit 0. Used by the `hello_world_runs` phase of `apps_launch_unless_a_required_service_is_missing`.
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
