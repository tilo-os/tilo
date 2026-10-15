//! `/usr/libexec/tilo-staging-reclaim`: see the library.

use std::process::ExitCode;

fn main() -> ExitCode {
    let code = tilo_staging_reclaim::main(&tilo_staging_reclaim::System);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
