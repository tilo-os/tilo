//! Shared signed revocation restore.
use super::{BTreeMap, RevocationList, VerifiedCertificate};
use tilo_user_identity::{RevocationEntry, RevocationError};

#[derive(Debug, PartialEq, Eq)]
pub(in crate::state) enum RestoreRevocationsError {
    RootMismatch,
    Signature(RevocationError),
    AuthorityRevoked,
}
impl std::fmt::Display for RestoreRevocationsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootMismatch => formatter.write_str("durable revocation root mismatch"),
            Self::Signature(error) => error.fmt(formatter),
            Self::AuthorityRevoked => formatter.write_str("pinned authority device is revoked"),
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
}
impl BoundedRevocations {
    /// Only a signature-verified enrollment union. There is no admitted roster
    /// here, so root binding and pinned-authority denial remain mandatory in
    /// the actual restore below.
    pub(in crate::state) fn verified(
        entries: &[RevocationEntry],
    ) -> Result<Self, RestoreRevocationsError> {
        Ok(Self {
            entries: verified_union(entries.iter().cloned(), |_| None)?,
        })
    }
    pub(in crate::state) fn list(&self) -> &RevocationList {
        &self.entries
    }
    pub(super) fn imported(entries: RevocationList) -> Self {
        Self { entries }
    }
    pub(super) fn restore(
        stored: &[RevocationEntry],
        enrollment: &RevocationList,
        certificates: &super::roster::BoundedRoster,
        authority: [u8; 32],
    ) -> Result<Self, RestoreRevocationsError> {
        let entries = restore(
            stored.iter().chain(enrollment.iter()).cloned(),
            |key| {
                certificates
                    .certificate(key)
                    .map(|certificate| certificate.user_public_key)
            },
            authority,
        )?;
        Ok(Self { entries })
    }
    pub(super) fn is_revoked(&self, key: &[u8; 32]) -> bool {
        self.entries.is_revoked(key)
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = &RevocationEntry> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests;
