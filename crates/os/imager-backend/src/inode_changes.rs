//! Whether an open file's inode was written or had its metadata changed since
//! a point in time, by anyone, without relying on timestamps.
use std::io;

/// Whether an inode was written or had its metadata changed since the watch
/// began, by anyone. Timestamps alone can't say: a kernel without fine-grained
/// timestamps (before Linux 6.13) stamps a file at the timer tick's
/// resolution, so a same-length rewrite within a tick leaves the size, mtime
/// and ctime as they were. The kernel reports each such write to an inotify
/// watch on the inode itself.
pub struct InodeChanges {
    fd: std::os::fd::OwnedFd,
    changed: std::sync::atomic::AtomicBool,
}
impl InodeChanges {
    /// Starts watching the inode `file` has open.
    pub fn watch(file: &std::fs::File) -> io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: inotify_init1 takes only flags and returns a new descriptor or -1.
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a descriptor this call just created and nothing else owns.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // The open descriptor's /proc link names the inode it has open,
        // whatever its path names by now.
        let path = std::ffi::CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: both descriptors are open and `path` is a NUL-terminated string.
        let watch = unsafe {
            libc::inotify_add_watch(
                fd.as_raw_fd(),
                path.as_ptr(),
                libc::IN_MODIFY | libc::IN_ATTRIB | libc::IN_CLOSE_WRITE,
            )
        };
        if watch < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            changed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Whether any change was reported; once one was, always.
    pub fn changed(&self) -> io::Result<bool> {
        use std::os::fd::AsRawFd;
        use std::sync::atomic::Ordering;
        if self.changed.load(Ordering::Acquire) {
            return Ok(true);
        }
        let mut events = [0_u8; 4096];
        // SAFETY: the buffer is writable for its whole length and the descriptor is open.
        let read = unsafe {
            libc::read(
                self.fd.as_raw_fd(),
                events.as_mut_ptr().cast(),
                events.len(),
            )
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::WouldBlock {
                Ok(false)
            } else {
                Err(error)
            };
        }
        // Any event (a queue overflow included) is a change.
        let changed = read > 0;
        if changed {
            self.changed.store(true, Ordering::Release);
        }
        Ok(changed)
    }
}
