//! `/usr/libexec/tilo-install-handoff-copy`: see the library module.

use std::ffi::OsString;
use std::process::ExitCode;

use tilo_install_helpers::handoff_copy;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    tilo_install_helpers::main(&handoff_copy::COMMAND, &args, handoff_copy::run)
}
