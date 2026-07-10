//! Per-entry **sync / hydration state**, exposed to apps (the file manager)
//! through the `user.qos.sync` extended attribute.
//!
//! The collaborative FS *projects* remote metadata immediately — a file shows up
//! in `readdir` with its real name and size the instant a peer gossips it — but
//! only *hydrates* the content on first read. An app that wants to tell the user
//! whether a file is already on disk, still remote, or mid-download needs to see
//! that distinction.
//!
//! Rather than invent a bespoke query socket, the driver answers a single
//! extended attribute: `getxattr(path, "user.qos.sync")` returns one of
//! `local` / `projected` / `downloading`. This rides the existing FUSE mount,
//! needs no qOS-specific client (`getfattr -n user.qos.sync <path>` works), and
//! is naturally sandbox-safe: any process that can `stat` the file can read its
//! sync state, nothing more. See [`crate::fs::QuickFS::getxattr`].

/// The extended-attribute name the FUSE driver answers with an entry's sync
/// state. Read it with `getxattr(path, "user.qos.sync")`.
pub const SYNC_XATTR: &str = "user.qos.sync";

/// The hydration state of one filesystem entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Content is fully present in the local cache — a plain read never touches
    /// the network. Directories, empty files, and CRDT-backed documents (which
    /// are materialised from the namespace itself) are always `Local`.
    Local,
    /// Metadata is projected but the content — or some chunk of it — is not yet
    /// local. Reading it will hydrate the missing bytes from a peer.
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
        for s in [SyncState::Local, SyncState::Projected, SyncState::Downloading] {
            assert_eq!(SyncState::parse(s.as_str()), Some(s), "round-trip {s:?}");
        }
        assert_eq!(SyncState::parse("bogus"), None);
        // The xattr name is the documented, stable contract with the app.
        assert_eq!(SYNC_XATTR, "user.qos.sync");
    }
}
