//! A node's attributes as the tree keeps them. Where a mount presents the tree (Linux and macOS)
//! they are FUSE's own types, handed to the kernel unchanged. An Android app mounts nothing, so
//! there they are plain types with the same fields.

#[cfg(not(target_os = "android"))]
pub use fuser::{FileAttr, FileType, INodeNo};

#[cfg(target_os = "android")]
pub use plain::{FileAttr, FileType, INodeNo};

#[cfg(target_os = "android")]
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
