//! The administrative Linux broker is the only producer of block-claim facts.
use std::io;
/// Administrative command entry point. Paths and caller-provided health/FD assertions are invalid.
///
/// # Errors
/// Refuses unsupported platforms and invalid or unavailable selections.
pub fn command(args: &[String]) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::command(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "production NTFS claims require Linux",
        ))
    }
}
#[cfg(target_os = "linux")]
mod linux;

#[cfg(any(target_os = "linux", test))]
mod topology;
