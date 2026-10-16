//! `/usr/libexec/tilo-vm-install-proof`: see the library module.

use std::ffi::OsString;
use std::process::ExitCode;

use tilo_install_helpers::install_proof;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    tilo_install_helpers::main(&install_proof::COMMAND, &args, |values| {
        install_proof::run(&install_proof::System, values)
    })
}
