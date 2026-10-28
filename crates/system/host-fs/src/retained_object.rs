//! Retain the object a local view observed, without reading its payload.
//! An observation is a stale-view precondition, never a filesystem grant.
use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObservedObject {
    device: u64,
    inode: u64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}
impl ObservedObject {
    /// The object observed by an existing metadata lookup. Unsupported hosts
    /// return None instead of inventing a path-based identity.
    pub fn from_metadata(metadata: &Metadata) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Some(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            None
        }
    }
    pub fn device(self) -> u64 {
        self.device
    }
    pub fn inode(self) -> u64 {
        self.inode
    }
}

/// An actual owned descriptor, kept from selection until the host's capture
/// reply. Only the host's existing grants and caller custody confer authority.
pub struct RetainedObject {
    file: File,
    observed: ObservedObject,
}
impl RetainedObject {
    /// Retain precisely the object whose metadata the view already observed.
    /// No-follow `O_PATH` (on macOS, an event-only no-follow open that refuses
    /// device nodes) avoids payload reads, hydration and special-file I/O.
    pub fn retain_shown(path: &Path, expected: ObservedObject) -> io::Result<Self> {
        let file = open_object(path)?;
        let observed = ObservedObject::from_metadata(&file.metadata()?)
            .ok_or_else(|| io::Error::other("object observations are unsupported"))?;
        if observed != expected {
            return Err(io::Error::other("shown filesystem object changed"));
        }
        Ok(Self { file, observed })
    }
    /// The process-local descriptor number. Its owner must remain alive until
    /// the actual host has duplicated it; this number grants nothing itself.
    pub fn descriptor(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            u32::try_from(self.file.as_raw_fd()).ok()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
    pub fn observed(&self) -> ObservedObject {
        self.observed
    }
    /// A live retained descriptor prevents inode reuse. Later metadata for
    /// that same object may change as the user edits it without replacing it.
    pub fn retains(&self, observation: ObservedObject) -> bool {
        self.observed.device == observation.device && self.observed.inode == observation.inode
    }
    /// Metadata of the actual retained object, independent of its current name.
    pub fn metadata(&self) -> io::Result<Metadata> {
        self.file.metadata()
    }
}

#[cfg(target_os = "linux")]
fn open_object(path: &Path) -> io::Result<File> {
    use std::os::fd::FromRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: NUL-terminated name; no-follow O_PATH returns an owned descriptor
    // without opening a file, device or FIFO for payload I/O.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful open transferred this fresh descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}
#[cfg(target_os = "macos")]
fn open_object(path: &Path) -> io::Result<File> {
    use std::os::fd::FromRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::FileTypeExt as _;
    // macOS has no O_PATH, so the object is opened for real. Opening a device
    // node runs its driver's open routine, which a selection must never do;
    // refuse those up front. A swap to a device after this check is caught by
    // the identity comparison in `retain_shown`.
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if kind.is_block_device() || kind.is_char_device() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "device nodes are not retained",
        ));
    }
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: NUL-terminated name; open returns an owned descriptor. O_SYMLINK
    // keeps a symlink itself rather than its target, O_NONBLOCK keeps a FIFO
    // from waiting for a writer, and O_EVTONLY keeps the descriptor from
    // holding the volume busy against an eject.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_SYMLINK | libc::O_NONBLOCK | libc::O_EVTONLY | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful open transferred this fresh descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_object(_: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "retained local objects require Linux or macOS",
    ))
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tests {
    use super::*;
    fn observation(path: &Path) -> io::Result<ObservedObject> {
        ObservedObject::from_metadata(&std::fs::symlink_metadata(path)?)
            .ok_or_else(|| io::Error::other("object identity unavailable"))
    }
    fn scratch() -> io::Result<std::path::PathBuf> {
        // macOS reads the clock in whole microseconds, so tests starting
        // together also need a per-process sequence number to stay apart.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tilo-retained-{}-{}-{}",
            std::process::id(),
            tilo_clock::try_unix_now_ns()
                .ok_or_else(|| io::Error::other("system clock is before the Unix epoch"))?,
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(path)
    }
    #[test]
    fn retained_object_refuses_replacement_before_selection() {
        let directory = scratch().expect("owned scratch directory");
        let path = directory.join("selected");
        std::fs::write(&path, b"original").expect("original");
        let observed = observation(&path).expect("observed identity");
        std::fs::rename(&path, directory.join("original")).expect("replacement");
        std::fs::write(&path, b"replacement").expect("replacement bytes");
        assert!(RetainedObject::retain_shown(&path, observed).is_err());
        std::fs::remove_dir_all(directory).expect("scratch cleanup");
    }
    #[test]
    fn retained_object_keeps_selected_identity_after_path_replacement() {
        let directory = scratch().expect("owned scratch directory");
        let path = directory.join("selected");
        std::fs::write(&path, b"original").expect("original");
        let observed = observation(&path).expect("observed identity");
        let retained = RetainedObject::retain_shown(&path, observed).expect("retain shown object");
        std::fs::rename(&path, directory.join("original")).expect("replace selected path");
        std::fs::write(&path, b"replacement").expect("replacement bytes");
        assert!(retained.descriptor().is_some());
        assert!(retained.retains(observed));
        assert!(!retained.retains(observation(&path).expect("replacement identity")));
        let actual =
            ObservedObject::from_metadata(&retained.metadata().expect("retained metadata"))
                .expect("actual retained identity");
        assert_eq!(
            (actual.device(), actual.inode()),
            (observed.device(), observed.inode())
        );
        assert!(retained.retains(actual));
        drop(retained);
        std::fs::remove_dir_all(directory).expect("scratch cleanup");
    }
    #[test]
    fn retained_object_keeps_symlink_without_opening_its_target() {
        let directory = scratch().expect("owned scratch directory");
        let path = directory.join("selected");
        // A missing target proves this observes the link itself, rather than
        // requiring payload access or hydration of what it points to.
        std::os::unix::fs::symlink("missing-target", &path).expect("link");
        let retained =
            RetainedObject::retain_shown(&path, observation(&path).expect("link identity"))
                .expect("retain the link itself");
        assert!(retained.metadata().expect("link metadata").is_symlink());
        assert!(!directory.join("missing-target").exists());
        drop(retained);
        std::fs::remove_dir_all(directory).expect("scratch cleanup");
    }
}
