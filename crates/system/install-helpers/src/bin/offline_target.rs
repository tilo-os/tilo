//! `/usr/libexec/tilo-installer-offline-target`: see the library module.

use std::ffi::OsString;
use std::process::ExitCode;

use tilo_install_helpers::offline_target;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    offline_target::main(&offline_target::System, &args)
}
