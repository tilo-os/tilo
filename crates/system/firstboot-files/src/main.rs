//! `/usr/libexec/tilo-firstboot-files`: see the library for the commands.

use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let outcome = {
        let mut stdin = std::io::stdin().lock();
        tilo_firstboot_files::main(&tilo_firstboot_files::System, &args, &mut stdin)
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-firstboot-files: {error}");
            ExitCode::FAILURE
        }
    }
}
