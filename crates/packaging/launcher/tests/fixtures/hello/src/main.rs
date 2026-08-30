// Minimal fixture: print hello, exit 0. Used by `hello_world_runs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

fn main() {
    println!("hello");
}
