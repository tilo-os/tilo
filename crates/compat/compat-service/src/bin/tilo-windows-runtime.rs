fn main() -> std::process::ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    match compat_service::run_translated_command(&arguments) {
        Ok(code) => std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("tilo-windows-runtime: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
