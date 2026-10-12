//! Descriptor-only OS operations, independent of deserialized policy identities.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

use super::invalid;

/// Read the literal target of an already pinned link, never a path from the wire.
pub(super) fn read_pinned_link(pin: &File, limit: usize) -> io::Result<Vec<u8>> {
    // One extra byte detects truncation at the consumer's target-size bound.
    let capacity = limit
        .checked_add(1)
        .ok_or_else(|| invalid("autostart link limit overflow"))?;
    let mut bytes = vec![0u8; capacity];
    // Empty-path readlinkat reads the pinned link, even if its name changes.
    // SAFETY: pin owns a live fd, the empty C string is terminated, and the
    // writable buffer has bytes.len() bytes available for the duration of the call.
    let count = unsafe {
        libc::readlinkat(
            pin.as_raw_fd(),
            c"".as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    let count = usize::try_from(count).map_err(|_| invalid("invalid link length"))?;
    if count > limit {
        return Err(invalid("autostart link exceeds limit"));
    }
    bytes.truncate(count);
    Ok(bytes)
}
