//! Internal headless worker; launched only through the canonical service sandbox.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(target_os = "linux")]
    if std::env::args_os().skip(1).collect::<Vec<_>>() == [std::ffi::OsString::from("--protocol")] {
        println!(
            "{}",
            serde_json::json!({"schema":1,"worker_protocol":imager::home_export_worker_protocol()})
        );
        return ExitCode::SUCCESS;
    }
    #[cfg(target_os = "linux")]
    if std::env::args_os().skip(1).collect::<Vec<_>>()
        == [std::ffi::OsString::from("--tilo-retained-source-io")]
    {
        return match imager::run_retained_source_io_service() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    #[cfg(target_os = "linux")]
    if std::env::args_os().skip(1).collect::<Vec<_>>()
        == [std::ffi::OsString::from("--tilo-retained-home-io")]
    {
        return match imager::run_retained_home_io_service() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    #[cfg(target_os = "linux")]
    if std::env::args_os().skip(1).collect::<Vec<_>>()
        == [std::ffi::OsString::from(
            "--tilo-retained-target-projection",
        )]
    {
        return match imager::run_retained_target_projection_service() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    if std::env::args_os().len() != 1 {
        return ExitCode::from(64);
    }
    #[cfg(target_os = "linux")]
    match imager::run_home_export_service() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
    #[cfg(not(target_os = "linux"))]
    ExitCode::from(64)
}
