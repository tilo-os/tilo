//! What a keystroke costs a desk with six apps open: the CPU time it takes the
//! thread that runs it, and how many of the open apps worked on it.
//!
//! The desk is the browser's launch demo — Notes, Sheets, Writer, Calendar,
//! Calculator and Slides, one window each — with the keyboard in the note, so
//! the frame a key asks for is the one a person typing beside five other open
//! windows waits on. Everything runs through the production frame path
//! ([`ui::headless::FrameHarness`]) with no GPU, so the number is the CPU a
//! key owes, not what a display took to show it.
//!
//! Run: `cargo run --release -p tilo-desktop --example typing-perf [-- --keys N]`

#![allow(
    clippy::print_stdout,
    clippy::expect_used,
    reason = "a benchmark reports to stdout and treats a failed setup as fatal"
)]

#[path = "../tests/typing_desktop/mod.rs"]
mod typing_desktop;

use typing_desktop::DemoDesk;

fn main() {
    let keys: usize = std::env::args()
        .skip_while(|arg| arg != "--keys")
        .nth(1)
        .and_then(|count| count.parse().ok())
        .unwrap_or(40);
    let mut desk = DemoDesk::open();
    desk.focus_the_note();
    // Unmeasured: the first keys find the caches a typist's do.
    for _ in 0..8 {
        desk.type_key("l");
    }
    let mut costs = Vec::with_capacity(keys);
    for index in 0..keys {
        let key = if index % 10 == 9 { " " } else { "l" };
        costs.push(desk.measure_key(key));
    }
    let mut cpu: Vec<u64> = costs.iter().map(|cost| cost.cpu_us).collect();
    cpu.sort_unstable();
    let median = cpu.get(cpu.len() / 2).copied().unwrap_or(0);
    let worst = cpu.last().copied().unwrap_or(0);
    println!("typing into the note, {} keys, six apps open", costs.len());
    println!(
        "  cpu per key   p50 {:.2} ms   max {:.2} ms",
        ms(median),
        ms(worst)
    );
    let passes: usize = costs.iter().map(|cost| cost.passes).sum();
    let declarations: usize = costs.iter().map(|cost| cost.declarations).sum();
    println!(
        "  per key       {:.2} action passes   {:.2} declaration builds",
        passes as f64 / costs.len() as f64,
        declarations as f64 / costs.len() as f64,
    );
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}
