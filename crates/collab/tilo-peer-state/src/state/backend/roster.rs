//! The restore certificate sequence.
use super::{BTreeMap, DeviceCert, VerifiedCertificate};

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

/// The certificates a durable state holds, with those its enrollment and
/// `authority` add. Matching durable evidence is historical even when
/// enrollment repeats it; only genuinely new enrollment is checked against
/// the current time.
pub(in crate::state) fn restore(
    stored: Vec<DeviceCert>,
    enrollment: Vec<DeviceCert>,
    authority: &DeviceCert,
    now_ns: u64,
) -> Result<BTreeMap<[u8; 32], VerifiedCertificate>, RosterError> {
    let mut certificates = BTreeMap::<[u8; 32], VerifiedCertificate>::new();
    for certificate in stored {
        let key = certificate.device_public_key;
        if certificates
            .get(&key)
            .is_some_and(|known| *known.as_ref() != certificate)
        {
            return Err(RosterError::StoredConflict);
        }
        certificates.insert(key, verify(certificate, None)?);
    }
    for certificate in enrollment.into_iter().chain([authority.clone()]) {
        let key = certificate.device_public_key;
        if let Some(known) = certificates.get(&key) {
            if *known.as_ref() != certificate {
                return Err(RosterError::EnrollmentConflict);
            }
            continue;
        }
        certificates.insert(key, verify(certificate, Some(now_ns))?);
    }
    Ok(certificates)
}

fn verify(value: DeviceCert, now: Option<u64>) -> Result<VerifiedCertificate, RosterError> {
    match now {
        Some(now) => VerifiedCertificate::new_typed(value, now),
        None => VerifiedCertificate::historical_typed(value),
    }
    .map_err(RosterError::Certificate)
}

#[cfg(test)]
mod tests;
