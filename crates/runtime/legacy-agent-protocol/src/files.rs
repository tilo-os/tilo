//! Shared directories over vsock, for VMs whose hypervisor has no virtio-fs.
//!
//! A hosted VM (Firecracker, see `legacy-vm`'s `hosted` module) has no
//! virtio-fs device, so the guest agent mounts each shared directory as a
//! FUSE filesystem whose every operation is a request to the host:
//!
//! ```text
//! host  → MountFiles { shares }          guest → FilesMounted { paths }
//! guest connects to the share's host port, once per mounted share:
//! guest → FsRequest::Hello { path }      host  → FsReply::Ok | Error
//! guest → FsRequest::…                   host  → one FsReply each, in order
//! ```
//!
//! Each share has a port of its own, served by a host process confined to
//! that share alone. The host decides everything: it serves only the
//! directories it named,
//! resolves every path beneath the share's root, refuses writes to a
//! read-only share, and reports every file as the guest user's. Nodes are
//! the host's numbers for paths it has resolved; [`ROOT_NODE`] is the share
//! itself. Errors are `errno` values, as the guest kernel wants them.

use serde::{Deserialize, Serialize};

/// The host's vsock port of a VM's first share; the next ones count up.
pub const FILES_PORT_BASE: u32 = 7200;

/// The share's own directory.
pub const ROOT_NODE: u64 = 1;

/// The most a single read or write carries; FUSE asks for at most 128 KiB.
pub const MAX_IO_BYTES: u32 = 128 * 1024;

/// The most directory entries one reply carries.
pub const MAX_ENTRIES: usize = 256;

/// One host directory the guest mounts at the same path over vsock.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct FileShare {
    pub path: String,
    pub read_only: bool,
    /// The host's vsock port serving it.
    pub port: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    /// A socket, fifo or device: listed, never opened.
    Other,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Time {
    pub secs: i64,
    pub nanos: u32,
}

/// What `stat` says about a node, as the guest sees it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Attr {
    pub node: u64,
    pub kind: FileKind,
    /// Permission bits only (`0o7777`).
    pub perm: u16,
    pub size: u64,
    pub blocks: u64,
    pub nlink: u32,
    pub atime: Time,
    pub mtime: Time,
    pub ctime: Time,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub kind: FileKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SetTime {
    Now,
    At(Time),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StatFs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
}

/// Guest → host, on a file connection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FsRequest {
    /// The first request: which share this connection serves.
    Hello {
        path: String,
    },
    Lookup {
        parent: u64,
        name: Vec<u8>,
    },
    /// The guest kernel forgot a node; the host may forget it too.
    Forget {
        node: u64,
    },
    GetAttr {
        node: u64,
    },
    SetAttr {
        node: u64,
        mode: Option<u32>,
        size: Option<u64>,
        atime: Option<SetTime>,
        mtime: Option<SetTime>,
    },
    /// Up to [`MAX_ENTRIES`] entries from `offset` (an index into the
    /// directory as the host lists it, `.` and `..` excluded).
    ReadDir {
        node: u64,
        offset: u64,
    },
    Open {
        node: u64,
        write: bool,
        truncate: bool,
    },
    Read {
        handle: u64,
        offset: u64,
        size: u32,
    },
    Write {
        handle: u64,
        offset: u64,
        data: Vec<u8>,
    },
    Release {
        handle: u64,
    },
    Fsync {
        handle: u64,
    },
    Create {
        parent: u64,
        name: Vec<u8>,
        mode: u32,
        exclusive: bool,
        truncate: bool,
    },
    MkDir {
        parent: u64,
        name: Vec<u8>,
        mode: u32,
    },
    Unlink {
        parent: u64,
        name: Vec<u8>,
    },
    RmDir {
        parent: u64,
        name: Vec<u8>,
    },
    Rename {
        parent: u64,
        name: Vec<u8>,
        new_parent: u64,
        new_name: Vec<u8>,
        no_replace: bool,
    },
    Symlink {
        parent: u64,
        name: Vec<u8>,
        target: Vec<u8>,
    },
    ReadLink {
        node: u64,
    },
    Link {
        node: u64,
        new_parent: u64,
        new_name: Vec<u8>,
    },
    StatFs,
}

/// Host → guest: one per request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FsReply {
    Ok,
    Attr(Attr),
    Entries {
        entries: Vec<DirEntry>,
        done: bool,
    },
    Opened {
        handle: u64,
    },
    Created {
        attr: Attr,
        handle: u64,
    },
    Data(Vec<u8>),
    Written(u32),
    Target(Vec<u8>),
    StatFs(StatFs),
    /// An `errno`.
    Error(i32),
}

/// Whether `name` is one path component the host may act on: not empty,
/// not `.` or `..`, no `/` and no NUL.
#[must_use]
pub fn valid_component(name: &[u8]) -> bool {
    !name.is_empty()
        && name != b"."
        && name != b".."
        && !name.contains(&b'/')
        && !name.contains(&0)
        && name.len() <= 255
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_single_components_name_entries() {
        for good in [&b"a"[..], b"Cargo.toml", b".hidden", b"..."] {
            assert!(valid_component(good), "{good:?}");
        }
        for bad in [&b""[..], b".", b"..", b"a/b", b"/", b"a\0b"] {
            assert!(!valid_component(bad), "{bad:?}");
        }
        assert!(!valid_component(&[b'x'; 256]));
    }

    #[test]
    fn requests_round_trip_within_a_frame() {
        let request = FsRequest::Write {
            handle: 7,
            offset: 1 << 40,
            data: vec![0xab; MAX_IO_BYTES as usize],
        };
        let mut bytes = Vec::new();
        crate::send(&mut bytes, &request).unwrap();
        let back: FsRequest = crate::recv(&mut bytes.as_slice()).unwrap().unwrap();
        assert_eq!(back, request);
    }
}
