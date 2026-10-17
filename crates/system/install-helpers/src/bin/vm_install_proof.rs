//! `/usr/libexec/tilo-vm-install-proof`: see the library module.

use std::ffi::OsString;
use std::process::ExitCode;

use tilo_install_helpers::install_proof;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args.first().is_some_and(|word| word == "code-refresh") {
        return tilo_install_helpers::code_refresh::main(
            &tilo_install_helpers::code_refresh::System,
        );
    }
    tilo_install_helpers::main(&install_proof::COMMAND, &args, |values| {
        install_proof::run(&install_proof::System, values)
    })
}
