//! `/usr/libexec/tilo-board-successor`: see the library for the commands.

use std::process::ExitCode;

fn main() -> ExitCode {
    tilo_board_successor::command::install_cancellation();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = tilo_board_successor::main(&args);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
