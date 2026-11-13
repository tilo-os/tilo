//! The rows a workspace's governance keeps per device: its certificate and how
//! it was admitted.
use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

use super::GovernedDelegateRow;
use crate::join::WorkspaceId;

/// A device row: its key, the bytes it was read from, and the row they hold.
pub(crate) type IdentityRow = (String, Vec<u8>, GovernedIdentity);

/// The workspace document's device rows, in key order.
pub(crate) fn identity_rows(doc: &AutoCommit) -> Result<Vec<IdentityRow>, String> {
    crate::workspace::governance_of(Some(doc))
        .identities
        .into_iter()
        .map(|(id, bytes)| {
            GovernedIdentity::decode(&bytes)
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
        .ok_or("governed identities missing")?
        .1)
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// A device row as the workspace document holds it: postcard, keys and
/// signatures as the bytes they are. A delegate's row, which few devices
/// carry, is kept as the JSON its own types read and write.
#[derive(Clone)]
pub(crate) struct GovernedIdentity {
    pub(crate) device_cert: DeviceCert,
    pub(crate) revocations: Vec<tilo_user_identity::RevocationEntry>,
    pub(crate) admission: Option<RedemptionReceipt>,
    pub(crate) admissions: Vec<RedemptionReceipt>,
    /// Present when the device is a delegate of its person: an agent, or an
    /// installed bot ([`super::delegates`]). Absent from every other identity's
    /// bytes.
    pub(crate) delegate: Option<GovernedDelegateRow>,
    /// The workspace delegations a build before [`super::DURABLE_BASE_VERSION`]
    /// wrote in this row: it let another device issue invitations for a
    /// workspace. Workspaces no longer have them, so nothing here reads one to
    /// act on; they are kept because the row's bytes, and the media
    /// authorization basis computed over them, are a digest a stored policy
    /// still names. Written back as they were read, never added to.
    pub(crate) workspace_delegations: Vec<WorkspaceDelegation>,
}

impl GovernedIdentity {
    /// The identity row `bytes` hold, as stored.
    ///
    /// # Errors
    ///
    /// The bytes are not an identity row.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, RowError> {
        crate::governance_work::identity_decoded();
        let stored: StoredIdentity = postcard::from_bytes(bytes).map_err(RowError::Postcard)?;
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
            Self::Postcard(error) => write!(f, "governed identity row: {error}"),
            Self::Delegate(error) => write!(f, "governed identity row's delegate: {error}"),
        }
    }
}
impl std::error::Error for RowError {}

/// [`GovernedIdentity`] as stored, in field order.
#[derive(Deserialize)]
struct StoredIdentity {
    device_cert: DeviceCert,
    revocations: Vec<tilo_user_identity::RevocationEntry>,
    admission: Option<RedemptionReceipt>,
    admissions: Vec<RedemptionReceipt>,
    delegate: Option<Vec<u8>>,
    workspace_delegations: Vec<WorkspaceDelegation>,
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
}

/// A workspace delegation as a build before [`super::DURABLE_BASE_VERSION`]
/// wrote it, read for the media authorization basis and nothing else. The
/// fields are that layout's; nothing here interprets them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkspaceDelegation {
    pub(crate) version: u32,
    pub(crate) room: String,
    pub(crate) workspace_id: WorkspaceId,
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

impl Default for GovernedIdentity {
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
        }
    }
}

impl GovernedIdentity {
    pub(crate) fn has_admission(&self) -> bool {
        // The governor admits a bot device itself, so its installation is
        // its admission; an agent row is not, which keeps other people's
        // agents out.
        self.admission.is_some()
            || !self.admissions.is_empty()
            || self
                .delegate
                .as_ref()
                .is_some_and(|row| row.installation.is_some())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RedemptionReceipt {
    pub(crate) invite_id: [u8; 32],
    pub(crate) user_public_key: [u8; 32],
    pub(crate) device_public_key: [u8; 32],
    pub(crate) request_id: [u8; 32],
    pub(crate) role: crate::join::MemberRole,
}
