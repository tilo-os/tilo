//! Shared signed revocation restore, with separately retained bounded storage.
use super::*;
use automerge::resource_limits::{NativeResourceError, NativeWorkTurn};
use tilo_user_identity::{RevocationEntry, RevocationError};

#[derive(Debug, PartialEq, Eq)]
pub(in crate::state) enum RestoreRevocationsError {
    RootMismatch,
    Signature(RevocationError),
    AuthorityRevoked,
    Resource(NativeResourceError),
}
impl std::fmt::Display for RestoreRevocationsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootMismatch => formatter.write_str("durable revocation root mismatch"),
            Self::Signature(error) => error.fmt(formatter),
            Self::AuthorityRevoked => formatter.write_str("pinned authority device is revoked"),
            Self::Resource(error) => write!(formatter, "revocation resource refusal: {error:?}"),
        }
    }
}

fn verified_union(
    entries: impl IntoIterator<Item = RevocationEntry>,
    root_for: impl Fn(&[u8; 32]) -> Option<[u8; 32]>,
) -> Result<RevocationList, RestoreRevocationsError> {
    let mut revocations = RevocationList::new();
    for entry in entries {
        if root_for(&entry.revoked_device_public_key)
            .is_some_and(|root| root != entry.user_public_key)
        {
            return Err(RestoreRevocationsError::RootMismatch);
        }
        // Includes independent verified roots and keeps the earliest original
        // signed entry, exactly as the existing RevocationList union does.
        revocations
            .insert(entry)
            .map_err(RestoreRevocationsError::Signature)?;
    }
    Ok(revocations)
}

fn restore(
    entries: impl IntoIterator<Item = RevocationEntry>,
    root_for: impl Fn(&[u8; 32]) -> Option<[u8; 32]>,
    authority: [u8; 32],
) -> Result<RevocationList, RestoreRevocationsError> {
    let revocations = verified_union(entries, root_for)?;
    if revocations.is_revoked(&authority) {
        return Err(RestoreRevocationsError::AuthorityRevoked);
    }
    Ok(revocations)
}

pub(in crate::state) fn restore_legacy(
    stored: Vec<RevocationEntry>,
    enrollment: &RevocationList,
    certificates: &BTreeMap<[u8; 32], VerifiedCertificate>,
    authority: [u8; 32],
) -> Result<RevocationList, RestoreRevocationsError> {
    restore(
        stored.into_iter().chain(enrollment.iter().cloned()),
        |key| {
            certificates
                .get(key)
                .map(|certificate| certificate.user_public_key)
        },
        authority,
    )
}
pub(in crate::state) struct BoundedRevocations {
    entries: RevocationList,
    _retention: NativeReservation,
}
impl BoundedRevocations {
    /// Only a signature-verified enrollment union. There is no admitted roster
    /// in this resource bridge, so root binding and pinned-authority denial
    /// remain mandatory in the actual restore below.
    pub(in crate::state) fn verified(
        entries: &[RevocationEntry],
        turn: &NativeWorkTurn,
    ) -> Result<Self, RestoreRevocationsError> {
        let retention = turn
            .reserve(insertion_bytes(entries.len())?)
            .map_err(RestoreRevocationsError::Resource)?;
        let entries = verified_union(entries.iter().cloned(), |_| None)?;
        Ok(Self {
            entries,
            _retention: retention,
        })
    }
    pub(in crate::state) fn list(&self) -> &RevocationList {
        &self.entries
    }
    // Private custody transfer from the closed governed-import engine only.
    pub(super) fn imported(entries: RevocationList, retention: NativeReservation) -> Self {
        Self {
            entries,
            _retention: retention,
        }
    }
    pub(super) fn restore(
        stored: &[RevocationEntry],
        enrollment: &RevocationList,
        certificates: &super::roster::BoundedRoster,
        authority: [u8; 32],
        turn: &NativeWorkTurn,
    ) -> Result<Self, RestoreRevocationsError> {
        let count =
            stored
                .len()
                .checked_add(enrollment.len())
                .ok_or(RestoreRevocationsError::Resource(
                    NativeResourceError::WorkBytes,
                ))?;
        let retention = turn
            .reserve(insertion_bytes(count)?)
            .map_err(RestoreRevocationsError::Resource)?;
        // RevocationEntry has only inline fixed arrays, numbers and an enum.
        // Its clone creates no owned byte backing or shared uncharged handle.
        let entries = restore(
            stored.iter().chain(enrollment.iter()).cloned(),
            |key| {
                certificates
                    .certificate(key)
                    .map(|certificate| certificate.user_public_key)
            },
            authority,
        )?;
        Ok(Self {
            entries,
            _retention: retention,
        })
    }
    pub(super) fn is_revoked(&self, key: &[u8; 32]) -> bool {
        self.entries.is_revoked(key)
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = &RevocationEntry> {
        self.entries.iter()
    }
}

pub(in crate::state) fn insertion_bytes(count: usize) -> Result<usize, RestoreRevocationsError> {
    let capacity = || RestoreRevocationsError::Resource(NativeResourceError::WorkBytes);
    if count == 0 {
        return Ok(0);
    }
    // Pinned std1.97.1 uses hashbrown0.17.1. This concrete non-ZST entry is
    // larger than three bytes, so capacity_to_buckets starts at4,8,16 and
    // then reserves1/8 buckets. No deletion occurs in this fresh union.
    let buckets = if count < 4 {
        4
    } else if count < 8 {
        8
    } else if count < 15 {
        16
    } else {
        (count.checked_mul(8).ok_or_else(capacity)? / 7)
            .checked_next_power_of_two()
            .ok_or_else(capacity)?
    };
    type Entry = ([u8; 32], RevocationEntry);
    // TableLayout rounds data to max(entry alignment, control width), then
    // stores one control byte per bucket plus one complete trailing group.
    //16 bounds pinned aarch64/x86/wasm groups; actual aarch64 width is8.
    let alignment = align_of::<Entry>().max(16);
    let data = size_of::<Entry>()
        .checked_mul(buckets)
        .ok_or_else(capacity)?;
    let padded = data.checked_add(alignment - 1).ok_or_else(capacity)? & !(alignment - 1);
    let table = padded
        .checked_add(buckets)
        .and_then(|n| n.checked_add(16))
        .ok_or_else(capacity)?;
    // A growing table and its prior table overlap during rehash. All earlier
    // tables have already been freed; two maximum tables bound that peak.
    table.checked_mul(2).ok_or_else(capacity)
}

#[cfg(test)]
mod tests;
