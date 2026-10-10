//! One governed identity import algorithm for both storage modes.
use super::{
    AutoCommit, BTreeMap, DeviceCert, GovernedIdentity, RedemptionReceipt, RevocationList,
    SystemTime, VerifiedCertificate, now_ns, revocations, roster,
};
use crate::resources::{
    PeerResourceError,
    json::{self, JsonLimits},
};
use crate::room::read::{
    self, GovernanceEntry, LegacyRead, ReadError, RoomRead, RoomValue, Selection,
};
use tilo_user_identity::{DeviceCertError, RevocationError};

#[derive(Clone, Copy)]
pub(super) struct ImportLimits {
    pub(super) collection_bytes: usize,
    pub(super) certificate_bytes: usize,
    pub(super) json: JsonLimits,
}
#[derive(Debug)]
pub(in crate::state) enum ImportError {
    Capacity,
    Decode(PeerResourceError),
    LegacyJson(serde_json::Error),
    Certificate(DeviceCertError),
    Revocation(RevocationError),
    LedgerShape,
    ReceiptShape,
    ReceiptKey,
    ReceiptConflict,
    IdentityKey,
    CertificateConflict,
    MissingAdmission,
    RevocationRoot,
    RevocationCrossRoot,
    Read,
}
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacyJson(error) => error.fmt(f),
            Self::Certificate(error) => error.fmt(f),
            Self::Revocation(error) => error.fmt(f),
            Self::LedgerShape => f.write_str("governed invitation receipt ledger is not a map"),
            Self::ReceiptShape => f.write_str("governed invitation receipt is not bytes"),
            Self::ReceiptKey => f.write_str("governed invitation receipt key mismatch"),
            Self::ReceiptConflict => {
                f.write_str("governed invitation has conflicting redemption receipts")
            }
            Self::IdentityKey => f.write_str("governed identity key mismatch"),
            Self::CertificateConflict => f.write_str("certificate conflict"),
            Self::MissingAdmission => {
                f.write_str("independent identity is missing governed admission receipt")
            }
            Self::RevocationRoot => f.write_str("revocation root mismatch"),
            Self::RevocationCrossRoot => f.write_str("revocation cannot cross identity roots"),
            Self::Capacity => f.write_str("governed import exceeds its limit"),
            Self::Decode(error) => write!(f, "governed import decode refusal: {error:?}"),
            Self::Read => f.write_str("governed import read failed"),
        }
    }
}
fn read_error(error: ReadError<ImportError>) -> ImportError {
    match error {
        ReadError::Capacity => ImportError::Capacity,
        ReadError::Visitor(error) => error,
        ReadError::Malformed => ImportError::Read,
    }
}
trait Decode {
    fn identity(&self, bytes: &[u8]) -> Result<GovernedIdentity, ImportError>;
    fn receipt(&self, bytes: &[u8]) -> Result<RedemptionReceipt, ImportError>;
}
struct Legacy;
impl Decode for Legacy {
    fn identity(&self, bytes: &[u8]) -> Result<GovernedIdentity, ImportError> {
        serde_json::from_slice(bytes).map_err(ImportError::LegacyJson)
    }
    fn receipt(&self, bytes: &[u8]) -> Result<RedemptionReceipt, ImportError> {
        serde_json::from_slice(bytes).map_err(ImportError::LegacyJson)
    }
}
struct Bounded {
    limits: JsonLimits,
}
impl Decode for Bounded {
    fn identity(&self, bytes: &[u8]) -> Result<GovernedIdentity, ImportError> {
        json::decode(bytes, self.limits).map_err(ImportError::Decode)
    }
    fn receipt(&self, bytes: &[u8]) -> Result<RedemptionReceipt, ImportError> {
        json::decode(bytes, self.limits).map_err(ImportError::Decode)
    }
}

struct Inputs {
    identities: BTreeMap<String, GovernedIdentity>,
}
fn inputs<R: RoomRead, D: Decode>(
    room: &R,
    decoder: &D,
    work: &mut Work,
) -> Result<Inputs, ImportError> {
    let mut identities = BTreeMap::new();
    read::visit_governance_entries(room, |entry| {
        if let GovernanceEntry::Identity { key, bytes } = entry {
            // Exact historical winner/ignore traversal. Decode every identity
            // before checking any receipt, key, certificate or revocation.
            let value = decoder.identity(bytes)?;
            let key = work.text(key)?;
            work.tree::<String, GovernedIdentity>()?;
            identities.insert(key, value);
        }
        Ok(())
    })
    .map_err(read_error)?;
    // The old invitation_receipts helper checks ALL root container shapes
    // before examining rows, then all row shapes before decoding any JSON.
    room.visit_property(
        &room.root(),
        "invitation_receipts_v1",
        Selection::All,
        |value| {
            if !matches!(
                value,
                RoomValue::Object {
                    kind: automerge::ObjType::Map,
                    ..
                }
            ) {
                return Err(ImportError::LedgerShape);
            }
            Ok(())
        },
    )
    .map_err(read_error)?;
    let mut receipts = BTreeMap::<String, Vec<Vec<u8>>>::new();
    room.visit_property(
        &room.root(),
        "invitation_receipts_v1",
        Selection::All,
        |value| {
            let RoomValue::Object {
                kind: automerge::ObjType::Map,
                object,
            } = value
            else {
                return Err(ImportError::LedgerShape);
            };
            room.visit_keys(&object, |key| {
                // Keep all raw values in the same sorted-key/ledger order as the
                // original collector.
                let mut values = Vec::new();
                room.visit_property(&object, key, Selection::All, |value| {
                    let RoomValue::Bytes(bytes) = value else {
                        return Err(ImportError::ReceiptShape);
                    };
                    work.debit(Some(bytes.len()))?;
                    work.push(&mut values, bytes.to_vec())
                })
                .map_err(read_error)?;
                if !receipts.contains_key(key) {
                    let key = work.text(key)?;
                    work.tree::<String, Vec<Vec<u8>>>()?;
                    receipts.insert(key, Vec::new());
                }
                let destination = receipts.get_mut(key).ok_or(ImportError::Read)?;
                for value in values {
                    work.push(destination, value)?;
                }
                Ok(())
            })
            .map_err(read_error)?;
            Ok(())
        },
    )
    .map_err(read_error)?;
    let mut redemptions = BTreeMap::new();
    for (key, values) in &receipts {
        for bytes in values {
            let receipt = decoder.receipt(bytes)?;
            if !is_key(key, &receipt.invite_id) {
                return Err(ImportError::ReceiptKey);
            }
            insert_receipt(&mut redemptions, &receipt, work)?;
        }
    }
    for identity in identities.values() {
        for receipt in identity.admission.iter().chain(&identity.admissions) {
            insert_receipt(&mut redemptions, receipt, work)?;
        }
    }
    Ok(Inputs { identities })
}
fn insert_receipt(
    redemptions: &mut BTreeMap<[u8; 32], RedemptionReceipt>,
    receipt: &RedemptionReceipt,
    work: &mut Work,
) -> Result<(), ImportError> {
    // All receipt fields are fixed arrays and an enum; clone has no backing.
    work.tree::<[u8; 32], RedemptionReceipt>()?;
    if let Some(existing) = redemptions.insert(receipt.invite_id, receipt.clone())
        && existing != *receipt
    {
        return Err(ImportError::ReceiptConflict);
    }
    Ok(())
}
fn is_key(text: &str, key: &[u8; 32]) -> bool {
    let mut encoded = [0; 64];
    hex::encode_to_slice(key, &mut encoded).is_ok() && text.as_bytes() == encoded
}

fn apply(
    inputs: &Inputs,
    original: &BTreeMap<[u8; 32], VerifiedCertificate>,
    certificates: &mut BTreeMap<[u8; 32], VerifiedCertificate>,
    revocations: &mut RevocationList,
    authority: &DeviceCert,
    now: SystemTime,
    cert_work: &mut Work,
) -> Result<bool, ImportError> {
    let mut changed = false;
    for (id, identity) in &inputs.identities {
        let cert = &identity.device_cert;
        if !is_key(id, &cert.device_public_key) {
            return Err(ImportError::IdentityKey);
        }
        let verified = if let Some(known) = certificates.get(&cert.device_public_key) {
            if known.as_ref() != cert {
                return Err(ImportError::CertificateConflict);
            }
            known.clone()
        } else {
            cert_work.certificate(cert, Some(now_ns(now)))?
        };
        if !original.contains_key(&cert.device_public_key)
            && cert.user_public_key != authority.user_public_key
            && !identity.has_admission()
        {
            return Err(ImportError::MissingAdmission);
        }
        for entry in &identity.revocations {
            if entry.user_public_key != cert.user_public_key {
                return Err(ImportError::RevocationRoot);
            }
            if certificates
                .get(&entry.revoked_device_public_key)
                .is_some_and(|target| target.user_public_key != entry.user_public_key)
            {
                return Err(ImportError::RevocationCrossRoot);
            }
            changed |= revocations
                .insert(entry.clone())
                .map_err(ImportError::Revocation)?;
        }
        changed |= certificates.get(&cert.device_public_key).map(AsRef::as_ref) != Some(cert);
        cert_work.tree::<[u8; 32], VerifiedCertificate>()?;
        certificates.insert(cert.device_public_key, verified);
    }
    Ok(changed)
}

/// Imported certificates and revocations, and whether either changed.
type LegacyImport = (
    BTreeMap<[u8; 32], VerifiedCertificate>,
    RevocationList,
    bool,
);
pub(in crate::state) fn legacy(
    room: &AutoCommit,
    original: &BTreeMap<[u8; 32], VerifiedCertificate>,
    revocations: &RevocationList,
    authority: &DeviceCert,
    now: SystemTime,
) -> Result<LegacyImport, ImportError> {
    let mut work = Work::unlimited();
    let inputs = inputs(&LegacyRead(room), &Legacy, &mut work)?;
    let mut certificates = original.clone();
    let mut revocations = revocations.clone();
    let changed = apply(
        &inputs,
        original,
        &mut certificates,
        &mut revocations,
        authority,
        now,
        &mut work,
    )?;
    Ok((certificates, revocations, changed))
}
pub(super) struct Imported {
    pub(super) certificates: roster::BoundedRoster,
    pub(super) revocations: revocations::BoundedRevocations,
}
pub(super) fn bounded<R: RoomRead>(
    room: &R,
    original: &roster::BoundedRoster,
    revocations: &revocations::BoundedRevocations,
    authority: &DeviceCert,
    now: SystemTime,
    limits: ImportLimits,
) -> Result<Imported, ImportError> {
    let mut work = Work::limited(limits.collection_bytes);
    let inputs = inputs(
        room,
        &Bounded {
            limits: limits.json,
        },
        &mut work,
    )?;
    let mut cert_work = Work::limited(limits.certificate_bytes);
    let mut certificates = BTreeMap::new();
    for (key, certificate) in original.map() {
        let copied = cert_work.certificate(certificate, None)?;
        cert_work.tree::<[u8; 32], VerifiedCertificate>()?;
        certificates.insert(*key, copied);
    }
    let mut candidate_revocations = RevocationList::new();
    for entry in revocations.iter() {
        candidate_revocations
            .insert(entry.clone())
            .map_err(ImportError::Revocation)?;
    }
    apply(
        &inputs,
        original.map(),
        &mut certificates,
        &mut candidate_revocations,
        authority,
        now,
        &mut cert_work,
    )?;
    Ok(Imported {
        certificates: roster::BoundedRoster::imported(certificates),
        revocations: revocations::BoundedRevocations::imported(candidate_revocations),
    })
}

/// A byte limit on the collections one import builds.
struct Work {
    remaining: usize,
}
impl Work {
    fn unlimited() -> Self {
        Self {
            remaining: usize::MAX,
        }
    }
    fn limited(bytes: usize) -> Self {
        Self { remaining: bytes }
    }
    fn debit(&mut self, bytes: Option<usize>) -> Result<(), ImportError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.ok_or(ImportError::Capacity)?)
            .ok_or(ImportError::Capacity)?;
        Ok(())
    }
    fn tree<K, V>(&mut self) -> Result<(), ImportError> {
        // An estimate of a BTreeMap node's share per insertion.
        self.debit(
            size_of::<K>()
                .checked_add(size_of::<V>())
                .and_then(|n| n.checked_mul(33))
                .and_then(|n| n.checked_add(768)),
        )
    }
    fn text(&mut self, value: &str) -> Result<String, ImportError> {
        self.debit(Some(value.len()))?;
        Ok(value.to_owned())
    }
    fn push<T>(&mut self, values: &mut Vec<T>, value: T) -> Result<(), ImportError> {
        if values.len() == values.capacity() {
            let next = values.capacity().saturating_mul(2).max(4);
            self.debit(next.checked_mul(size_of::<T>()))?;
            values
                .try_reserve_exact(next.saturating_sub(values.len()))
                .map_err(|_| ImportError::Capacity)?;
        }
        values.push(value);
        Ok(())
    }
    fn certificate(
        &mut self,
        value: &DeviceCert,
        now: Option<u64>,
    ) -> Result<VerifiedCertificate, ImportError> {
        self.debit(Some(size_of::<DeviceCert>()))?;
        let value = DeviceCert {
            user_public_key: value.user_public_key,
            device_public_key: value.device_public_key,
            device_label: self.text(&value.device_label)?,
            capabilities: value.capabilities.clone(),
            issued_at_ns: value.issued_at_ns,
            expires_at_ns: value.expires_at_ns,
            signature: value.signature,
        };
        match now {
            Some(now) => VerifiedCertificate::new_typed(value, now),
            None => VerifiedCertificate::historical_typed(value),
        }
        .map_err(ImportError::Certificate)
    }
}
#[cfg(test)]
mod tests;
