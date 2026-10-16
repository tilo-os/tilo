//! `/usr/libexec/tilo-vm-bootc-copy-cache`: see the library module.

use std::ffi::OsString;
use std::process::ExitCode;

use tilo_install_helpers::bootc_copy_cache;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    bootc_copy_cache::main(&bootc_copy_cache::System, &args)
}
