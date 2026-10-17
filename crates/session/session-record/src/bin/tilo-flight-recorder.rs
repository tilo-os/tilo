//! Relays the compositor's output to stdout while keeping its bounded private
//! flight record: `tilo-flight-recorder RECORD [START PID SHELL]`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(recording) = tilo_session_record::recorder::Recording::from_args(&args) else {
        return ExitCode::from(2);
    };
    tilo_session_record::recorder::record(&recording, std::io::stdin(), std::io::stdout());
    ExitCode::SUCCESS
}
