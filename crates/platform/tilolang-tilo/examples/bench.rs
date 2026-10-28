//! `cargo run --release -p tilolang-tilo --example bench -- <package dir> [options]`: the same
//! benchmark as `tilolang bench`, without building the host app.

/// Counted, so the records hold the allocations each scenario made.
#[global_allocator]
static ALLOCATOR: tilolang_tilo::bench::CountingAlloc = tilolang_tilo::bench::CountingAlloc;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    tilolang_tilo::bench::main(&args)
}
