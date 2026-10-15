//! `/usr/libexec/tilo-image-policy-check`: see the library.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = tilo_image_policy_check::main(&args, &mut std::io::stdout(), &mut std::io::stderr());
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
