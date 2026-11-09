//! The rows a workspace's governance keeps per device: its certificate and how
//! it was admitted.
use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

use super::GovernedDelegateRow;

/// A device row: its key, the bytes it was read from, and the row they hold.
pub(crate) type IdentityRow = (String, Vec<u8>, GovernedIdentity);

/// The workspace document's device rows, in key order.
pub(crate) fn identity_rows(doc: &AutoCommit) -> Result<Vec<IdentityRow>, String> {
    crate::workspace::governance_of(Some(doc))
        .identities
        .into_iter()
        .map(|(id, bytes)| {
            serde_json::from_slice(&bytes)
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
        let bytes = serde_json::to_vec(row).map_err(err)?;
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

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct GovernedIdentity {
    pub(crate) device_cert: DeviceCert,
    #[serde(default)]
    pub(crate) revocations: Vec<tilo_user_identity::RevocationEntry>,
    #[serde(default)]
    pub(crate) admission: Option<RedemptionReceipt>,
    #[serde(default)]
    pub(crate) admissions: Vec<RedemptionReceipt>,
    /// Present when the device is a delegate of its person: an agent, or an
    /// installed bot ([`super::delegates`]). Absent from every other identity's
    /// bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delegate: Option<GovernedDelegateRow>,
    /// The workspace delegations a build before [`super::DURABLE_BASE_VERSION`]
    /// wrote in this row: it let another device issue invitations for a
    /// workspace. Workspaces no longer have them, so nothing here reads one to
    /// act on; they are kept because the row's bytes, and the media
    /// authorization basis computed over them, are a digest a stored policy
    /// still names. Written back as they were read, never added to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) workspace_delegations: Vec<WorkspaceDelegation>,
}

/// A workspace delegation as a build before [`super::DURABLE_BASE_VERSION`]
/// wrote it, read for the media authorization basis and nothing else. The
/// fields are that layout's; nothing here interprets them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkspaceDelegation {
    pub(crate) version: u32,
    pub(crate) room: String,
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

impl Default for GovernedIdentity {
    fn default() -> Self {
        Self {
            device_cert: DeviceCert {
                user_public_key: [0; 32],
                device_public_key: [0; 32],
                device_label: String::new(),
                capabilities: tilo_user_identity::DeviceCapabilities {
                    can_own_workspaces: false,
                    can_admit_devices: false,
                    can_act_as_host: false,
                    can_read_all_workspaces: false,
                    foreign: false,
                    can_administer_devices: false,
                    agent: None,
                    stand_in: false,
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
    /// The signed invitation redeemed, in its text form
    /// ([`crate::join::encode_invitation_text`]): what lets every receiver
    /// check the admission itself, whoever wrote it. Kept once, in the
    /// workspace's per-invite ledger; an identity row's copy leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) invitation: Option<String>,
}
impl RedemptionReceipt {
    /// The receipt as an identity row keeps it: without the invitation.
    pub(crate) fn on_row(&self) -> Self {
        Self {
            invitation: None,
            ..self.clone()
        }
    }
}
