//! `/usr/libexec/tilo-staging-handoff`: see the library module.

use std::ffi::OsString;
use std::process::ExitCode;

use tilo_install_helpers::staging_handoff;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    staging_handoff::main(&staging_handoff::System, &args)
}
