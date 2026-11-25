//! The rows a workspace's governance keeps per device: its certificate and how
//! it was admitted.
use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

use super::DelegateRow;

/// A device row: its key, the bytes it was read from, and the row they hold.
pub(crate) type IdentityRow = (String, Vec<u8>, DeviceRow);

/// The workspace document's device rows, in key order.
pub(crate) fn identity_rows(doc: &AutoCommit) -> Result<Vec<IdentityRow>, String> {
    crate::workspace::governance_of(Some(doc))
        .identities
        .into_iter()
        .map(|(id, bytes)| {
            DeviceRow::decode(&bytes)
                .map(|row| (id, bytes, row))
                .map_err(err)
        })
        .collect()
}

/// Write the rows whose bytes changed, as gone from `rows` where the document
/// holds them. Whether anything changed.
pub(crate) fn put_identity_rows(
    doc: &mut AutoCommit,
    rows: &[IdentityRow],
) -> Result<bool, String> {
    let map = identities(doc)?;
    let mut changed = false;
    for (id, old, row) in rows {
        let bytes = row.encode().map_err(err)?;
        if bytes != *old {
            doc.put(&map, id, ScalarValue::Bytes(bytes)).map_err(err)?;
            changed = true;
        }
    }
    Ok(changed)
}

fn identities(doc: &AutoCommit) -> Result<ObjId, String> {
    Ok(doc
        .get(ROOT, "peer_identities_v1")
        .map_err(err)?
        .ok_or("member identities missing")?
        .1)
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// A device row as the workspace document holds it: postcard, keys and
/// signatures as the bytes they are. A delegate's row, which few devices
/// carry, is kept as the JSON its own types read and write.
#[derive(Clone)]
pub(crate) struct DeviceRow {
    pub(crate) device_cert: DeviceCert,
    pub(crate) revocations: Vec<tilo_user_identity::RevocationEntry>,
    pub(crate) admission: Option<RedemptionReceipt>,
    pub(crate) admissions: Vec<RedemptionReceipt>,
    /// Present when the device is a delegate of its person, an agent
    /// ([`super::delegates`]). Absent from every other identity's
    /// bytes.
    pub(crate) delegate: Option<DelegateRow>,
    /// The workspace delegations older builds wrote in this row: it let
    /// another device issue invitations for a workspace. Workspaces no longer have them, so nothing here reads one to
    /// act on; they are kept because the row's bytes, and the media
    /// authorization basis computed over them, are a digest a stored policy
    /// still names. Written back as they were read, never added to.
    pub(crate) workspace_delegations: Vec<WorkspaceDelegation>,
    /// The workspace took this device off its member list without deleting the
    /// row: the admission, delegation and revocation it carries stay, for the
    /// history that names them and for verification, and the row is no longer
    /// one of the workspace's devices. Only
    /// [`PeerGovernance::removed_from_member_list`](super::PeerGovernance::removed_from_member_list)
    /// reads it, and a deliberate admission of the device writes it false
    /// again.
    pub(crate) removed: bool,
    /// What admitted the device through the workspace's entry, where the
    /// workspace's entry is what let it in: the entry's digest, the role it
    /// opened at, and the request it came in under. No invitation is behind
    /// it, and no device of the host's signed it.
    pub(crate) entry: Option<EntryReceipt>,
}

impl DeviceRow {
    /// The certificate the identity row `bytes` hold, its first part, read
    /// without the rest of the row.
    ///
    /// # Errors
    ///
    /// The bytes do not start with a certificate.
    pub(crate) fn certificate_of(bytes: &[u8]) -> Result<DeviceCert, RowError> {
        postcard::take_from_bytes::<DeviceCert>(bytes)
            .map(|(cert, _)| cert)
            .map_err(RowError::Postcard)
    }

    /// The identity row `bytes` hold, as stored.
    ///
    /// A row written before rows carried a removal's mark ends where its last
    /// field did: it is the same field sequence without the mark, and the
    /// device it names is one the workspace holds.
    ///
    /// # Errors
    ///
    /// The bytes are not an identity row.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, RowError> {
        crate::work::identity_decoded();
        let stored: StoredIdentity = match postcard::from_bytes(bytes) {
            Ok(stored) => stored,
            Err(postcard::Error::DeserializeUnexpectedEnd) => {
                let (
                    device_cert,
                    revocations,
                    admission,
                    admissions,
                    delegate,
                    workspace_delegations,
                    removed,
                ) = match postcard::from_bytes::<(
                    DeviceCert,
                    Vec<tilo_user_identity::RevocationEntry>,
                    Option<RedemptionReceipt>,
                    Vec<RedemptionReceipt>,
                    Option<Vec<u8>>,
                    Vec<WorkspaceDelegation>,
                    bool,
                )>(bytes)
                {
                    Ok(row) => row,
                    Err(postcard::Error::DeserializeUnexpectedEnd) => {
                        let (
                            device_cert,
                            revocations,
                            admission,
                            admissions,
                            delegate,
                            workspace_delegations,
                        ) = postcard::from_bytes::<(
                            DeviceCert,
                            Vec<tilo_user_identity::RevocationEntry>,
                            Option<RedemptionReceipt>,
                            Vec<RedemptionReceipt>,
                            Option<Vec<u8>>,
                            Vec<WorkspaceDelegation>,
                        )>(bytes)
                        .map_err(RowError::Postcard)?;
                        (
                            device_cert,
                            revocations,
                            admission,
                            admissions,
                            delegate,
                            workspace_delegations,
                            false,
                        )
                    }
                    Err(error) => return Err(RowError::Postcard(error)),
                };
                StoredIdentity {
                    device_cert,
                    revocations,
                    admission,
                    admissions,
                    delegate,
                    workspace_delegations,
                    removed,
                    entry: None,
                }
            }
            Err(error) => return Err(RowError::Postcard(error)),
        };
        Ok(Self {
            device_cert: stored.device_cert,
            revocations: stored.revocations,
            admission: stored.admission,
            admissions: stored.admissions,
            delegate: stored
                .delegate
                .map(|json| serde_json::from_slice(&json))
                .transpose()
                .map_err(RowError::Delegate)?,
            workspace_delegations: stored.workspace_delegations,
            removed: stored.removed,
            entry: stored.entry,
        })
    }

    /// The bytes the workspace document holds for this row.
    ///
    /// # Errors
    ///
    /// A field does not encode.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, RowError> {
        let delegate = self
            .delegate
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(RowError::Delegate)?;
        postcard::to_stdvec(&StoredIdentityRef {
            device_cert: &self.device_cert,
            revocations: &self.revocations,
            admission: self.admission.as_ref(),
            admissions: &self.admissions,
            delegate: delegate.as_deref(),
            workspace_delegations: &self.workspace_delegations,
            removed: self.removed,
            entry: self.entry.as_ref(),
        })
        .map_err(RowError::Postcard)
    }
}

/// Why a device row does not read or write.
#[derive(Debug)]
pub enum RowError {
    Postcard(postcard::Error),
    Delegate(serde_json::Error),
}
impl std::fmt::Display for RowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Postcard(error) => write!(f, "identity row: {error}"),
            Self::Delegate(error) => write!(f, "identity row's delegate: {error}"),
        }
    }
}
impl std::error::Error for RowError {}

/// [`DeviceRow`] as stored, in field order.
#[derive(Deserialize)]
struct StoredIdentity {
    device_cert: DeviceCert,
    revocations: Vec<tilo_user_identity::RevocationEntry>,
    admission: Option<RedemptionReceipt>,
    admissions: Vec<RedemptionReceipt>,
    delegate: Option<Vec<u8>>,
    workspace_delegations: Vec<WorkspaceDelegation>,
    removed: bool,
    entry: Option<EntryReceipt>,
}
/// [`StoredIdentity`], borrowed to be written.
#[derive(Serialize)]
struct StoredIdentityRef<'a> {
    device_cert: &'a DeviceCert,
    revocations: &'a [tilo_user_identity::RevocationEntry],
    admission: Option<&'a RedemptionReceipt>,
    admissions: &'a [RedemptionReceipt],
    delegate: Option<&'a [u8]>,
    workspace_delegations: &'a [WorkspaceDelegation],
    removed: bool,
    entry: Option<&'a EntryReceipt>,
}

/// A workspace delegation as older builds wrote it, read for the media
/// authorization basis and nothing else. The fields are that layout's;
/// nothing here interprets them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkspaceDelegation {
    pub(crate) version: u32,
    pub(crate) workspace: String,
    /// The number the builds of that layout derived from `workspace`.
    pub(crate) workspace_id: u64,
    pub(crate) authority_device_public_key: [u8; 32],
    pub(crate) delegate: DeviceCert,
    pub(crate) permissions: WorkspaceDelegationPermissions,
    pub(crate) nonce: [u8; 16],
    pub(crate) expires_at_ns: Option<u64>,
    #[serde(with = "serde_bytes")]
    pub(crate) signature: [u8; 64],
}

/// What a [`WorkspaceDelegation`] let its delegate do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkspaceDelegationPermissions {
    pub(crate) issue_invitations: bool,
    pub(crate) admit_invited_members: bool,
}

impl Default for DeviceRow {
    fn default() -> Self {
        Self {
            device_cert: DeviceCert {
                user_public_key: [0; 32],
                device_public_key: [0; 32],
                device_label: String::new(),
                capabilities: tilo_user_identity::DeviceCapabilities {
                    can_own_workspaces: false,
                    ..tilo_user_identity::DeviceCapabilities::member()
                },
                issued_at_ns: 0,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            },
            revocations: Vec::new(),
            admission: None,
            admissions: Vec::new(),
            delegate: None,
            workspace_delegations: Vec::new(),
            removed: false,
            entry: None,
        }
    }
}

impl DeviceRow {
    pub(crate) fn has_admission(&self) -> bool {
        // An agent row is no admission, which keeps other people's agents
        // out.
        self.admission.is_some() || !self.admissions.is_empty() || self.entry.is_some()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RedemptionReceipt {
    pub(crate) invite_id: [u8; 32],
    pub(crate) user_public_key: [u8; 32],
    pub(crate) device_public_key: [u8; 32],
    pub(crate) request_id: [u8; 32],
    pub(crate) role: crate::join::Role,
}

/// What admitted a device through the workspace's entry: the entry digest it
/// came in under, the role that entry opened at, and the request it asked
/// with. The owner's entry is the grant, so no invitation and no host's key
/// is named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EntryReceipt {
    pub(crate) entry: [u8; 32],
    pub(crate) role: crate::join::Role,
    pub(crate) request_id: [u8; 32],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_written_before_the_mark_reads_as_one_the_workspace_holds() {
        let identity = DeviceRow::default();
        // The bytes a build wrote before rows carried a removal's mark: the
        // same field sequence, ending after its last field.
        let before = postcard::to_stdvec(&(
            &identity.device_cert,
            &identity.revocations,
            identity.admission.as_ref(),
            &identity.admissions,
            None::<&[u8]>,
            &identity.workspace_delegations,
        ))
        .expect("the layout before the mark writes");
        let read = DeviceRow::decode(&before).expect("a row before the mark reads");
        assert!(!read.removed, "a row that names no mark holds its device");
        assert_eq!(
            read.device_cert.device_public_key,
            identity.device_cert.device_public_key
        );
        // The bytes a build wrote after the mark and before the entry: the
        // same fields with the mark's one byte after them.
        let with_mark = [before.as_slice(), &[1]].concat();
        let read = DeviceRow::decode(&with_mark).expect("a row before the entry reads");
        assert!(read.removed, "the mark reads back");
        assert_eq!(read.entry, None, "a row before the entry names none");
        let mut marked = identity;
        marked.removed = true;
        let bytes = marked.encode().expect("a marked row writes");
        assert_eq!(
            bytes,
            [with_mark.as_slice(), &[0]].concat(),
            "today's row is the earlier one with an absent entry after it"
        );
        assert!(
            DeviceRow::decode(&bytes)
                .expect("a marked row reads")
                .removed
        );
    }
}
