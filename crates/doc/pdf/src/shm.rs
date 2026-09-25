//! Memory files: how document bytes, rendered pixels and exported PDFs
//! cross the process boundary without copies through the socket.
//!
//! The writer creates a memfd, fills it, drops every writable mapping and
//! seals it against writes and resizing; the reader verifies the seals
//! before mapping it read-only. A sealed mapping cannot change under the
//! reader, so it can be handed out as ordinary immutable bytes.

use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

use memmap2::{Mmap, MmapMut, MmapOptions};

const SEALS: libc::c_int =
    libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE | libc::F_SEAL_SEAL;

/// Create an empty, sealable memfd of `len` bytes.
///
/// # Errors
///
/// `memfd_create` or `ftruncate` failures.
pub fn create(name: &str, len: u64) -> io::Result<File> {
    let cname = std::ffi::CString::new(name).map_err(io::Error::other)?;
    // SAFETY: `cname` is a valid NUL-terminated string for the call.
    let raw =
        unsafe { libc::memfd_create(cname.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned a fresh descriptor we now own.
    let file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.set_len(len)?;
    Ok(file)
}

/// Seal `file` read-only. Every writable mapping must already be gone.
///
/// # Errors
///
/// `fcntl` failures (for example a live writable mapping).
pub fn seal(file: &File) -> io::Result<()> {
    // SAFETY: F_ADD_SEALS on a descriptor we own; no pointers involved.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A sealed memfd holding a copy of `bytes`.
///
/// # Errors
///
/// Creation, write or sealing failures.
pub fn from_bytes(name: &str, bytes: &[u8]) -> io::Result<OwnedFd> {
    let mut file = create(name, 0)?;
    file.write_all(bytes)?;
    seal(&file)?;
    Ok(file.into())
}

/// Map a received descriptor read-only after checking it is sealed against
/// writes and shrinking and holds at least `min_len` bytes.
///
/// # Errors
///
/// A descriptor that is not a sealed memfd, is too short, or cannot be
/// mapped.
pub fn map_sealed(fd: &OwnedFd, min_len: u64) -> io::Result<Mmap> {
    // SAFETY: F_GET_SEALS on a descriptor we own; no pointers involved.
    let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
    let required = libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK;
    if seals < 0 || seals & required != required {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared buffer is not sealed",
        ));
    }
    let file = File::from(fd.as_fd().try_clone_to_owned()?);
    let len = file.metadata()?.len();
    if len < min_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared buffer is shorter than announced",
        ));
    }
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared buffer is empty",
        ));
    }
    // SAFETY: the file is sealed against writes and shrinking, so the
    // mapping can neither change nor be truncated under us. Populated up
    // front: the caller reads every byte, and one batched fault is far
    // cheaper than one per page.
    unsafe { MmapOptions::new().populate().map(&file) }
}

/// Map a file (or memfd) read-only for the engine. The helper maps the
/// client's document this way; a concurrent truncation can only fault the
/// sandboxed helper, which the client survives.
///
/// # Errors
///
/// Mapping failures, or an empty file.
pub fn map_document(file: &File) -> io::Result<Mmap> {
    if file.metadata()?.len() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the document is empty",
        ));
    }
    // SAFETY: see the function documentation; the only party that can be
    // hurt by a changing file is this (sandboxed) process.
    unsafe { Mmap::map(file) }
}

/// Map a freshly created memfd writable.
///
/// # Errors
///
/// Mapping failures.
pub fn map_writable(file: &File) -> io::Result<MmapMut> {
    // SAFETY: the memfd was created by this process and is not shared yet.
    // Populated up front, since the renderer writes every byte.
    unsafe { MmapOptions::new().populate().map_mut(file) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_memfd_round_trips_and_rejects_unsealed() {
        let fd = from_bytes("t", b"hello").unwrap();
        let map = map_sealed(&fd, 5).unwrap();
        assert_eq!(&map[..], b"hello");
        assert!(map_sealed(&fd, 6).is_err(), "shorter than announced");
        let unsealed: OwnedFd = create("u", 4).unwrap().into();
        assert!(map_sealed(&unsealed, 4).is_err());
    }
}
