use crate::join::TransportHint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};
use tilo_user_identity::DeviceCert;

/// Native-only state-file envelope. Its fields cannot be authored by UI callers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeAuthorityStorage {
    pub(crate) revision: u64,
    /// A state file outlives an image update and is read by whatever build the
    /// update left, so the map is written under the key it was first written
    /// under; a file written while it was named for the workspace still reads.
    #[serde(rename = "rooms", alias = "workspaces")]
    pub(crate) workspaces: BTreeMap<WorkspaceId, StoredWorkspace>,
    pub(crate) pending: BTreeMap<WorkspaceId, StoredPendingEnrollment>,
}
impl NativeAuthorityStorage {
    /// Raised by every publication of the record.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    #[must_use]
    pub fn requires(&self, workspace: WorkspaceId) -> bool {
        self.workspaces.contains_key(&workspace) || self.pending.contains_key(&workspace)
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty() && self.pending.is_empty()
    }
    /// Workspaces that are enrolled or have an enrollment in flight.
    pub fn workspaces(&self) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.workspaces.keys().chain(self.pending.keys()).copied()
    }
    /// Whether this device holds `workspace`: its replica is installed, not
    /// merely an enrollment into it in flight.
    #[must_use]
    pub fn holds(&self, workspace: WorkspaceId) -> bool {
        self.workspaces.contains_key(&workspace)
    }
    /// Every workspace whose replica is installed here, with the workspace id the
    /// workspace's record names (the workspace's transport shard).
    pub fn held(&self) -> impl Iterator<Item = (WorkspaceId, WorkspaceSlot)> + '_ {
        self.workspaces
            .iter()
            .map(|(workspace, stored)| (*workspace, WorkspaceSlot(stored.workspace_id)))
    }
    /// Every workspace an enrollment is in flight into, with the local workspace
    /// it installs into, where it names one.
    pub fn joins(&self) -> impl Iterator<Item = (WorkspaceId, Option<WorkspaceSlot>)> + '_ {
        self.pending
            .iter()
            .map(|(workspace, pending)| (*workspace, pending.target_workspace()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredWorkspace {
    pub version: u32,
    /// The workspace this device holds. Files written before this field was
    /// named for it carry `room`.
    #[serde(rename = "room")]
    pub workspace: WorkspaceId,
    /// The workspace's transport shard, as its record names it.
    #[serde(rename = "workspace")]
    pub workspace_id: u64,
    /// The host this workspace's commits are ordered by: a device of its owner,
    /// this device itself for a workspace its own host keeps. Written under
    /// `room_host`, the key builds before the vocabulary change wrote (and
    /// older ones under `governor`); a file written while the field was named
    /// for the workspace reads as the same host.
    #[serde(rename = "room_host", alias = "governor", alias = "workspace_host")]
    pub workspace_host: DeviceCert,
    pub device: DeviceCert,
    /// How this device joined the workspace its host governs. A workspace it founded
    /// itself, its own host keeping it, holds neither this nor a vouch.
    #[serde(flatten)]
    pub enrollment: Option<StoredEnrollment>,
    /// The membership answer this device's host gave it for a workspace it
    /// holds without an enrollment of its own
    /// ([`crate::NativeWorkspaceOwner::install_vouched`]). A record holds
    /// this or an enrollment, never both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vouched: Option<StoredVouch>,
    /// Where this device's own host of the workspace is reached, for a
    /// workspace founded here: the signed address its host keeps the
    /// workspace's records at ([`crate::NativeWorkspaceOwner`] founded it, so
    /// no other device admitted this one and its record holds neither an
    /// enrollment nor a vouch to say where the host is).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_hint: Option<TransportHint>,
    /// `StatePeer::durable()`: the largest field by far.
    #[serde(with = "base64_bytes")]
    pub peer: Vec<u8>,
    pub generation: u64,
    pub continuity: u64,
    /// Outgoing publication state; required on every committed workspace record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<crate::owner::publication::StoredPublication>,
    /// Original person membership command outcomes, private to the owning host.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspace_memberships:
        BTreeMap<String, crate::owner::workspace_membership::StoredMembershipReceipt>,
    /// Exact broker terminal bindings, committed with the signed workspace checkpoint.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effect_receipts: BTreeMap<String, crate::owner::effect_receipt::StoredEffectReceipt>,
    /// Retired with the shortcut commit family: a saved shortcut is one of the
    /// workspace's settings. A record kept before then still names the receipt
    /// its published checkpoint was committed under, so it stays here to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut_change: Option<crate::owner_commit::OwnerCommitReceipt>,
    /// Content-addressed cold UUID index, also anchored in signed public workspace history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_history_root: Option<[u8; 32]>,
    /// The invitations this device issued for the workspace, the challenges it is
    /// answering and the joins it admitted: what another device's join is
    /// decided against, held by every participant that answers one.
    #[serde(default)]
    pub join_ledger: crate::join_ledger::JoinLedger,
    /// The join admissions committed here, by the digest of their binding:
    /// the workspace delta that carried each one is published under its receipt.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub join_admissions: BTreeMap<String, StoredJoinReceipt>,
}

impl StoredWorkspace {
    /// Whether `other` gives this device the same standing in the workspace:
    /// everything this record holds besides the replica, its generation and
    /// what it published. Compared as stored, so a field the comparison
    /// cannot read counts as changed.
    pub(crate) fn same_standing(&self, other: &Self) -> bool {
        let standing = |record: &Self| {
            serde_json::to_vec(&(
                record.version,
                &record.workspace,
                record.workspace_id,
                &record.workspace_host,
                &record.device,
                &record.enrollment,
                &record.vouched,
                &record.host_hint,
                record.continuity,
                &record.workspace_memberships,
                &record.effect_receipts,
                &record.shortcut_change,
                &record.join_ledger,
                &record.join_admissions,
            ))
            .ok()
        };
        standing(self).is_some_and(|ours| Some(ours) == standing(other))
    }

    /// Whether this device keeps the workspace's records itself: its own host orders the
    /// workspace's commits. A replica held on a host's answer ([`Self::vouched`]) never does,
    /// even under the host's own key: the host keeps the records and answers its writes.
    pub(crate) fn hosted_here(&self) -> bool {
        self.device == self.workspace_host && self.vouched.is_none()
    }
}

/// The receipt of one join admission this device committed, by the digest of
/// its binding: what the workspace delta it published is admitted as.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredJoinReceipt {
    pub(crate) receipt: crate::owner_commit::OwnerCommitReceipt,
}

/// The signed join a device enrolled in a host-governed workspace with, and the
/// host it pinned then.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredEnrollment {
    #[serde(with = "base64_bytes")]
    pub invitation: Vec<u8>,
    #[serde(with = "base64_bytes")]
    pub enrollment_intent: Vec<u8>,
    pub enrolled_at_ns: u64,
    pub transport_hint: TransportHint,
}
/// The membership answer a host vouched for: a postcard
/// [`crate::join::JoinTransportResponse`], checked again against
/// the record whenever it is restored.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredVouch {
    #[serde(with = "base64_bytes")]
    pub bootstrap: Vec<u8>,
    /// Where the host that answered is reached, for a workspace this device
    /// holds as a further device of its person: the host hint its answer
    /// came with, host-signed like a link's. Absent from a page's vouch,
    /// which reaches its host through the session it authenticated to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<crate::join::TransportHint>,
}

/// Only a successful state-file publication creates an enrollment handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEnrollmentId {
    /// Written under the key it was first written under, as the state file it
    /// belongs to is; a handle written while the field was named for the
    /// workspace still reads.
    #[serde(rename = "room", alias = "workspace")]
    workspace: WorkspaceId,
    generation: u64,
    digest: [u8; 32],
}
impl PendingEnrollmentId {
    pub(crate) fn new(workspace: WorkspaceId, generation: u64, digest: [u8; 32]) -> Self {
        Self {
            workspace,
            generation,
            digest,
        }
    }
    #[must_use]
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
    }
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingEnrollmentStatus {
    Active,
    Cancelled,
}
/// The local slot an enrollment installs into: the workspace that is the
/// enrolled workspace, made for the join before the enrollment began. A slot
/// never changes its workspace, so installing binds nothing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrollmentTarget {
    pub workspace: WorkspaceSlot,
    /// The workspace the enrollment is into. Files from before this record
    /// bound it as a placeholder slot, under the stored name it keeps; nothing
    /// reads it back.
    #[serde(rename = "prior_room")]
    pub prior_workspace: WorkspaceId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredPendingEnrollment {
    /// `None` where the host has no local slots to bind.
    pub(crate) target: Option<EnrollmentTarget>,
    pub(crate) id: PendingEnrollmentId,
    pub(crate) intent: Vec<u8>,
    pub(crate) status: PendingEnrollmentStatus,
}
impl StoredPendingEnrollment {
    #[must_use]
    pub fn id(&self) -> &PendingEnrollmentId {
        &self.id
    }
    #[must_use]
    pub fn intent_bytes(&self) -> &[u8] {
        &self.intent
    }
    #[must_use]
    pub fn status(&self) -> PendingEnrollmentStatus {
        self.status
    }
    /// The local workspace the enrollment installs into, where the host has
    /// workspaces.
    #[must_use]
    pub fn target_workspace(&self) -> Option<WorkspaceSlot> {
        self.target.as_ref().map(|target| target.workspace)
    }
}

/// Opaque byte fields of a workspace record: base64 text in a human-readable
/// format (the owner file, the desktop state file), the unchanged `Vec<u8>`
/// encoding in a binary one.
pub(crate) mod base64_bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

    pub(crate) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&STANDARD.encode(bytes))
        } else {
            bytes.serialize(serializer)
        }
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        if deserializer.is_human_readable() {
            STANDARD
                .decode(String::deserialize(deserializer)?)
                .map_err(de::Error::custom)
        } else {
            Vec::deserialize(deserializer)
        }
    }
}
