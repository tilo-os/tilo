//! How a FUSE session's end is read.
//!
//! Unmounting aborts the kernel connection, and a session thread blocked in
//! a read of the FUSE device can wake inside that abort: the kernel has
//! already disconnected the device's processing queue while a request (such
//! as the asynchronous `RELEASE` of a just-closed file) still sits in the
//! input queue. That read fails with `ECONNABORTED` even though the session
//! never negotiated `FUSE_ABORT_ERROR` (Linux `fuse_dev_do_read`), where the
//! usual unmount answer is `ENODEV`. fuser 0.18 ends the loop cleanly only
//! on `ENODEV` and hands the `ECONNABORTED` back as the session's error, so
//! an ordinary unmount intermittently reported failure. Either errno means
//! the connection is gone, which is how every session ends.

use std::io;

/// The session's result with an aborted connection read as a clean end.
///
/// # Errors
/// The session ended with any other error.
pub fn ended(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.raw_os_error() == Some(libc::ECONNABORTED) => Ok(()),
        other => other,
    }
}

/// Unmounts `session` and waits for its thread, reading an aborted
/// connection as the end it is.
///
/// # Errors
/// The unmount failed, or the session's thread ended with an error or
/// panicked.
pub fn unmount_and_join(session: fuser::BackgroundSession) -> io::Result<()> {
    ended(session.umount_and_join())
}
