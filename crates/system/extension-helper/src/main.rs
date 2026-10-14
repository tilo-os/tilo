//! `/usr/libexec/tilo-extension`: see the library for the commands.

use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = tilo_extension::main(
        &tilo_extension::System,
        &tilo_extension::Layout::default(),
        &argv,
    );
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
