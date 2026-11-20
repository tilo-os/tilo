//! A folder as the mount shows it: the listing the view allows, and the
//! requests a daemon answers.
//!
//! A listing is taken once per revision of the tree and the ACL and kept, so a
//! folder's later pages do not build it again.

use fuser::FileType;
use tilo_lock::MutexRecover as _;

use super::{QuickFS, child_path};

/// A directory listing as the mount's identity sees it, with the revision it was taken at.
pub(super) struct ReaddirCache {
    pub(super) key: (u64, u64, String),
    pub(super) entries: Vec<(u64, FileType, fs_protocol::FileNameV1)>,
}

/// Whether every answered request is named on stderr (`TILO_FS_FUSE_REQUESTS=1`), so the
/// requests one listing costs can be counted from a daemon's log.
pub(super) fn requests_traced() -> bool {
    static TRACED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACED.get_or_init(|| std::env::var_os("TILO_FS_FUSE_REQUESTS").is_some())
}

pub(super) fn note_request(kind: &str) {
    if requests_traced() {
        eprintln!("fuse request {kind}");
    }
}

impl QuickFS {
    /// The entries of `ino` that belong in the view, as `readdir` would report them; `None`
    /// when `ino` is not a directory in the view. `.` and `..` always survive.
    pub(super) fn visible_entries(
        &self,
        ino: u64,
    ) -> Option<Vec<(u64, FileType, fs_protocol::FileNameV1)>> {
        if !self.may_read_workspace() {
            return None;
        }
        // A listing is good while nothing in the tree or the ACL changed: a folder's pages then
        // pay for it once, not once per page. The revision is read before the folder is, so a
        // cached page never asks the namespace for a thousand names to throw them away.
        let revision = {
            let ns = self.ns();
            ns.indexed_document_revision().to_owned()
        };
        let key = self
            .enforcement
            .as_ref()
            .map(|(acl, _)| (ino, acl.revision(), revision));
        {
            let cache = self.readdir_cache.lock_recover();
            if let Some(key) = &key
                && let Some(cached) = cache.as_ref()
                && cached.key == *key
            {
                return Some(cached.entries.clone());
            }
        }
        let (entries, dir_path) = {
            let mut ns = self.ns();
            (ns.readdir(ino), ns.path_for(ino))
        };
        let entries = entries?;
        // Without enforcement there is no ACL revision to key a listing on, and nothing to
        // filter: the folder answers as it is.
        let (Some((acl, identity)), Some(key)) = (&self.enforcement, key) else {
            return Some(entries);
        };
        let dir_path = dir_path?;
        if !acl.visible(identity, &dir_path) {
            return None;
        }
        let visible: Vec<_> = entries
            .into_iter()
            .filter(|(_, _, name)| {
                name == "."
                    || name == ".."
                    || acl.visible(identity, &child_path(&dir_path, name.as_bytes()))
            })
            .collect();
        {
            let mut cache = self.readdir_cache.lock_recover();
            cache.replace(ReaddirCache {
                key,
                entries: visible.clone(),
            });
        }
        Some(visible)
    }
}
