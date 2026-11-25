//! The id of one entry of a history.

/// One entry of a history as people read it. Opaque: only ever compared
/// for equality (revert/restore callbacks pass it back to the caller). The
/// caller picks the byte layout — UUID-v4 today, a content hash later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HistoryEntryId(pub [u8; 16]);

impl HistoryEntryId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// The id as the wire and a document key spell it: lower-case hex
    /// ([`crate::remote::workspace_activity::entry_hex`]).
    #[must_use]
    pub fn hex(&self) -> String {
        crate::remote::workspace_activity::entry_hex(&self.0)
    }

    /// The id [`Self::hex`] spells, in either case. `None` on anything that
    /// is not exactly 32 hex characters, so a corrupted document key is
    /// skipped rather than resolving to a wrong change.
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        crate::remote::workspace_activity::entry_id(hex).map(Self)
    }
}
