//! Internal headless worker; launched only through the canonical service sandbox.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(target_os = "linux")]
    {
        linux(arguments)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = arguments;
        ExitCode::from(64)
    }
}

/// The worker's fixed service modes. The sandbox launches exactly one of them,
/// so the platform boundary is one function instead of one gate per mode.
#[cfg(target_os = "linux")]
fn linux(arguments: Vec<std::ffi::OsString>) -> ExitCode {
    let one = |expected: &str| arguments == [std::ffi::OsString::from(expected)];
    if one("--protocol") {
        println!(
            "{}",
            serde_json::json!({"schema":1,"worker_protocol":imager::home_export_worker_protocol()})
        );
        return ExitCode::SUCCESS;
    }
    if one("--tilo-retained-source-io") {
        return service(imager::run_retained_source_io_service());
    }
    if one("--tilo-retained-home-io") {
        return service(imager::run_retained_home_io_service());
    }
    if one("--tilo-retained-target-projection") {
        return service(imager::run_retained_target_projection_service());
    }
    if arguments.len() != 1 {
        return ExitCode::from(64);
    }
    service(imager::run_home_export_service())
}

#[cfg(target_os = "linux")]
fn service(result: std::io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
