//! Checked wrappers over the Linux syscalls the data plane uses and that
//! neither `std` nor `libc` wrap safely.
//!
//! Everything unsafe in the crate lives here or in [`crate::fanotify`]. Each
//! wrapper validates its inputs, owns the descriptors it creates and turns
//! `-1`/`errno` into [`io::Error`].

use std::ffi::{CString, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

pub(crate) fn cvt(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

pub(crate) fn cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

/// Opens `path` with raw `open(2)` flags; `O_CLOEXEC` is always added.
pub(crate) fn open(path: &Path, flags: libc::c_int) -> io::Result<OwnedFd> {
    let c = cstring(path)?;
    // SAFETY: `c` is a valid NUL-terminated string that outlives the call.
    let fd = cvt(unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC, 0o600) })?;
    // SAFETY: `open` returned a fresh descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub(crate) fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    open(path, libc::O_RDONLY | libc::O_DIRECTORY)
}

pub(crate) fn fd_path(fd: BorrowedFd<'_>) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

/// An ioctl request as the C library takes it: glibc's `unsigned long`, musl's
/// `int`. A request code with the top bit of its `int` set is the same 32 bits
/// either way, which is what the kernel reads.
fn request(code: u32) -> libc::Ioctl {
    #[cfg(not(target_env = "musl"))]
    {
        libc::Ioctl::from(code)
    }
    #[cfg(target_env = "musl")]
    {
        libc::Ioctl::from_ne_bytes(code.to_ne_bytes())
    }
}

/// Issues an ioctl whose argument is a pointer to `arg`.
///
/// # Safety
///
/// `code` must be an ioctl whose argument is a pointer to a value with the
/// exact layout of `T`.
pub(crate) unsafe fn ioctl_ptr<T>(
    fd: BorrowedFd<'_>,
    code: u32,
    arg: *mut T,
) -> io::Result<libc::c_int> {
    // SAFETY: the caller guarantees that `code` takes a `*mut T`.
    cvt(unsafe { libc::ioctl(fd.as_raw_fd(), request(code), arg) })
}

pub(crate) fn ioctl_int(
    fd: BorrowedFd<'_>,
    code: u32,
    arg: libc::c_int,
) -> io::Result<libc::c_int> {
    cvt(unsafe { libc::ioctl(fd.as_raw_fd(), request(code), arg) })
}

/// An opaque kernel file handle (`struct file_handle`), as produced by
/// `name_to_handle_at(2)` and reported by fanotify FID records.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FileHandle {
    pub(crate) handle_type: i32,
    pub(crate) bytes: Vec<u8>,
}

/// Size of the fixed `struct file_handle` header: `handle_bytes` + `handle_type`.
const FILE_HANDLE_HEADER: usize = 8;
/// `MAX_HANDLE_SZ` from `<linux/exportfs.h>`.
const MAX_HANDLE_SZ: usize = 128;

impl FileHandle {
    fn to_kernel(&self) -> io::Result<Vec<u32>> {
        let len = u32::try_from(self.bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file handle too long"))?;
        let words = FILE_HANDLE_HEADER
            .saturating_add(self.bytes.len())
            .div_ceil(4);
        let mut buf = vec![0u32; words];
        let mut raw = Vec::with_capacity(words.saturating_mul(4));
        raw.extend_from_slice(&len.to_ne_bytes());
        raw.extend_from_slice(&self.handle_type.to_ne_bytes());
        raw.extend_from_slice(&self.bytes);
        raw.resize(words.saturating_mul(4), 0);
        for (word, chunk) in buf.iter_mut().zip(raw.chunks_exact(4)) {
            let mut b = [0u8; 4];
            b.copy_from_slice(chunk);
            *word = u32::from_ne_bytes(b);
        }
        Ok(buf)
    }

    /// Returns the handle of `path` (relative to `dir`, or `dir` itself when
    /// `path` is empty) without following a final symlink.
    pub fn for_path(dir: BorrowedFd<'_>, path: &Path) -> io::Result<Self> {
        let c = cstring(path)?;
        let mut buf = vec![0u32; FILE_HANDLE_HEADER.saturating_add(MAX_HANDLE_SZ) / 4];
        if let Some(first) = buf.first_mut() {
            *first = u32::try_from(MAX_HANDLE_SZ).unwrap_or(128);
        }
        let mut mount_id: libc::c_int = 0;
        let flags = if path.as_os_str().is_empty() {
            libc::AT_EMPTY_PATH
        } else {
            0
        };
        // SAFETY: `buf` is a writable, aligned `struct file_handle` with room
        // for MAX_HANDLE_SZ bytes, and `c` is NUL-terminated.
        cvt(unsafe {
            libc::name_to_handle_at(
                dir.as_raw_fd(),
                c.as_ptr(),
                buf.as_mut_ptr().cast::<libc::file_handle>(),
                &raw mut mount_id,
                flags,
            )
        })?;
        let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).collect();
        Self::parse(&bytes)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed file handle"))
    }

    pub(crate) fn parse(raw: &[u8]) -> Option<Self> {
        let len = u32::from_ne_bytes(raw.get(0..4)?.try_into().ok()?) as usize;
        let handle_type = i32::from_ne_bytes(raw.get(4..8)?.try_into().ok()?);
        let bytes = raw
            .get(FILE_HANDLE_HEADER..FILE_HANDLE_HEADER.checked_add(len)?)?
            .to_vec();
        Some(Self { handle_type, bytes })
    }

    /// Opens the object this handle names. Needs `CAP_DAC_READ_SEARCH`.
    pub fn open(&self, mount: BorrowedFd<'_>, flags: libc::c_int) -> io::Result<OwnedFd> {
        let mut buf = self.to_kernel()?;
        // SAFETY: `buf` is a correctly laid out, aligned `struct file_handle`.
        let fd = cvt(unsafe {
            libc::open_by_handle_at(
                mount.as_raw_fd(),
                buf.as_mut_ptr().cast::<libc::file_handle>(),
                flags | libc::O_CLOEXEC,
            )
        })?;
        // SAFETY: a fresh descriptor that nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

pub(crate) const BTRFS_SUPER_MAGIC: libc::c_long = 0x9123_683E;

pub(crate) fn statfs_type(path: &Path) -> io::Result<libc::c_long> {
    // rustix types `f_type` as the kernel's signed word on every C library;
    // musl's own `statfs` types it unsigned.
    Ok(rustix::fs::statfs(path)?.f_type)
}

/// Reads an extended attribute without following symlinks; `Ok(None)` when the
/// attribute is absent.
pub(crate) fn get_xattr(path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    let c = cstring(path)?;
    let n = CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "xattr name contains NUL"))?;
    let mut buf = vec![0u8; 256];
    loop {
        // SAFETY: `buf` is writable for `buf.len()` bytes; strings are valid.
        let ret =
            unsafe { libc::lgetxattr(c.as_ptr(), n.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        if let Ok(len) = usize::try_from(ret) {
            buf.truncate(len);
            return Ok(Some(buf));
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ENODATA | libc::ENOTSUP) => return Ok(None),
            Some(libc::ERANGE) if buf.len() < 65536 => {
                buf.resize(buf.len().saturating_mul(4), 0);
            }
            _ => return Err(err),
        }
    }
}

pub(crate) fn fset_xattr(file: &File, name: &str, value: &[u8]) -> io::Result<()> {
    let n = CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "xattr name contains NUL"))?;
    // SAFETY: `value` is readable for its length; `n` is NUL-terminated.
    cvt(unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            n.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    })?;
    Ok(())
}

pub(crate) fn fset_xattr_fd(fd: BorrowedFd<'_>, name: &str, value: &[u8]) -> io::Result<()> {
    let n = CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "xattr name contains NUL"))?;
    // SAFETY: `value` is readable for its length; `n` is NUL-terminated.
    cvt(unsafe {
        libc::fsetxattr(
            fd.as_raw_fd(),
            n.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    })?;
    Ok(())
}

pub(crate) fn fremove_xattr(fd: BorrowedFd<'_>, name: &str) -> io::Result<()> {
    let n = CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "xattr name contains NUL"))?;
    // SAFETY: `n` is NUL-terminated.
    cvt(unsafe { libc::fremovexattr(fd.as_raw_fd(), n.as_ptr()) })?;
    Ok(())
}

pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let a = cstring(from)?;
    let b = cstring(to)?;
    // SAFETY: both strings are NUL-terminated and outlive the call.
    cvt(unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    })?;
    Ok(())
}

pub(crate) fn fget_xattr(fd: BorrowedFd<'_>, name: &str) -> io::Result<Option<Vec<u8>>> {
    let n = CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "xattr name contains NUL"))?;
    let mut buf = vec![0u8; 512];
    let ret = unsafe {
        libc::fgetxattr(
            fd.as_raw_fd(),
            n.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if let Ok(len) = usize::try_from(ret) {
        buf.truncate(len);
        return Ok(Some(buf));
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ENODATA | libc::ENOTSUP) => Ok(None),
        _ => Err(err),
    }
}

pub(crate) fn futimens(file: &File, mtime: libc::timespec) -> io::Result<()> {
    let times = [mtime, mtime];
    cvt(unsafe { libc::futimens(file.as_raw_fd(), times.as_ptr()) })?;
    Ok(())
}

pub(crate) fn fmtime(fd: BorrowedFd<'_>) -> io::Result<libc::timespec> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    cvt(unsafe { libc::fstat(fd.as_raw_fd(), &raw mut st) })?;
    Ok(libc::timespec {
        tv_sec: st.st_mtime,
        tv_nsec: st.st_mtime_nsec,
    })
}

pub(crate) fn set_fmtime(fd: BorrowedFd<'_>, mtime: libc::timespec) -> io::Result<()> {
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        mtime,
    ];
    cvt(unsafe { libc::futimens(fd.as_raw_fd(), times.as_ptr()) })?;
    Ok(())
}

pub(crate) fn punch_hole(file: &File, offset: u64, len: u64) -> io::Result<()> {
    let off = libc::off_t::try_from(offset)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset out of range"))?;
    let l = libc::off_t::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "length out of range"))?;
    cvt(unsafe {
        libc::fallocate(
            file.as_raw_fd(),
            libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
            off,
            l,
        )
    })?;
    Ok(())
}

pub struct Mmap {
    ptr: *mut libc::c_void,
    len: usize,
}

unsafe impl Send for Mmap {}
unsafe impl Sync for Mmap {}

impl Mmap {
    pub fn map(file: &File, len: u64) -> io::Result<Self> {
        let len = usize::try_from(len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file too large to map"))?;
        if len == 0 {
            return Ok(Self {
                ptr: std::ptr::null_mut(),
                len: 0,
            });
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            libc::madvise(ptr, len, libc::MADV_SEQUENTIAL);
        }
        Ok(Self { ptr, len })
    }

    pub fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.ptr.cast::<u8>(), self.len) }
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        if self.len != 0 {
            unsafe {
                libc::munmap(self.ptr, self.len);
            }
        }
    }
}

pub fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

pub fn has_capability(cap: u32) -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    let Some(hex) = status
        .lines()
        .find_map(|l| l.strip_prefix("CapEff:"))
        .map(str::trim)
    else {
        return false;
    };
    let Ok(bits) = u64::from_str_radix(hex, 16) else {
        return false;
    };
    bits.checked_shr(cap).is_some_and(|b| b & 1 == 1)
}

pub const CAP_SYS_ADMIN: u32 = 21;
pub const CAP_DAC_READ_SEARCH: u32 = 2;

pub(crate) fn os_string(bytes: &[u8]) -> OsString {
    OsString::from_vec(bytes.to_vec())
}
