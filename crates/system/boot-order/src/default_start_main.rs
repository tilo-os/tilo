//! `tilo-default-start`: see [`tilo_boot_order::default_start`].

use std::process::ExitCode;

fn main() -> ExitCode {
    let machine = tilo_boot_order::System;
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    ExitCode::from(
        u8::try_from(tilo_boot_order::default_start::main(
            &machine, &args, &mut out, &mut err,
        ))
        .unwrap_or(1),
    )
}
