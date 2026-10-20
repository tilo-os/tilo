//! `tilolang`: check, build, format and lock the ids of tilolang packages. See [`tilolang::cli`].
//!
//! `run` needs the platform and lives in the host app's binary of the same name
//! (`apps/tilolang`), which runs every command here as well.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    tilolang::cli::main(&args, None)
}
