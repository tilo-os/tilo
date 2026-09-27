//! The restore certificate sequence shared by both storage modes.
use super::{BTreeMap, DeviceCert, VerifiedCertificate};
use std::borrow::Borrow;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::state) enum RosterError {
    StoredConflict,
    EnrollmentConflict,
    Certificate(tilo_user_identity::DeviceCertError),
}
impl std::fmt::Display for RosterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StoredConflict => formatter.write_str("durable certificate conflict"),
            Self::EnrollmentConflict => {
                formatter.write_str("durable certificate conflicts with enrollment")
            }
            Self::Certificate(error) => error.fmt(formatter),
        }
    }
}

// Both storage modes execute exactly this restore sequence. In particular,
// matching durable evidence is historical even when enrollment repeats it;
// only genuinely new enrollment is checked against the current time.
fn restore<C: Borrow<DeviceCert>>(
    stored: impl IntoIterator<Item = C>,
    enrollment: impl IntoIterator<Item = C>,
    now_ns: u64,
    mut make: impl FnMut(C, Option<u64>) -> Result<VerifiedCertificate, RosterError>,
) -> Result<BTreeMap<[u8; 32], VerifiedCertificate>, RosterError> {
    let mut certificates = BTreeMap::<[u8; 32], VerifiedCertificate>::new();
    for certificate in stored {
        let value = certificate.borrow();
        let key = value.device_public_key;
        if certificates
            .get(&key)
            .is_some_and(|known| known.as_ref() != value)
        {
            return Err(RosterError::StoredConflict);
        }
        certificates.insert(key, make(certificate, None)?);
    }
    for certificate in enrollment {
        let value = certificate.borrow();
        let key = value.device_public_key;
        if let Some(known) = certificates.get(&key) {
            if known.as_ref() != value {
                return Err(RosterError::EnrollmentConflict);
            }
            continue;
        }
        certificates.insert(key, make(certificate, Some(now_ns))?);
    }
    Ok(certificates)
}

pub(in crate::state) fn restore_legacy(
    stored: Vec<DeviceCert>,
    enrollment: Vec<DeviceCert>,
    authority: &DeviceCert,
    now_ns: u64,
) -> Result<BTreeMap<[u8; 32], VerifiedCertificate>, RosterError> {
    restore(
        stored,
        enrollment.into_iter().chain([authority.clone()]),
        now_ns,
        verify_owned,
    )
}

fn verify_owned(value: DeviceCert, now: Option<u64>) -> Result<VerifiedCertificate, RosterError> {
    match now {
        Some(now) => VerifiedCertificate::new_typed(value, now),
        None => VerifiedCertificate::historical_typed(value),
    }
    .map_err(RosterError::Certificate)
}

pub(in crate::state) struct BoundedRoster {
    certificates: BTreeMap<[u8; 32], VerifiedCertificate>,
}
impl BoundedRoster {
    pub(super) fn map(&self) -> &BTreeMap<[u8; 32], VerifiedCertificate> {
        &self.certificates
    }
    pub(super) fn imported(certificates: BTreeMap<[u8; 32], VerifiedCertificate>) -> Self {
        Self { certificates }
    }
    pub(super) fn restore(
        stored: &[DeviceCert],
        enrollment: &[DeviceCert],
        authority: &DeviceCert,
        now_ns: u64,
    ) -> Result<Self, RosterError> {
        let certificates = restore(
            stored,
            enrollment.iter().chain([authority]),
            now_ns,
            |value, now| verify_owned(value.clone(), now),
        )?;
        Ok(Self { certificates })
    }
    pub(super) fn verified(&self, key: &[u8; 32]) -> Option<&VerifiedCertificate> {
        self.certificates.get(key)
    }
    pub(super) fn certificate(&self, key: &[u8; 32]) -> Option<&DeviceCert> {
        self.certificates.get(key).map(AsRef::as_ref)
    }
}

#[cfg(test)]
mod tests;
