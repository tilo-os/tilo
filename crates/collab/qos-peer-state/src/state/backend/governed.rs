//! One governed identity import algorithm, with independently retained native outputs.
use super::*;
use crate::resources::{
    PeerResourceError,
    json::{JsonHeld, JsonLimits},
};
use crate::room::read::{
    self, CopyValue, GovernanceEntry, LegacyCopy, LegacyRead, ReadError, RoomRead, RoomValue,
    Selection, TurnCopy,
};
use automerge::resource_limits::{NativeResourceError, NativeWorkTurn};
use tilo_user_identity::{DeviceCertError, RevocationError};

#[derive(Clone, Copy)]
pub(super) struct ImportLimits {
    pub(super) collection_bytes: usize,
    pub(super) certificate_bytes: usize,
    pub(super) json: JsonLimits,
}
#[derive(Debug)]
pub(in crate::state) enum ImportError {
    Resource(NativeResourceError),
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
            Self::Resource(error) => write!(f, "governed import resource refusal: {error:?}"),
            Self::Decode(error) => write!(f, "governed import decode refusal: {error:?}"),
            Self::Read => f.write_str("governed import read failed"),
        }
    }
}
fn read_error(error: ReadError<ImportError>) -> ImportError {
    match error {
        ReadError::Resource(error) => ImportError::Resource(error),
        ReadError::Visitor(error) => error,
        ReadError::Malformed => ImportError::Read,
    }
}
trait View<T> {
    fn value(&self) -> &T;
}
impl View<GovernedIdentity> for GovernedIdentity {
    fn value(&self) -> &Self {
        self
    }
}
impl View<RedemptionReceipt> for RedemptionReceipt {
    fn value(&self) -> &Self {
        self
    }
}
impl<T: crate::resources::json::Schema> View<T> for JsonHeld<T> {
    fn value(&self) -> &T {
        self.value()
    }
}
trait Decode {
    type Identity: View<GovernedIdentity>;
    type Receipt: View<RedemptionReceipt>;
    fn identity(&self, bytes: &[u8]) -> Result<Self::Identity, ImportError>;
    fn receipt(&self, bytes: &[u8]) -> Result<Self::Receipt, ImportError>;
}
struct Legacy;
impl Decode for Legacy {
    type Identity = GovernedIdentity;
    type Receipt = RedemptionReceipt;
    fn identity(&self, bytes: &[u8]) -> Result<Self::Identity, ImportError> {
        serde_json::from_slice(bytes).map_err(ImportError::LegacyJson)
    }
    fn receipt(&self, bytes: &[u8]) -> Result<Self::Receipt, ImportError> {
        serde_json::from_slice(bytes).map_err(ImportError::LegacyJson)
    }
}
struct Bounded<'a> {
    turn: &'a NativeWorkTurn,
    limits: JsonLimits,
}
impl Decode for Bounded<'_> {
    type Identity = JsonHeld<GovernedIdentity>;
    type Receipt = JsonHeld<RedemptionReceipt>;
    fn identity(&self, bytes: &[u8]) -> Result<Self::Identity, ImportError> {
        JsonHeld::decode(bytes, self.limits, self.turn).map_err(ImportError::Decode)
    }
    fn receipt(&self, bytes: &[u8]) -> Result<Self::Receipt, ImportError> {
        JsonHeld::decode(bytes, self.limits, self.turn).map_err(ImportError::Decode)
    }
}

/// Inputs have their own guards. `Work` precedes these collections and remains
/// alive until their maps, vector backing and copied keys have been destroyed.
struct Inputs<I> {
    identities: BTreeMap<String, I>,
}
fn inputs<R: RoomRead, C: CopyValue, D: Decode>(
    room: &R,
    copies: &C,
    decoder: &D,
    work: &mut Work,
) -> Result<Inputs<D::Identity>, ImportError> {
    let mut identities = BTreeMap::new();
    read::visit_governance_entries(room, |entry| {
        if let GovernanceEntry::Identity { key, bytes } = entry {
            // Exact historical winner/ignore traversal. Decode every identity
            // before checking any receipt, key, certificate or revocation.
            let value = decoder.identity(bytes)?;
            let key = work.text(key)?;
            work.tree::<String, D::Identity>()?;
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
    let mut receipts = BTreeMap::<String, Vec<C::Bytes>>::new();
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
                // original collector. Copied native bytes retain separate credit.
                let mut values = Vec::new();
                room.visit_property(&object, key, Selection::All, |value| {
                    let RoomValue::Bytes(bytes) = value else {
                        return Err(ImportError::ReceiptShape);
                    };
                    let bytes = copies.bytes(bytes).map_err(ImportError::Resource)?;
                    work.push(&mut values, bytes)
                })
                .map_err(read_error)?;
                if !receipts.contains_key(key) {
                    let key = work.text(key)?;
                    work.tree::<String, Vec<C::Bytes>>()?;
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
            let receipt = decoder.receipt(bytes.as_ref())?;
            let receipt = receipt.value();
            if !is_key(key, &receipt.invite_id) {
                return Err(ImportError::ReceiptKey);
            }
            insert_receipt(&mut redemptions, receipt, work)?;
        }
    }
    for identity in identities.values() {
        let identity = identity.value();
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

fn apply<I: View<GovernedIdentity>>(
    inputs: &Inputs<I>,
    original: &BTreeMap<[u8; 32], VerifiedCertificate>,
    certificates: &mut BTreeMap<[u8; 32], VerifiedCertificate>,
    revocations: &mut RevocationList,
    authority: &DeviceCert,
    now: SystemTime,
    cert_work: &mut Work,
) -> Result<bool, ImportError> {
    let mut changed = false;
    for (id, held) in &inputs.identities {
        let identity = held.value();
        let cert = &identity.device_cert;
        if !is_key(id, &cert.device_public_key) {
            return Err(ImportError::IdentityKey);
        }
        let verified = if let Some(known) = certificates.get(&cert.device_public_key) {
            if known.as_ref() != cert {
                return Err(ImportError::CertificateConflict);
            }
            // This shares only a backing already owned by THIS candidate's
            // retained roster, never an unqualified source certificate Arc.
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

pub(in crate::state) fn legacy(
    room: &AutoCommit,
    original: &BTreeMap<[u8; 32], VerifiedCertificate>,
    revocations: &RevocationList,
    authority: &DeviceCert,
    now: SystemTime,
) -> Result<
    (
        BTreeMap<[u8; 32], VerifiedCertificate>,
        RevocationList,
        bool,
    ),
    ImportError,
> {
    let mut work = Work::legacy();
    let inputs = inputs(&LegacyRead(room), &LegacyCopy, &Legacy, &mut work)?;
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
    pub(super) changed: bool,
}
pub(super) fn bounded<R: RoomRead>(
    room: &R,
    original: &roster::BoundedRoster,
    revocations: &revocations::BoundedRevocations,
    authority: &DeviceCert,
    now: SystemTime,
    turn: &NativeWorkTurn,
    limits: ImportLimits,
) -> Result<Imported, ImportError> {
    let mut work = Work::bounded(turn, limits.collection_bytes)?;
    let inputs = inputs(
        room,
        &TurnCopy(turn),
        &Bounded {
            turn,
            limits: limits.json,
        },
        &mut work,
    )?;
    let mut cert_work = Work::bounded(turn, limits.certificate_bytes)?;
    let mut certificates = BTreeMap::new();
    for (key, certificate) in original.map() {
        let copied = cert_work.certificate(certificate, None)?;
        cert_work.tree::<[u8; 32], VerifiedCertificate>()?;
        certificates.insert(*key, copied);
    }
    let mut count = revocations.list().len();
    for held in inputs.identities.values() {
        count = count
            .checked_add(held.value().revocations.len())
            .ok_or(ImportError::Resource(NativeResourceError::WorkBytes))?;
    }
    let bytes = revocations::insertion_bytes(count).map_err(|error| match error {
        revocations::RestoreRevocationsError::Resource(error) => ImportError::Resource(error),
        _ => ImportError::Resource(NativeResourceError::Unaccounted),
    })?;
    let revocation_retention = turn.reserve(bytes).map_err(ImportError::Resource)?;
    let mut candidate_revocations = RevocationList::new();
    for entry in revocations.iter() {
        candidate_revocations
            .insert(entry.clone())
            .map_err(ImportError::Revocation)?;
    }
    let changed = apply(
        &inputs,
        original.map(),
        &mut certificates,
        &mut candidate_revocations,
        authority,
        now,
        &mut cert_work,
    )?;
    // These are closed transfers of fields allocated by the engine above, not
    // arbitrary map/DTO qualification APIs. The private holders drop fields first.
    Ok(Imported {
        certificates: roster::BoundedRoster::imported(
            certificates,
            cert_work.retention.take().ok_or(ImportError::Read)?,
        ),
        revocations: revocations::BoundedRevocations::imported(
            candidate_revocations,
            revocation_retention,
        ),
        changed,
    })
}

struct Work {
    remaining: usize,
    retention: Option<NativeReservation>,
}
impl Work {
    fn legacy() -> Self {
        Self {
            remaining: usize::MAX,
            retention: None,
        }
    }
    fn bounded(turn: &NativeWorkTurn, bytes: usize) -> Result<Self, ImportError> {
        Ok(Self {
            remaining: bytes,
            retention: Some(turn.reserve(bytes).map_err(ImportError::Resource)?),
        })
    }
    fn debit(&mut self, bytes: Option<usize>) -> Result<(), ImportError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.ok_or(ImportError::Resource(NativeResourceError::WorkBytes))?)
            .ok_or(ImportError::Resource(NativeResourceError::WorkBytes))?;
        Ok(())
    }
    fn tree<K, V>(&mut self) -> Result<(), ImportError> {
        // Pinned std1.97.1: 11 slots and12 child pointers per internal node;
        // fewer than3 nodes per insertion bounds splits/root overlap. <=16
        // alignment and256 bytes per header bound these concrete map types.
        if align_of::<K>() > 16 || align_of::<V>() > 16 {
            return Err(ImportError::Resource(NativeResourceError::Unaccounted));
        }
        self.debit(
            size_of::<K>()
                .checked_add(size_of::<V>())
                .and_then(|n| n.checked_mul(33))
                .and_then(|n| n.checked_add(768)),
        )
    }
    fn text(&mut self, value: &str) -> Result<String, ImportError> {
        self.debit(Some(value.len()))?;
        let mut text = String::new();
        text.try_reserve_exact(value.len())
            .map_err(|_| ImportError::Resource(NativeResourceError::WorkBytes))?;
        if text.capacity() > value.len() {
            return Err(ImportError::Resource(NativeResourceError::Unaccounted));
        }
        text.push_str(value);
        Ok(text)
    }
    fn push<T>(&mut self, values: &mut Vec<T>, value: T) -> Result<(), ImportError> {
        if values.len() == values.capacity() {
            let next = values
                .capacity()
                .checked_mul(2)
                .unwrap_or(usize::MAX)
                .max(4);
            // Cumulative reservations retain old+new allocations during growth.
            self.debit(next.checked_mul(size_of::<T>()))?;
            values
                .try_reserve_exact(next - values.len())
                .map_err(|_| ImportError::Resource(NativeResourceError::WorkBytes))?;
            if values.capacity() > next {
                return Err(ImportError::Resource(NativeResourceError::Unaccounted));
            }
        }
        values.push(value);
        Ok(())
    }
    fn certificate(
        &mut self,
        value: &DeviceCert,
        now: Option<u64>,
    ) -> Result<VerifiedCertificate, ImportError> {
        let arc = std::alloc::Layout::new::<[std::sync::atomic::AtomicUsize; 2]>()
            .extend(std::alloc::Layout::new::<DeviceCert>())
            .map_err(|_| ImportError::Resource(NativeResourceError::WorkBytes))?
            .0
            .pad_to_align()
            .size();
        self.debit(Some(arc))?;
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
