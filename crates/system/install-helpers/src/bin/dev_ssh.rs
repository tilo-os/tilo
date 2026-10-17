//! `/usr/libexec/tilo-install-dev-ssh`: see the library module.

use std::ffi::OsString;
use std::process::ExitCode;

use tilo_install_helpers::dev_ssh;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    tilo_install_helpers::main(&dev_ssh::COMMAND, &args, dev_ssh::run)
}
