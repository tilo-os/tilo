//! Shared signed revocation restore.
use super::{BTreeMap, RevocationList, VerifiedCertificate};
use tilo_user_identity::{RevocationEntry, RevocationError};

#[derive(Debug, PartialEq, Eq)]
pub(in crate::state) enum RestoreRevocationsError {
    RootMismatch,
    Signature(RevocationError),
}
impl std::fmt::Display for RestoreRevocationsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RootMismatch => formatter.write_str("durable revocation root mismatch"),
            Self::Signature(error) => error.fmt(formatter),
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

pub(in crate::state) fn restore_legacy(
    stored: Vec<RevocationEntry>,
    enrollment: &RevocationList,
    certificates: &BTreeMap<[u8; 32], VerifiedCertificate>,
) -> Result<RevocationList, RestoreRevocationsError> {
    verified_union(
        stored.into_iter().chain(enrollment.iter().cloned()),
        |key| {
            certificates
                .get(key)
                .map(|certificate| certificate.user_public_key)
        },
    )
}
#[cfg(test)]
mod tests;
