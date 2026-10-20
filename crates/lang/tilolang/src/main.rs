//! `tilolangc` (short: `tlc`): check, build, format and lock the ids of tilolang packages. See
//! [`tilolang::cli`].
//!
//! `run` needs the platform and lives in the full build tool, `tilolang` (the host app's binary,
//! `apps/tilolang`), which runs every command here as well.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    tilolang::cli::main(&args, None)
}
