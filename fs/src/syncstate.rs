//! Per-entry sync/hydration state, exposed to apps through the
//! `user.qos.sync` extended attribute.
//!
//! The FS projects remote metadata immediately but only hydrates content on
//! first read, so an app needs a way to tell local from remote from
//! mid-download. `getxattr(path, "user.qos.sync")` returns `local` /
//! `projected` / `downloading` — rides the existing mount, no qOS-specific
//! client needed, and is sandbox-safe: any process that can `stat` the file can read it, nothing more.

/// The xattr name the driver answers with an entry's sync state (`getxattr(path, "user.qos.sync")`).
pub const SYNC_XATTR: &str = "user.qos.sync";

/// The xattr name for a file's thumbnail preview. `ENODATA` when not previewable, not locally generatable, and not gossiped yet.
pub const THUMB_XATTR: &str = "user.qos.thumb";

/// The hydration state of one filesystem entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Fully present locally — a read never touches the network. Directories, empty files, and CRDT documents are always `Local`.
    Local,
    /// Metadata is projected but some content is not yet local; reading it will hydrate the missing bytes from a peer.
    Projected,
    /// A hydration fetch for this file's content is in flight right now.
    Downloading,
}

impl SyncState {
    /// The stable wire string carried in the `user.qos.sync` xattr.
    pub fn as_str(self) -> &'static str {
        match self {
            SyncState::Local => "local",
            SyncState::Projected => "projected",
            SyncState::Downloading => "downloading",
        }
    }

    /// Parse the xattr string back into a state (`None` for anything else).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "local" => Some(SyncState::Local),
            "projected" => Some(SyncState::Projected),
            "downloading" => Some(SyncState::Downloading),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_strings_round_trip() {
        for s in [
            SyncState::Local,
            SyncState::Projected,
            SyncState::Downloading,
        ] {
            assert_eq!(SyncState::parse(s.as_str()), Some(s), "round-trip {s:?}");
        }
        assert_eq!(SyncState::parse("bogus"), None);
        // The xattr name is the documented, stable contract with the app.
        assert_eq!(SYNC_XATTR, "user.qos.sync");
    }
}
