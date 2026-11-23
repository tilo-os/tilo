//! A folder as the mount shows it: the listing the view allows, and the
//! requests a daemon answers.
//!
//! A listing is taken once and kept, so a folder's later pages do not build it
//! again. Its pages are one pass over the folder: while a listing is being read
//! the tree can be moving (a folder just filled is still being published), and
//! rebuilding for every page would ask for a thousand names once per page. A
//! page therefore continues the listing its first page took, and the next
//! listing asks again. An ACL change is different: a page must not show what
//! was just revoked, so that rebuilds even mid-listing.

use crate::node::FileType;
use tilo_lock::MutexRecover as _;

use super::{QuickFS, child_path};

/// A directory listing as the mount's identity sees it, with the revisions it was taken at:
/// the folder, the ACL, and the tree.
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

/// Whether every request's cost is named on stderr (`TILO_FS_FUSE_TIMING=1`), so a check can
/// price the requests a copy makes one by one instead of guessing at the sum.
fn timing_traced() -> bool {
    static TIMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TIMED.get_or_init(|| std::env::var_os("TILO_FS_FUSE_TIMING").is_some())
}

/// Times one answered request: it reports its own cost as it is dropped, after the reply is
/// sent. Nothing is timed or printed unless the trace is asked for.
pub(super) struct OpTimer {
    kind: &'static str,
    started: std::time::Instant,
}

impl OpTimer {
    pub(super) fn new(kind: &'static str) -> Self {
        Self {
            kind,
            started: std::time::Instant::now(),
        }
    }
}

impl Drop for OpTimer {
    fn drop(&mut self) {
        if timing_traced() {
            eprintln!(
                "fuse timing {} {}",
                self.kind,
                self.started.elapsed().as_micros()
            );
        }
    }
}

impl QuickFS {
    /// The entries of `ino` that belong in the view, as `readdir` would report them; `None`
    /// when `ino` is not a directory in the view. `.` and `..` always survive.
    pub(super) fn visible_entries(
        &self,
        ino: u64,
    ) -> Option<Vec<(u64, FileType, fs_protocol::FileNameV1)>> {
        self.listed(ino, None)
    }

    /// One page of a listing at `offset`: the first page of a listing is taken fresh, and its
    /// later pages continue it.
    pub(super) fn listing_page(
        &self,
        ino: u64,
        offset: u64,
    ) -> Option<Vec<(u64, FileType, fs_protocol::FileNameV1)>> {
        self.listed(ino, Some(offset))
    }

    fn listed(
        &self,
        ino: u64,
        page: Option<u64>,
    ) -> Option<Vec<(u64, FileType, fs_protocol::FileNameV1)>> {
        if !self.may_read_workspace() {
            return None;
        }
        // The revision is read before the folder is, so a cached page never asks the namespace
        // for a thousand names to throw them away.
        let revision = {
            let ns = self.ns();
            ns.indexed_document_revision().to_owned()
        };
        let acl_revision = self
            .enforcement
            .as_ref()
            .map_or(0, |(acl, _)| acl.revision());
        let key = (ino, acl_revision, revision);
        {
            let cache = self.readdir_cache.lock_recover();
            if let Some(cached) = cache.as_ref() {
                // A listing's continuation keeps the listing it started, even if the tree moved
                // on while its pages were being read; a later listing asks again.
                let continuing = page.is_some_and(|offset| offset > 0);
                let same_listing = cached.key.0 == ino && cached.key.1 == acl_revision;
                if cached.key == key || (continuing && same_listing) {
                    return Some(cached.entries.clone());
                }
            }
        }
        let (entries, dir_path) = {
            let mut ns = self.ns();
            (ns.readdir(ino), ns.path_for(ino))
        };
        let entries = entries?;
        // Without enforcement there is nothing to filter: the folder answers as it is.
        let visible = match (&self.enforcement, dir_path) {
            (None, _) => entries,
            (Some((acl, identity)), Some(dir_path)) => {
                if !acl.visible(identity, &dir_path) {
                    return None;
                }
                entries
                    .into_iter()
                    .filter(|(_, _, name)| {
                        name == "."
                            || name == ".."
                            || acl.visible(identity, &child_path(&dir_path, name.as_bytes()))
                    })
                    .collect()
            }
            (Some(_), None) => return None,
        };
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
