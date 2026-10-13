//! Panics through the compositor's crash-log hook, to check what a field crash
//! report contains for a binary built the way packages build it:
//!
//! ```sh
//! cargo build --release -p shell --example crash_log_probe --no-default-features
//! XDG_CACHE_HOME=$(mktemp -d) target/release/examples/crash_log_probe
//! cat "$XDG_CACHE_HOME"/tilo/crashes/*.log
//! ```
//!
//! Every frame of the backtrace should name its function; `<unknown>` frames
//! mean the release binary lost its symbol table.

#[inline(never)]
fn reach_the_panic(depth: u32) {
    assert!(depth != 0, "crash_log_probe: deliberate panic");
    reach_the_panic(std::hint::black_box(depth.saturating_sub(1)));
}

fn main() {
    shell::crash_log::install("tilo crash log probe");
    reach_the_panic(std::hint::black_box(2));
}
