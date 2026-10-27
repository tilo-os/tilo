//! Target-user prepared setup consumer; no UI initialization or activation.
#[cfg(target_os = "linux")]
#[path = "tilo-prepared-setup/linux.rs"]
mod linux;

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    {
        linux::run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("Prepared setup import is unavailable on this platform.");
        std::process::ExitCode::FAILURE
    }
}
