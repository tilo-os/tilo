//! The governed identity import algorithm.
use super::{
    BTreeMap, GovernedIdentity, RedemptionReceipt, RevocationList, SystemTime, VerifiedCertificate,
    now_ns,
};
use crate::workspace::read::{ReadError, Selection, WorkspaceRead, WorkspaceValue};
use tilo_user_identity::{DeviceCertError, RevocationError};

#[derive(Debug)]
pub(in crate::state) enum ImportError {
    Row(crate::state::RowError),
    Receipt(postcard::Error),
    Certificate(DeviceCertError),
    Revocation(RevocationError),
    LogShape,
    ReceiptShape,
    ReceiptKey,
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
            Self::Row(error) => error.fmt(f),
            Self::Receipt(error) => write!(f, "governed invitation receipt: {error}"),
            Self::Certificate(error) => error.fmt(f),
            Self::Revocation(error) => error.fmt(f),
            Self::LogShape => f.write_str("governed invitation receipt log is not a map"),
            Self::ReceiptShape => f.write_str("governed invitation receipt is not bytes"),
            Self::ReceiptKey => f.write_str("governed invitation receipt key mismatch"),
            Self::IdentityKey => f.write_str("governed identity key mismatch"),
            Self::CertificateConflict => f.write_str("certificate conflict"),
            Self::MissingAdmission => {
                f.write_str("independent identity is missing governed admission receipt")
            }
            Self::RevocationRoot => f.write_str("revocation root mismatch"),
            Self::RevocationCrossRoot => f.write_str("revocation cannot cross identity roots"),
            Self::Read => f.write_str("governed import read failed"),
        }
    }
}
fn read_error(error: ReadError<ImportError>) -> ImportError {
    match error {
        ReadError::Visitor(error) => error,
        ReadError::Malformed => ImportError::Read,
    }
}
/// Where a workspace keeps its invitation receipts, by invitation.
const RECEIPTS: &str = "invitation_receipts_v1";
/// Every invitation receipt the workspace holds reads back as a receipt of
/// the invitation it is kept under.
/// With `only`, the receipts kept under those invitations alone: the rest
/// were checked in a state of the workspace this one only added to.
fn receipts_checked<R: WorkspaceRead>(
    workspace: &R,
    only: Option<&std::collections::BTreeSet<String>>,
) -> Result<(), ImportError> {
    // The old invitation_receipts helper checks ALL root container shapes
    // before examining rows, then all row shapes before decoding any JSON.
    workspace
        .visit_property(
            &automerge::ROOT,
            "invitation_receipts_v1",
            Selection::All,
            |value| {
                if !matches!(
                    value,
                    WorkspaceValue::Object {
                        kind: automerge::ObjType::Map,
                        ..
                    }
                ) {
                    return Err(ImportError::LogShape);
                }
                Ok(())
            },
        )
        .map_err(read_error)?;
    let mut receipts = BTreeMap::<String, Vec<Vec<u8>>>::new();
    workspace
        .visit_property(
            &automerge::ROOT,
            "invitation_receipts_v1",
            Selection::All,
            |value| {
                let WorkspaceValue::Object {
                    kind: automerge::ObjType::Map,
                    object,
                } = value
                else {
                    return Err(ImportError::LogShape);
                };
                workspace
                    .visit_keys(&object, |key| {
                        if only.is_some_and(|only| !only.contains(key)) {
                            return Ok(());
                        }
                        crate::work::receipt_read();
                        // Keep all raw values in the same sorted-key/log order as the
                        // original collector.
                        let mut values = Vec::new();
                        workspace
                            .visit_property(&object, key, Selection::All, |value| {
                                let WorkspaceValue::Bytes(bytes) = value else {
                                    return Err(ImportError::ReceiptShape);
                                };
                                values.push(bytes.to_vec());
                                Ok(())
                            })
                            .map_err(read_error)?;
                        receipts.entry(key.to_owned()).or_default().extend(values);
                        Ok(())
                    })
                    .map_err(read_error)?;
                Ok(())
            },
        )
        .map_err(read_error)?;
    // Two key holders may admit through one invitation at once, each
    // before it holds the other's receipt: both receipts stay, as every
    // receiver holds them alike.
    for (key, values) in &receipts {
        for bytes in values {
            receipt_checked(key, bytes)?;
        }
    }
    Ok(())
}

/// The receipt `bytes`, kept under `key`, reads back as a receipt of that
/// invitation. Remembered: a receipt never changes, and the workspace's
/// receipts are read again with every change to its governance.
fn receipt_checked(key: &str, bytes: &[u8]) -> Result<(), ImportError> {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static CHECKED: OnceLock<Mutex<HashSet<Vec<u8>>>> = OnceLock::new();
    /// Room for the receipts of every workspace a host holds.
    const LIMIT: usize = 65_536;
    let mut seen_as = key.as_bytes().to_vec();
    seen_as.push(0);
    seen_as.extend_from_slice(bytes);
    let checked = CHECKED.get_or_init(Mutex::default);
    if checked
        .lock()
        .is_ok_and(|checked| checked.contains(&seen_as))
    {
        return Ok(());
    }
    let receipt: RedemptionReceipt = crate::state::StoredReceipt::decode(bytes)
        .map_err(ImportError::Receipt)?
        .receipt;
    if !is_key(key, &receipt.invite_id) {
        return Err(ImportError::ReceiptKey);
    }
    if let Ok(mut checked) = checked.lock() {
        if checked.len() >= LIMIT {
            checked.clear();
        }
        checked.insert(seen_as);
    }
    Ok(())
}

fn is_key(text: &str, key: &[u8; 32]) -> bool {
    let mut encoded = [0; 64];
    hex::encode_to_slice(key, &mut encoded).is_ok() && text.as_bytes() == encoded
}

fn apply<'a>(
    identities: impl IntoIterator<Item = (&'a str, &'a GovernedIdentity)>,
    member: &dyn Fn(&str) -> bool,
    original: &BTreeMap<[u8; 32], VerifiedCertificate>,
    certificates: &mut BTreeMap<[u8; 32], VerifiedCertificate>,
    revocations: &mut RevocationList,
    owner: &[u8; 32],
    now: SystemTime,
) -> Result<bool, ImportError> {
    let mut changed = false;
    for (id, identity) in identities {
        let cert = &identity.device_cert;
        if !is_key(id, &cert.device_public_key) {
            return Err(ImportError::IdentityKey);
        }
        let verified = match certificates.get(&cert.device_public_key) {
            Some(known) if known.as_ref() == cert => known.clone(),
            // The person's root renewed the device's certificate: the same
            // member, under the certificate that replaces the one it held.
            Some(known) if crate::state::certificate_renewal::renews(cert, known.as_ref()) => {
                verified(cert, now)?
            }
            // An older state of the workspace, from before the renewal it holds:
            // the renewal stays.
            Some(known) if crate::state::certificate_renewal::renews(known.as_ref(), cert) => {
                known.clone()
            }
            Some(_) => return Err(ImportError::CertificateConflict),
            None => verified(cert, now)?,
        };
        if !original.contains_key(&cert.device_public_key)
            && cert.user_public_key != *owner
            && !identity.has_admission()
            && !member(&hex::encode(cert.user_public_key))
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
        changed |=
            certificates.get(&cert.device_public_key).map(AsRef::as_ref) != Some(verified.as_ref());
        certificates.insert(cert.device_public_key, verified);
    }
    Ok(changed)
}

/// Imported certificates and revocations, and whether either changed.
type Import = (
    BTreeMap<[u8; 32], VerifiedCertificate>,
    RevocationList,
    bool,
);
/// The certificates and revocations a workspace's governed identity rows
/// prove, over those the replica already holds. The rows are read once per
/// document state ([`crate::cached_document`]): an identity row a change left
/// as it was is neither decoded again nor its certificate checked again, so
/// taking in a change costs what the change touched, not the workspace.
pub(in crate::state) fn import(
    workspace: &crate::cached_document::CachedDocument,
    original: &BTreeMap<[u8; 32], VerifiedCertificate>,
    revocations: &RevocationList,
    owner: &[u8; 32],
    now: SystemTime,
) -> Result<Import, ImportError> {
    let views = workspace.identities();
    let mut identities = Vec::with_capacity(views.len());
    for (key, identity) in views {
        let Ok(identity) = identity else {
            // The error as the row's own bytes give it.
            let bytes = workspace
                .policy()
                .identities
                .get(key)
                .ok_or(ImportError::Read)?;
            return Err(GovernedIdentity::decode(bytes)
                .err()
                .map_or(ImportError::Read, ImportError::Row));
        };
        identities.push((key.as_str(), identity));
    }
    // The receipts a change wrote are checked; those of the state it was
    // made on were already: reading every receipt, each carrying its whole
    // invitation, cost each admission to a workspace of hundreds more than
    // the rest of taking it in.
    let written = workspace
        .written_since_receipts_checked()
        .and_then(|written| crate::workspace::rows::touched(workspace, written));
    match written.as_ref().map(|written| written.get(RECEIPTS)) {
        Some(None) => {}
        Some(Some(crate::workspace::rows::Wrote::Entries(keys))) => {
            receipts_checked(&**workspace, Some(keys))?;
        }
        Some(Some(crate::workspace::rows::Wrote::Whole)) | None => {
            receipts_checked(&**workspace, None)?;
        }
    }
    workspace.receipts_checked();
    let members = crate::members::replay_declared(workspace).members;
    let mut certificates = original.clone();
    let mut revocations = revocations.clone();
    let changed = apply(
        identities,
        &|person| members.contains_key(person),
        original,
        &mut certificates,
        &mut revocations,
        owner,
        now,
    )?;
    Ok((certificates, revocations, changed))
}
fn verified(
    cert: &tilo_user_identity::DeviceCert,
    now: SystemTime,
) -> Result<VerifiedCertificate, ImportError> {
    VerifiedCertificate::new_typed(cert.clone(), now_ns(now)).map_err(ImportError::Certificate)
}
#[cfg(test)]
mod tests;
