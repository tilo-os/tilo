//! The id of one entry of a history, and its hex spelling.

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
    /// ([`entry_hex`]).
    #[must_use]
    pub fn hex(&self) -> String {
        entry_hex(&self.0)
    }

    /// The id [`Self::hex`] spells, in either case. `None` on anything that
    /// is not exactly 32 hex characters, so a corrupted document key is
    /// skipped rather than resolving to a wrong change.
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        entry_id(hex).map(Self)
    }
}

/// The history id `hex` spells, in either case: 32 hex characters, else none.
#[must_use]
pub fn entry_id(hex: &str) -> Option<[u8; 16]> {
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(hex, &mut bytes).ok()?;
    Some(bytes)
}

/// The history id `bytes` spells: lower-case hex.
#[must_use]
pub fn entry_hex(bytes: &[u8; 16]) -> String {
    hex::encode(bytes)
}

/// The history id of a change, as a history spells an entry's id: the first 16
/// bytes of the change's 32, lower-case hex.
#[must_use]
pub fn change_hex(change: &[u8; 32]) -> String {
    hex::encode(&change[..16])
}
