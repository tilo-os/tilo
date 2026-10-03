//! Anonymous memory files: how bulk data crosses a process boundary
//! without copies through a socket.
//!
//! The writer creates a memory file, fills it and drops every writable
//! mapping. On Linux it then seals the memfd against writes and resizing,
//! and the reader verifies the seals before mapping it read-only, so a
//! sealed mapping cannot change under the reader. macOS has no sealing:
//! there [`map_sealed`] copies the bytes once into private memory, which
//! gives the reader the same guarantee at the price of one copy.
//!
//! macOS also rounds a shared memory object's size up to the page size, so
//! a file's length is only an upper bound. Every reader here takes the
//! meaningful length from its caller.

use std::fs::File;
use std::io;
use std::ops::Deref;
use std::os::fd::{AsFd, OwnedFd};

use memmap2::{Mmap, MmapMut, MmapOptions};

/// Create an empty anonymous memory file of at least `len` bytes. `name`
/// is a diagnostic label.
///
/// A file can be sized only once on macOS: create it at its final size.
///
/// # Errors
///
/// Creation or sizing failures.
pub fn create(name: &str, len: u64) -> io::Result<File> {
    sys::create(name, len)
}

/// A read-only memory file holding a copy of `bytes`, sealed where the
/// platform can seal.
///
/// # Errors
///
/// Creation, write or sealing failures.
pub fn from_bytes(name: &str, bytes: &[u8]) -> io::Result<OwnedFd> {
    let len = bytes.len() as u64;
    let file = create(name, len)?;
    if !bytes.is_empty() {
        let mut map = map_writable(&file)?;
        map.get_mut(..bytes.len())
            .ok_or_else(|| io::Error::other("memory file shorter than requested"))?
            .copy_from_slice(bytes);
        map.flush()?;
    }
    sys::seal(&file)?;
    Ok(file.into())
}

/// Bytes read from a memory file or regular file, truncated to the length
/// the caller vouched for.
pub struct Bytes {
    backing: Backing,
    len: usize,
}

enum Backing {
    Mapped(Mmap),
    Copied(Vec<u8>),
}

impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        let all: &[u8] = match &self.backing {
            Backing::Mapped(map) => map,
            Backing::Copied(bytes) => bytes,
        };
        all.get(..self.len).unwrap_or(all)
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

/// The first `len` bytes of a descriptor received from a less trusted
/// process, which cannot change afterwards: a verified sealed mapping on
/// Linux, a private copy where memory cannot be sealed.
///
/// # Errors
///
/// A descriptor that is not a sealed memfd (Linux), is shorter than `len`,
/// is empty, or cannot be mapped.
pub fn map_sealed(fd: &OwnedFd, len: u64) -> io::Result<Bytes> {
    let file = File::from(fd.as_fd().try_clone_to_owned()?);
    let actual = file.metadata()?.len();
    if actual < len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared buffer is shorter than announced",
        ));
    }
    if actual == 0 || len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared buffer is empty",
        ));
    }
    let len = usize::try_from(len).map_err(io::Error::other)?;
    if sys::verify_sealed(&file)? {
        // SAFETY: the file is sealed against writes and shrinking, so the
        // mapping can neither change nor be truncated under us. Populated up
        // front: the caller reads every byte, and one batched fault is far
        // cheaper than one per page.
        let map = unsafe { MmapOptions::new().populate().map(&file) }?;
        return Ok(Bytes {
            backing: Backing::Mapped(map),
            len,
        });
    }
    // SAFETY: the sender may still write this memory; the mapping is only
    // read once, to copy the bytes out, and nothing is inferred from them
    // until the copy is complete.
    let map = unsafe { MmapOptions::new().len(len).map(&file) }?;
    Ok(Bytes {
        backing: Backing::Copied(map.to_vec()),
        len,
    })
}

/// Map a file or memory file read-only, limited to `len` bytes when given
/// (a memory file's size can exceed its content). A concurrent truncation
/// by the sender can only fault this process, so use it where this process
/// is the confined one.
///
/// # Errors
///
/// Mapping failures, a file shorter than `len`, or no bytes.
pub fn map_document(file: &File, len: Option<u64>) -> io::Result<Bytes> {
    let actual = file.metadata()?.len();
    let len = len.unwrap_or(actual);
    if len == 0 || actual == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the document is empty",
        ));
    }
    if actual < len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the document is shorter than announced",
        ));
    }
    let len = usize::try_from(len).map_err(io::Error::other)?;
    // SAFETY: see the function documentation; the only party that can be
    // hurt by a changing file is this (confined) process.
    let map = unsafe { Mmap::map(file) }?;
    Ok(Bytes {
        backing: Backing::Mapped(map),
        len,
    })
}

/// Map a memory file this process shares with another read-only, for
/// memory the other side writes on request (a render target).
///
/// # Errors
///
/// Mapping failures.
pub fn map_shared(file: &File) -> io::Result<Mmap> {
    // SAFETY: the peer writes this memory only while serving a request the
    // caller made and waits for; callers treat the bytes as plain data.
    unsafe { Mmap::map(file) }
}

/// Map a memory file writable.
///
/// # Errors
///
/// Mapping failures.
pub fn map_writable(file: &File) -> io::Result<MmapMut> {
    // SAFETY: the file is a memory file shared only with a peer that reads
    // it after this side reports the write complete. Populated up front,
    // since the writer fills every byte.
    unsafe { MmapOptions::new().populate().map_mut(file) }
}

/// Reopen a regular file read-only, so a descriptor that also allows
/// writing is not handed to a less trusted process. `None` when the
/// platform cannot name the file (it was deleted, or is not a regular
/// file); callers then copy the bytes into a memory file.
#[must_use]
pub fn reopen_read_only(file: &File) -> Option<File> {
    sys::reopen_read_only(file)
}

#[cfg(target_os = "linux")]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    const SEALS: libc::c_int =
        libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE | libc::F_SEAL_SEAL;

    pub fn create(name: &str, len: u64) -> io::Result<File> {
        let cname = std::ffi::CString::new(name).map_err(io::Error::other)?;
        // SAFETY: `cname` is a valid NUL-terminated string for the call.
        let raw = unsafe {
            libc::memfd_create(cname.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a fresh descriptor we now own.
        let file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        file.set_len(len)?;
        Ok(file)
    }

    pub fn seal(file: &File) -> io::Result<()> {
        // SAFETY: F_ADD_SEALS on a descriptor we own; no pointers involved.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn verify_sealed(file: &File) -> io::Result<bool> {
        // SAFETY: F_GET_SEALS on a descriptor we own; no pointers involved.
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        let required = libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK;
        if seals < 0 || seals & required != required {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "shared buffer is not sealed",
            ));
        }
        Ok(true)
    }

    pub fn reopen_read_only(file: &File) -> Option<File> {
        File::open(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// `PSHMNAMLEN`: the longest name `shm_open` accepts.
    const MAX_NAME: usize = 31;
    /// Every object this crate creates is named under this prefix; the
    /// Seatbelt profile lets a confined helper create only these.
    pub const NAME_PREFIX: &str = "/tilo-";

    /// `name` is not used: shared memory names are global, so each object
    /// gets a unique one and is unlinked at once.
    pub fn create(_name: &str, len: u64) -> io::Result<File> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let len = libc::off_t::try_from(len).map_err(io::Error::other)?;
        for _ in 0..16 {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let mut path = format!("{NAME_PREFIX}{}-{serial}-{:x}", std::process::id(), nonce());
            path.truncate(MAX_NAME);
            let cpath = std::ffi::CString::new(path).map_err(io::Error::other)?;
            // SAFETY: `cpath` is a valid NUL-terminated string for the call.
            let raw = unsafe {
                libc::shm_open(
                    cpath.as_ptr(),
                    libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                    0o600 as libc::c_uint,
                )
            };
            if raw < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(err);
            }
            // SAFETY: shm_open returned a fresh descriptor we now own.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            // The object stays alive through the descriptor only.
            // SAFETY: `cpath` is a valid NUL-terminated string.
            unsafe { libc::shm_unlink(cpath.as_ptr()) };
            set_cloexec(&fd)?;
            // SAFETY: ftruncate on a descriptor we own.
            if unsafe { libc::ftruncate(fd.as_raw_fd(), len) } != 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(File::from(fd));
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free shared memory name",
        ))
    }

    fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
        // SAFETY: fcntl with integer arguments on a descriptor we own.
        let rc = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn nonce() -> u32 {
        let mut value = 0u32;
        // SAFETY: arc4random_buf fills exactly the bytes it is given.
        unsafe { libc::arc4random_buf((&raw mut value).cast(), size_of::<u32>()) };
        value
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the same signature as the Linux implementation, where it can fail"
    )]
    pub fn seal(_file: &File) -> io::Result<()> {
        Ok(())
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the same signature as the Linux implementation, where it can fail"
    )]
    pub fn verify_sealed(_file: &File) -> io::Result<bool> {
        Ok(false)
    }

    pub fn reopen_read_only(file: &File) -> Option<File> {
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes at most PATH_MAX bytes into `buf`.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
        if rc < 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0)?;
        buf.truncate(end);
        let path = std::path::PathBuf::from(
            <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(buf),
        );
        let reopened = File::open(path).ok()?;
        let same = |f: &File| {
            f.metadata().ok().map(|m| {
                use std::os::unix::fs::MetadataExt;
                (m.dev(), m.ino())
            })
        };
        (same(file)? == same(&reopened)?).then_some(reopened)
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
mod sys {
    use std::fs::File;
    use std::io;

    pub fn create(_name: &str, _len: u64) -> io::Result<File> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no anonymous memory files on this platform",
        ))
    }

    pub fn seal(_file: &File) -> io::Result<()> {
        Ok(())
    }

    pub fn verify_sealed(_file: &File) -> io::Result<bool> {
        Ok(false)
    }

    pub fn reopen_read_only(_file: &File) -> Option<File> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_file_round_trips_with_its_length() {
        let fd = from_bytes("t", b"hello").unwrap();
        let bytes = map_sealed(&fd, 5).unwrap();
        assert_eq!(&bytes[..], b"hello");
        let file = File::from(fd.try_clone().unwrap());
        assert_eq!(&map_document(&file, Some(5)).unwrap()[..], b"hello");
    }

    #[test]
    fn empty_and_short_buffers_are_refused() {
        let fd = from_bytes("t", b"hello").unwrap();
        assert!(map_sealed(&fd, 0).is_err());
        assert!(map_sealed(&fd, 1 << 30).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unsealed_memfd_is_refused() {
        let unsealed: OwnedFd = create("u", 4).unwrap().into();
        assert!(map_sealed(&unsealed, 4).is_err());
    }

    #[test]
    fn shared_mapping_sees_the_writers_bytes() {
        let file = create("pixels", 4096).unwrap();
        let reader = map_shared(&file).unwrap();
        let writer_file = File::from(file.as_fd().try_clone_to_owned().unwrap());
        let mut writer = map_writable(&writer_file).unwrap();
        writer[..4].copy_from_slice(b"rgba");
        assert_eq!(&reader[..4], b"rgba");
    }

    #[test]
    fn a_regular_file_reopens_read_only() {
        let dir = std::env::temp_dir().join(format!("tilo-sandbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.pdf");
        std::fs::write(&path, b"%PDF").unwrap();
        let rw = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let ro = reopen_read_only(&rw).expect("regular files reopen");
        let mut probe = &ro;
        assert!(std::io::Write::write(&mut probe, b"x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
