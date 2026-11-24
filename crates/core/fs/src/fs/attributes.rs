//! How a mount presents an entry's attributes: owned as the person, sized as a
//! read serves it, and link-counted from the view rather than the tree.
//!
//! Every answer about an inode goes through here, because the kernel sets a
//! file's size from whichever answer it last took: a `lookup` that answered
//! another size than `getattr` let a path walk shrink the size under a mapping
//! sized by the `stat` before it.

use std::time::Duration;

use crate::node::{FileAttr, FileType};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use fuser::Request;

use super::{QuickFS, TTL, child_path};

impl QuickFS {
    /// `attr` as every answer about a file must give it: sized as a read serves it (a
    /// write stage's length) and a directory's visible link count. `lookup`, `getattr` and
    /// `readdirplus` all answer through here, because the kernel sets the file size from
    /// each of them: a `lookup` that answered another size than `getattr` let the path walk
    /// of an `open` shrink the size under a mapping sized by the `stat` before it, and git's
    /// index refresh died with SIGBUS reading the mapping.
    pub(super) fn live_attr(&self, mut attr: FileAttr, staged_len: Option<u64>) -> FileAttr {
        let ino = attr.ino.0;
        if let Some(len) = staged_len {
            attr.size = len;
            attr.blocks = len.div_ceil(512);
        }
        if attr.kind == FileType::Directory {
            attr.nlink = self.visible_dir_nlink(ino);
        }
        attr
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn presented_to(&self, req: &Request, mut attr: FileAttr) -> FileAttr {
        let (uid, gid) = self.owner.unwrap_or_else(|| (req.uid(), req.gid()));
        attr.uid = uid;
        attr.gid = gid;
        attr
    }
    /// How long the kernel may keep attributes answered for `ttl`. The kernel keeps one
    /// answer per inode for every caller, so attributes presented as the caller's are never
    /// kept: another account would be shown the previous caller as owner. With the person
    /// named, every caller gets the same answer, kept as long as any.
    pub(super) fn attr_ttl(&self, ttl: Duration) -> Duration {
        if self.owner.is_some() {
            ttl
        } else {
            Duration::ZERO
        }
    }
    pub(super) fn presented_attr(&self, ino: u64) -> Option<(FileAttr, Duration)> {
        let buf_len = self.staged_len(ino);
        let attr = self.ns().getattr(ino)?;
        Some((self.live_attr(attr, buf_len), TTL))
    }
    /// The link count to report for a directory: `2` plus one per subdirectory in the view,
    /// counted as `readdir` filters its listing, so `nlink` and `readdir` cannot disagree and a
    /// count from the real tree never announces hidden subdirectories. Only the subdirectories
    /// are read: every `lookup` and `getattr` of a folder asks this, and a folder being filled
    /// with files would otherwise be listed whole for each one.
    pub(super) fn visible_dir_nlink(&self, ino: u64) -> u32 {
        let (names, dir_path) = {
            let ns = self.ns();
            (ns.subfolder_names(ino), ns.path_for(ino))
        };
        let subdirs = match (names, &self.enforcement) {
            (None, _) => 0,
            (Some(names), None) => names.len(),
            (Some(names), Some((acl, identity))) => match dir_path {
                Some(dir) if acl.visible(identity, &dir) => names
                    .iter()
                    .filter(|name| acl.visible(identity, &child_path(&dir, name.as_bytes())))
                    .count(),
                _ => 0,
            },
        };
        2u32.saturating_add(u32::try_from(subdirs).unwrap_or(u32::MAX))
    }
}
