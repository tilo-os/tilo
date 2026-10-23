//! Byte-stream connections to local Tilo services.
//!
//! On Unix a local service is a Unix-domain socket, reached by path or through
//! a descriptor the launcher passed down. Hosts without such sockets report
//! every connection as unavailable, so callers need no target checks. Streams
//! must be used from a tokio runtime context.

use std::io;
use std::path::Path;

/// Whether this host can reach local services at all.
pub const SUPPORTED: bool = cfg!(unix);

/// A connected local service stream.
#[cfg(unix)]
pub type Stream = tokio::net::UnixStream;
/// A connected local service stream; never produced on this host.
#[cfg(not(unix))]
pub type Stream = tokio::io::DuplexStream;

#[cfg_attr(
    not(unix),
    expect(
        clippy::unused_async,
        reason = "shares the signature of the Unix connect"
    )
)]
pub async fn connect(path: &Path) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(path).await
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "local service sockets are unavailable on this host",
        ))
    }
}

/// Why [`duplicate_service`] could not produce a stream.
#[derive(Debug)]
pub enum ServiceStreamError {
    /// No inherited descriptor exists for any of the names (always the case
    /// where [`SUPPORTED`] is `false`).
    Missing,
    /// The inherited descriptor could not be duplicated.
    Duplicate(io::Error),
    /// The duplicate is not a usable stream socket.
    Invalid(io::Error),
}

/// A fresh stream on a duplicate of the first inherited service descriptor
/// found among `names` (see [`crate::service_fd`]). The inherited descriptor
/// itself stays open for later connections.
///
/// # Errors
///
/// See [`ServiceStreamError`].
pub fn duplicate_service(names: &[&str]) -> Result<Stream, ServiceStreamError> {
    #[cfg(unix)]
    {
        use std::os::fd::BorrowedFd;

        let raw = names
            .iter()
            .find_map(|name| crate::service_fd(name))
            .ok_or(ServiceStreamError::Missing)?;
        // SAFETY: `service_fd` verified the inherited slot is open, and the
        // borrow ends before this function returns; only the duplicate is
        // owned.
        let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
        let owned = borrowed
            .try_clone_to_owned()
            .map_err(ServiceStreamError::Duplicate)?;
        let stream = std::os::unix::net::UnixStream::from(owned);
        stream
            .set_nonblocking(true)
            .map_err(ServiceStreamError::Invalid)?;
        tokio::net::UnixStream::from_std(stream).map_err(ServiceStreamError::Invalid)
    }
    #[cfg(not(unix))]
    {
        let _ = names;
        Err(ServiceStreamError::Missing)
    }
}
