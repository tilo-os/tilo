//! `tilolangc` (short: `tlc`): check, build, format and lock the ids of tilolang packages. See
//! [`tilolang::cli`].
//!
//! Building and running packages as Tilo apps is the language tool's, `tilolang`
//! (`crates/lang/tilolang-cli`), which runs every command here as well.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    tilolang::cli::main(&args)
}
