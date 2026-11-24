//! A node's attributes as the tree keeps them. Where a mount presents the tree (Linux and macOS)
//! they are FUSE's own types, handed to the kernel unchanged. Without FUSE (an Android app, Windows)
//! nothing is mounted, so there they are plain types with the same fields.

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use fuser::{FileAttr, FileType, INodeNo};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use plain::{FileAttr, FileType, INodeNo};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod plain {
    use std::time::SystemTime;

    /// What a node is.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub enum FileType {
        NamedPipe,
        CharDevice,
        BlockDevice,
        Directory,
        RegularFile,
        Symlink,
        Socket,
    }

    /// A node's number in the tree.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct INodeNo(pub u64);

    impl From<INodeNo> for u64 {
        fn from(ino: INodeNo) -> Self {
            ino.0
        }
    }

    /// A node's attributes, as `stat` reads them.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub struct FileAttr {
        pub ino: INodeNo,
        pub size: u64,
        pub blocks: u64,
        pub atime: SystemTime,
        pub mtime: SystemTime,
        pub ctime: SystemTime,
        pub crtime: SystemTime,
        pub kind: FileType,
        pub perm: u16,
        pub nlink: u32,
        pub uid: u32,
        pub gid: u32,
        pub rdev: u32,
        pub blksize: u32,
        pub flags: u32,
    }
}

/// A name as the tree keeps it (bytes) as the host's own: the same bytes on Unix; on Windows, whose
/// names are Unicode, the name read as UTF-8, with what is not UTF-8 replaced.
#[must_use]
pub fn host_name(name: &[u8]) -> std::borrow::Cow<'_, std::ffi::OsStr> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        std::borrow::Cow::Borrowed(std::ffi::OsStr::from_bytes(name))
    }
    #[cfg(not(unix))]
    {
        match String::from_utf8_lossy(name) {
            std::borrow::Cow::Borrowed(name) => {
                std::borrow::Cow::Borrowed(std::ffi::OsStr::new(name))
            }
            std::borrow::Cow::Owned(name) => std::borrow::Cow::Owned(name.into()),
        }
    }
}

/// The user and group a node is shown as owned by: this process's own, which on Unix the mount's
/// files are. Windows has no numeric owner; a node there is shown as owned by user and group 0.
#[must_use]
pub fn owner() -> (u32, u32) {
    #[cfg(unix)]
    {
        (
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw(),
        )
    }
    #[cfg(not(unix))]
    {
        (0, 0)
    }
}
