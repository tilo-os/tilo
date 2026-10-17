//! Retained-source worker roles and their owned consumer lifecycle.
//!
//! These entry points accept no source paths or source descriptors. Only the
//! canonical signed launcher's independently authenticated fixed role admits
//! inherited channels. Unsupported hosts fail explicitly before touching them.
use std::{error::Error, fmt, io};

pub mod consumer;
pub mod deadline;
#[cfg(target_os = "linux")]
mod handoff;
pub mod launch_request;
#[cfg(target_os = "linux")]
mod native;
pub mod projection;
pub mod records;
#[cfg(target_os = "linux")]
mod target_projection;
pub mod workspace;

/// The host cannot run the native retained-source service roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unsupported;

impl fmt::Display for Unsupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("retained-source services are unsupported on this host")
    }
}
impl Error for Unsupported {}

impl From<Unsupported> for io::Error {
    fn from(error: Unsupported) -> Self {
        Self::new(io::ErrorKind::Unsupported, error)
    }
}

/// Run the source role over the canonical launcher's fixed projection.
/// # Errors
/// Returns an error if launcher authentication, source admission, provider IO or
/// retirement fails, or if retained-source services are unsupported on this host.
pub fn run(home: bool) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        native::run(home)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = home;
        Err(Unsupported.into())
    }
}

/// Run the target-credential FUSE worker over its already admitted channel.
/// # Errors
/// Returns an error if bootstrap or credential validation, provider setup, FUSE
/// service or cleanup fails, or if this host is unsupported.
pub fn run_target_projection() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        target_projection::run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(Unsupported.into())
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn worker_roles_fail_with_typed_unsupported() {
        for result in [run(false), run(true), run_target_projection()] {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(error.get_ref().unwrap().is::<Unsupported>());
        }
    }
}
