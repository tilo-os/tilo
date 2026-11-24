//! Anonymous memory files: how bulk data crosses a process boundary without copies through a
//! socket. The writer creates a memory file, fills it and drops every writable mapping. On Linux it
//! then seals the memfd against writes and resizing, and the reader verifies the seals before
//! mapping it read-only, so a sealed mapping cannot change under the reader. macOS has no sealing:
//! there [`Mapping::map_sealed`] copies the bytes once into private memory, which gives the reader
//! the same guarantee at the price of one copy. macOS also rounds a shared memory object's size up to
//! the page size, so a file's length is only an upper bound. Every reader here takes the meaningful
//! length from its caller.
//!
//! Windows has no memory files: a buffer there is a pagefile-backed section ([`windows`]). Its
//! writer fills it and hands on only a handle that maps it read-only, after closing every handle
//! and view that could write it, so no process can change it again. A section cannot be resized,
//! so a view of it never faults under its reader either.
//!
//! A buffer that stays writable — the ring an app writes its frames into, a render target — is only
//! sealed against resizing: a mapping the reader holds can never turn into `SIGBUS` under it.

#![cfg(any(unix, windows))]

#[cfg(unix)]
use std::fs::File;
use std::io;
use std::ops::Deref;
#[cfg(unix)]
use std::os::fd::{AsFd, OwnedFd};

use memmap2::Mmap;
#[cfg(unix)]
use memmap2::{MmapMut, MmapOptions};

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::from_bytes;

/// The largest page size of a supported system: Apple silicon's 16 KiB.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const MAX_PAGE: usize = 16 * 1024;

#[cfg(unix)]
/// Create an empty anonymous memory file of at least `len` bytes. `name` is a diagnostic label. A
/// file can be sized only once on macOS: create it at its final size.
pub fn create(name: &str, len: u64) -> io::Result<File> {
    sys::create(name, len)
}

/// A read-only memory file holding a copy of `bytes`, sealed where the platform can seal.
#[cfg(unix)]
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

/// Seal `file` against shrinking, growing and further sealing, keeping it writable: a reader maps it
/// read-only, and its size can no longer change under that mapping.
#[cfg(unix)]
pub fn seal_sizes(file: &File) -> io::Result<()> {
    sys::seal_sizes(file)
}

/// Bytes read from a memory file or regular file, truncated to the length the caller vouched for.
pub struct Mapping {
    backing: Backing,
    len: usize,
}

enum Backing {
    #[cfg_attr(windows, allow(dead_code, reason = "Windows copies what it reads"))]
    Mapped(Mmap),
    /// A copy, where this system cannot seal memory.
    #[cfg_attr(
        any(target_os = "linux", target_os = "android"),
        allow(dead_code, reason = "Linux maps instead of copying")
    )]
    Copied(Vec<u8>),
}

impl Mapping {
    #[cfg(unix)]
    fn view(map: Mmap, len: usize) -> Self {
        Self {
            backing: Backing::Mapped(map),
            len,
        }
    }

    /// Map a whole regular file read-only, limited to `max_len` bytes: a font from the shared
    /// read-only view, which lives on the image's read-only `/usr` and so cannot shrink under the
    /// mapping.
    #[cfg(unix)]
    pub fn map_file(fd: &OwnedFd, max_len: usize) -> io::Result<Self> {
        let file = File::from(fd.as_fd().try_clone_to_owned()?);
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        let len = usize::try_from(metadata.len()).map_err(io::Error::other)?;
        if len == 0 || len > max_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a file's size is out of range",
            ));
        }
        // SAFETY: a fresh private read-only mapping of a file we hold, unmapped exactly once in
        // `Drop`.
        let map = unsafe { MmapOptions::new().map_copy_read_only(&file) }?;
        Ok(Self::view(map, len))
    }

    /// Map a file or memory file read-only, limited to `len` bytes when given (a memory file's size
    /// can exceed its content). A concurrent truncation by the sender can only fault this process,
    /// so use it where this process is the confined one.
    #[cfg(unix)]
    pub fn map_document(file: &File, len: Option<u64>) -> io::Result<Self> {
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
        // SAFETY: see the function documentation; the only party that can be hurt by a changing
        // file is this (confined) process.
        let map = unsafe { Mmap::map(file) }?;
        Ok(Self::view(map, len))
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `len` bytes at `offset`, or `None` outside the mapping.
    pub fn get(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        self.as_ref().get(offset..end)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
impl Mapping {
    /// Map `fd` read-only after checking it is a memfd sealed against resizing (and against writes
    /// when `immutable`), at least `min_len` bytes and at most `max_len`.
    pub fn map_sealed(
        fd: &impl AsFd,
        immutable: bool,
        min_len: usize,
        max_len: usize,
    ) -> io::Result<Self> {
        Self::sealed(
            File::from(fd.as_fd().try_clone_to_owned()?),
            immutable,
            min_len,
            max_len,
        )
    }

    fn sealed(file: File, immutable: bool, min_len: usize, max_len: usize) -> io::Result<Self> {
        sys::require_seals(&file, immutable)?;
        let len = usize::try_from(file.metadata()?.len()).map_err(io::Error::other)?;
        if len < min_len || len > max_len || len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a shared buffer's size is out of range",
            ));
        }
        // SAFETY: the file is sealed against shrinking, so the mapping can never be truncated under
        // us, and against writes when `immutable`. An immutable buffer is read whole, so it is
        // populated up front: one batched fault is far cheaper than one per page. A writable buffer
        // (the ring) is populated lazily, as the app writes it.
        let mut options = MmapOptions::new();
        if immutable {
            options.populate();
        }
        let map = unsafe { options.map(&file) }?;
        Ok(Self::view(map, len))
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
impl Mapping {
    /// The bytes of `fd`, at least `min_len` and at most `max_len` of them, copied once into this
    /// process's own read-only memory: nothing seals memory here, so a sender that kept it could
    /// still change it, but never the copy. A shared memory object's size is rounded up to whole
    /// pages, so it may exceed `max_len` by less than a page; what lies beyond `max_len` is not
    /// copied. A writable buffer (`immutable` false) needs seals and is refused.
    pub fn map_sealed(
        fd: &impl AsFd,
        immutable: bool,
        min_len: usize,
        max_len: usize,
    ) -> io::Result<Self> {
        Self::sealed(
            File::from(fd.as_fd().try_clone_to_owned()?),
            immutable,
            min_len,
            max_len,
        )
    }

    fn sealed(file: File, immutable: bool, min_len: usize, max_len: usize) -> io::Result<Self> {
        if !immutable {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a writable shared buffer needs sealed memory, which only Linux has",
            ));
        }
        let size = usize::try_from(file.metadata()?.len()).map_err(io::Error::other)?;
        let len = size.min(max_len);
        if size < min_len || len == 0 || size.saturating_sub(max_len) >= MAX_PAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a shared buffer's size is out of range",
            ));
        }
        // SAFETY: the sender may still write this memory; the mapping is only read once, to copy
        // the bytes out, and nothing is inferred from them until the copy is complete.
        let map = unsafe { MmapOptions::new().len(len).map(&file) }?;
        Ok(Self {
            backing: Backing::Copied(map.to_vec()),
            len,
        })
    }
}

impl Deref for Mapping {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        let all: &[u8] = match &self.backing {
            Backing::Mapped(map) => map,
            Backing::Copied(bytes) => bytes,
        };
        all.get(..self.len).unwrap_or(all)
    }
}

impl AsRef<[u8]> for Mapping {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

/// Map a memory file this process shares with another read-only, for memory the other side writes
/// on request (a render target).
#[cfg(unix)]
pub fn map_shared(file: &File) -> io::Result<Mmap> {
    // SAFETY: the peer writes this memory only while serving a request the caller made and waits
    // for; callers treat the bytes as plain data.
    unsafe { Mmap::map(file) }
}

#[cfg(unix)]
pub fn map_writable(file: &File) -> io::Result<MmapMut> {
    // SAFETY: the file is a memory file shared only with a peer that reads it after this side
    // reports the write complete. Pages fault in as they are written, so a buffer only as large as
    // what is used is charged to the writer.
    unsafe { MmapOptions::new().map_mut(file) }
}

/// Reopen a regular file read-only, so a descriptor that also allows writing is not handed to a
/// less trusted process. `None` when the platform cannot name the file (it was deleted, or is not a
/// regular file); callers then copy the bytes into a memory file.
#[cfg(unix)]
#[must_use]
pub fn reopen_read_only(file: &File) -> Option<File> {
    sys::reopen_read_only(file)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    /// Seals every buffer carries: its size can never change.
    const SIZE_SEALS: libc::c_int = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;

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
        add_seals(file, SIZE_SEALS | libc::F_SEAL_WRITE)
    }

    pub fn seal_sizes(file: &File) -> io::Result<()> {
        add_seals(file, SIZE_SEALS)
    }

    fn add_seals(file: &File, seals: libc::c_int) -> io::Result<()> {
        // SAFETY: F_ADD_SEALS on a descriptor we own; no pointers involved.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Whether `file` carries the seals `immutable` needs: a buffer whose size cannot change, and
    /// which cannot be written either.
    pub fn require_seals(file: &File, immutable: bool) -> io::Result<()> {
        let required = if immutable {
            SIZE_SEALS | libc::F_SEAL_WRITE
        } else {
            SIZE_SEALS
        };
        // SAFETY: F_GET_SEALS on a descriptor we own; no pointers involved.
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 {
            return Err(io::Error::last_os_error());
        }
        if seals & required != required {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a shared buffer must be a sealed memfd",
            ));
        }
        Ok(())
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
    /// Every object this crate creates is named under this prefix; the Seatbelt profile lets a
    /// confined helper create only these.
    pub const NAME_PREFIX: &str = "/tilo-";

    /// `name` is not used: shared memory names are global, so each object gets a unique one and is
    /// unlinked at once.
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
            // The object stays alive through the descriptor only. SAFETY: `cpath` is a valid
            // NUL-terminated string.
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

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the same signature as the Linux implementation, where it can fail"
    )]
    pub fn seal(_file: &File) -> io::Result<()> {
        Ok(())
    }

    /// Unlinked shared memory cannot be sealed, but nothing verifies sizes here: a writable buffer
    /// is only mapped on Linux.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the same signature as the Linux implementation, where it can fail"
    )]
    pub fn seal_sizes(_file: &File) -> io::Result<()> {
        Ok(())
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

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
mod sys {
    use std::fs::File;
    use std::io;

    pub fn create(_name: &str, _len: u64) -> io::Result<File> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no anonymous memory files on this platform",
        ))
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the signature of the platforms that seal memory, whose callers this shares"
    )]
    pub fn seal(_file: &File) -> io::Result<()> {
        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the signature of the platforms that seal memory, whose callers this shares"
    )]
    pub fn seal_sizes(_file: &File) -> io::Result<()> {
        Ok(())
    }

    pub fn reopen_read_only(_file: &File) -> Option<File> {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn unsealed_memfd_is_refused() {
        let unsealed: OwnedFd = create("u", 4).unwrap().into();
        assert!(Mapping::map_sealed(&unsealed, true, 4, 4).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_sizes_sealed_buffer_maps_read_only_while_its_writer_keeps_it() {
        let file = create("ring", 8).unwrap();
        seal_sizes(&file).unwrap();
        let fd: OwnedFd = file.as_fd().try_clone_to_owned().unwrap();
        let mut writer = map_writable(&file).unwrap();
        writer[..3].copy_from_slice(b"abc");
        let reader = Mapping::map_sealed(&fd, false, 8, 8).unwrap();
        assert_eq!(&reader[..3], b"abc");
        assert!(
            Mapping::map_sealed(&fd, true, 8, 8).is_err(),
            "a writable buffer is not an immutable one"
        );
    }

    tilo_test_env::scenario! {
        fn memory_files_map_at_their_length_share_writes_and_reopen_read_only;

        phase memory_file_round_trips_with_its_length;
        {
            let fd = from_bytes("t", b"hello").unwrap();
            let bytes = Mapping::map_sealed(&fd, true, 5, 5).unwrap();
            assert_eq!(&bytes[..], b"hello");
            let file = File::from(fd.try_clone().unwrap());
            assert_eq!(&Mapping::map_document(&file, Some(5)).unwrap()[..], b"hello");
        }

        phase empty_and_short_buffers_are_refused;
        {
            let fd = from_bytes("t", b"hello").unwrap();
            assert!(Mapping::map_sealed(&fd, true, 0, 0).is_err());
            assert!(Mapping::map_sealed(&fd, true, 1 << 30, 1 << 30).is_err());
        }

        phase shared_mapping_sees_the_writers_bytes;
        {
            let file = create("pixels", 4096).unwrap();
            let reader = map_shared(&file).unwrap();
            let writer_file = File::from(file.as_fd().try_clone_to_owned().unwrap());
            let mut writer = map_writable(&writer_file).unwrap();
            writer[..4].copy_from_slice(b"rgba");
            assert_eq!(&reader[..4], b"rgba");
        }

        phase a_regular_file_reopens_read_only;
        {
            let dir = std::env::temp_dir().join(format!("tilo-shm-{}", std::process::id()));
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
}
