//! The restore certificate sequence is shared; native custody is never inferred
//! from an existing unqualified Arc.
use super::*;
use automerge::resource_limits::{NativeResourceError, NativeWorkTurn};
use std::borrow::Borrow;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::state) enum RosterError {
    StoredConflict,
    EnrollmentConflict,
    Certificate(tilo_user_identity::DeviceCertError),
    Resource(NativeResourceError),
}
impl std::fmt::Display for RosterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StoredConflict => formatter.write_str("durable certificate conflict"),
            Self::EnrollmentConflict => {
                formatter.write_str("durable certificate conflicts with enrollment")
            }
            Self::Certificate(error) => error.fmt(formatter),
            Self::Resource(error) => write!(formatter, "certificate resource refusal: {error:?}"),
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
        |value, now| verify_owned(value, now),
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
    _retention: NativeReservation,
}
impl BoundedRoster {
    pub(super) fn map(&self) -> &BTreeMap<[u8; 32], VerifiedCertificate> {
        &self.certificates
    }
    // Only the closed governed-import engine transfers these freshly paid
    // fields; this private helper is not a public qualification constructor.
    pub(super) fn imported(
        certificates: BTreeMap<[u8; 32], VerifiedCertificate>,
        retention: NativeReservation,
    ) -> Self {
        Self {
            certificates,
            _retention: retention,
        }
    }
    pub(super) fn restore(
        stored: &[DeviceCert],
        enrollment: &[DeviceCert],
        authority: &DeviceCert,
        now_ns: u64,
        turn: &NativeWorkTurn,
    ) -> Result<Self, RosterError> {
        let capacity = || RosterError::Resource(NativeResourceError::WorkBytes);
        let mut bytes = 0usize;
        let arc = std::alloc::Layout::new::<[std::sync::atomic::AtomicUsize; 2]>()
            .extend(std::alloc::Layout::new::<DeviceCert>())
            .map_err(|_| capacity())?
            .0
            .pad_to_align()
            .size();
        // Pinned Rust1.97.1 BTreeMap: internal nodes contain 11 key/value
        // slots, 12 child pointers and a <=256-byte aligned header. Fewer
        // than three nodes per insertion includes split/root transients.
        // This concrete map has key alignment1, value alignment<=16.
        let node = (size_of::<[u8; 32]>() + size_of::<VerifiedCertificate>()) * 33 + 768;
        if align_of::<VerifiedCertificate>() > 16 || align_of::<DeviceCert>() > 16 {
            return Err(RosterError::Resource(NativeResourceError::Unaccounted));
        }
        for value in stored.iter().chain(enrollment).chain([authority]) {
            bytes = bytes
                .checked_add(node)
                .and_then(|n| n.checked_add(arc))
                .and_then(|n| n.checked_add(value.device_label.len()))
                .ok_or_else(capacity)?;
        }
        // Duplicate entries deliberately keep their conservative allowance:
        // replacement temporarily overlaps old/new certificate backing.
        let retention = turn.reserve(bytes).map_err(RosterError::Resource)?;
        let certificates = restore(
            stored,
            enrollment.iter().chain([authority]),
            now_ns,
            |value, now| {
                let mut label = String::new();
                label
                    .try_reserve_exact(value.device_label.len())
                    .map_err(|_| capacity())?;
                if label.capacity() > value.device_label.len() {
                    return Err(RosterError::Resource(NativeResourceError::Unaccounted));
                }
                label.push_str(&value.device_label);
                // Every field except this freshly reserved String is inline. Never
                // clone a previously verified Arc from an unqualified source.
                verify_owned(
                    DeviceCert {
                        user_public_key: value.user_public_key,
                        device_public_key: value.device_public_key,
                        device_label: label,
                        capabilities: value.capabilities.clone(),
                        issued_at_ns: value.issued_at_ns,
                        expires_at_ns: value.expires_at_ns,
                        signature: value.signature,
                    },
                    now,
                )
            },
        )?;
        Ok(Self {
            certificates,
            _retention: retention,
        })
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
