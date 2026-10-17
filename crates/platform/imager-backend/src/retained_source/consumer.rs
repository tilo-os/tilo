//! Fixed consuming-helper role and opaque namespace custody.
use std::{ffi::OsString, io};

pub mod endpoint;
pub mod namespace;
#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
mod native_namespace;

/// Authenticate and run the canonical consuming helper. Arguments route only
/// its inherited transport; they cannot grant source or namespace authority.
/// # Errors
/// Returns an error if helper authentication, argument validation, bootstrap,
/// namespace setup or consumer execution fails, or if this host is unsupported.
pub fn run(arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        native::run(arguments)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = arguments;
        Err(super::Unsupported.into())
    }
}
