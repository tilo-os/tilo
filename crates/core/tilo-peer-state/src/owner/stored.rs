#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the record's own readers are the host's file store and journal"
    )
)]
use crate::join::TransportHint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tilo_desktop_model::WorkspaceId;
use tilo_user_identity::DeviceCert;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod format;

/// The layout of a workspace's entry this build writes ([`StoredWorkspace`]):
/// the workspace named once, by its id.
pub(crate) const WORKSPACE_VERSION: u32 = 10;

/// Native-only state-file envelope. Its fields cannot be authored by UI callers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeAuthorityStorage {
    pub(crate) revision: u64,
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
    /// The record of `workspace`, as this device holds it.
    pub(crate) fn stored(&self, workspace: WorkspaceId) -> Option<&StoredWorkspace> {
        self.workspaces.get(&workspace)
    }
    /// Every workspace whose replica is installed here, with the workspace id the
    /// workspace's record names (the workspace's transport shard).
    pub fn held(&self) -> impl Iterator<Item = (WorkspaceId, WorkspaceId)> + '_ {
        self.workspaces
            .iter()
            .map(|(workspace, stored)| (*workspace, stored.workspace))
    }
    /// Every workspace an enrollment is in flight into, with the local workspace
    /// it installs into, where it names one.
    pub fn joins(&self) -> impl Iterator<Item = (WorkspaceId, Option<WorkspaceId>)> + '_ {
        self.pending
            .iter()
            .map(|(workspace, pending)| (*workspace, pending.target_workspace()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredWorkspace {
    pub version: u32,
    /// The workspace this device holds.
    pub workspace: WorkspaceId,
    /// The host this workspace's commits are ordered by: a device of its owner,
    /// this device itself for a workspace its own host keeps.
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
    pub peer: DurablePeer,
    pub generation: u64,
    pub continuity: u64,
    /// The record layout a build before the one path kept: its outgoing
    /// ownership and the answers it took for it. This build writes none and
    /// reads what an entry holds so the journal beside it replays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<crate::owner::publication::StoredPublication>,
    /// This device's own frames a peer has not been sent yet, oldest first:
    /// what makes a write durable here before any peer holds it, and sends it
    /// when one links. A frame is sent once and a peer that took it drops a
    /// second copy. This list goes once this device's writes are kept in the
    /// device's frame store ([`crate::store::Store`]), whose `holds` and whose
    /// link answers say what each peer has.
    #[serde(default, skip_serializing_if = "Frames::is_empty")]
    pub frames: Frames,
}

/// The frames a record keeps, oldest first, in runs each copy of the record shares: a device
/// offline keeps every write it made, and a copy, or one frame more, costs the same however many
/// that is. Stored as one list.
#[derive(Clone, Debug, Default)]
pub(crate) struct Frames {
    runs: Vec<std::sync::Arc<Vec<FrameToSend>>>,
    len: usize,
}

impl Frames {
    /// The frames a run holds at most.
    const RUN: usize = 64;

    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &FrameToSend> {
        self.runs.iter().flat_map(|run| run.iter())
    }

    /// The frames from the one at `from` on, reached run by run.
    pub(crate) fn iter_from(&self, from: usize) -> impl Iterator<Item = &FrameToSend> + Clone {
        let mut before = 0usize;
        let mut first = self.runs.len();
        for (at, run) in self.runs.iter().enumerate() {
            let after = before.saturating_add(run.len());
            if after > from {
                first = at;
                break;
            }
            before = after;
        }
        self.runs
            .get(first..)
            .unwrap_or_default()
            .iter()
            .flat_map(|run| run.iter())
            .skip(from.saturating_sub(before))
    }

    pub(crate) fn last(&self) -> Option<&FrameToSend> {
        self.runs.last().and_then(|run| run.last())
    }

    /// The runs, oldest first.
    pub(crate) fn runs(&self) -> &[std::sync::Arc<Vec<FrameToSend>>] {
        &self.runs
    }

    pub(crate) fn push(&mut self, frame: FrameToSend) {
        match self.runs.last_mut() {
            Some(run) if run.len() < Self::RUN => {
                if std::sync::Arc::get_mut(run).is_none() {
                    touched(run.len());
                }
                std::sync::Arc::make_mut(run).push(frame);
            }
            _ => self.runs.push(std::sync::Arc::new(vec![frame])),
        }
        self.len = self.len.saturating_add(1);
    }

    /// Take out the frame whose bytes are `bytes`, looking from the oldest, where a link takes
    /// them from: `false` when none is kept. The runs it does not hold stay shared.
    pub(crate) fn remove(&mut self, bytes: &[u8]) -> bool {
        for (at, run) in self.runs.iter_mut().enumerate() {
            let Some(found) = run.iter().position(|frame| *frame.bytes == *bytes) else {
                touched(run.len());
                continue;
            };
            touched(found.saturating_add(1).saturating_add(run.len()));
            std::sync::Arc::make_mut(run).remove(found);
            if run.is_empty() {
                self.runs.remove(at);
            }
            self.len = self.len.saturating_sub(1);
            return true;
        }
        false
    }
}

// Frames this thread copied or compared to keep them: what a test reads to see that a write's
// work on its record's frames stays the same however many the record keeps. Built for tests
// alone.
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TOUCHED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Count `frames` copied or compared.
#[cfg_attr(
    not(any(test, feature = "test-support")),
    allow(
        clippy::missing_const_for_fn,
        reason = "counts in a test build, and does nothing in any other"
    )
)]
pub(crate) fn touched(frames: usize) {
    #[cfg(any(test, feature = "test-support"))]
    TOUCHED.with(|touched| {
        touched.set(
            touched
                .get()
                .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX)),
        );
    });
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = frames;
}

/// The frames this thread copied or compared to keep them so far ([`touched`]).
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn frames_touched() -> u64 {
    TOUCHED.with(std::cell::Cell::get)
}

impl PartialEq for Frames {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}
impl Eq for Frames {}

impl FromIterator<FrameToSend> for Frames {
    fn from_iter<T: IntoIterator<Item = FrameToSend>>(frames: T) -> Self {
        let mut list = Self::default();
        for frame in frames {
            list.push(frame);
        }
        list
    }
}

impl Serialize for Frames {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for Frames {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Vec::<FrameToSend>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

/// One frame of this device's own, kept until a link carried it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FrameToSend {
    /// Shared: every change to the record copies it, and a device offline holds every write it made.
    #[serde(with = "base64_shared")]
    pub bytes: std::sync::Arc<[u8]>,
}

impl StoredWorkspace {
    /// Whether `other` gives this device the same member access in the workspace:
    /// everything this record holds besides the replica, its generation and
    /// what it published. Compared as stored, so a field the comparison
    /// cannot read counts as changed.
    pub(crate) fn same_access(&self, other: &Self) -> bool {
        let access = |record: &Self| {
            serde_json::to_vec(&(
                record.version,
                &record.workspace,
                record.workspace,
                &record.workspace_host,
                &record.device,
                &record.enrollment,
                &record.vouched,
                &record.host_hint,
                record.continuity,
            ))
            .ok()
        };
        access(self).is_some_and(|ours| Some(ours) == access(other))
    }
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredPendingEnrollment {
    /// Whether the enrolled workspace is on this device's shell, made for the join before the
    /// enrollment began.
    #[serde(default)]
    pub(crate) shown: bool,
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
    pub fn target_workspace(&self) -> Option<WorkspaceId> {
        self.shown.then(|| self.id.workspace())
    }
}

/// A replica's durable encoding as [`crate::StatePeer::durable_layout`] gives
/// it: its stretches concatenated are the bytes stored. A part no change
/// touched is the same allocation as in the encoding before, and a document
/// no change touched is shared with it, so a store keeping the workspace writes
/// only what changed ([`crate::owner::journal`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DurablePeer(crate::state::DurableParts);

impl DurablePeer {
    pub(crate) fn layout(&self) -> &crate::state::DurableParts {
        &self.0
    }
    /// The bytes whole.
    pub(crate) fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }
    /// The durable encoding `bytes`, as its stretches: bytes that do not read
    /// as one are kept whole, for the restore that reads them to refuse.
    pub(crate) fn read(bytes: &[u8]) -> Self {
        Self(
            crate::state::DurableParts::read(bytes)
                .unwrap_or_else(|_| crate::state::DurableParts::whole(bytes)),
        )
    }
}

impl From<crate::state::DurableParts> for DurablePeer {
    fn from(parts: crate::state::DurableParts) -> Self {
        Self(parts)
    }
}

impl PartialEq<Vec<u8>> for DurablePeer {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.to_vec() == *other
    }
}

impl Serialize for DurablePeer {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        base64_bytes::serialize(&self.to_vec(), serializer)
    }
}

impl<'de> Deserialize<'de> for DurablePeer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = base64_bytes::deserialize(deserializer)?;
        Ok(Self::read(&bytes))
    }
}

/// [`base64_bytes`] for bytes a record shares between its copies.
pub(crate) mod base64_shared {
    use serde::{Deserializer, Serializer};
    use std::sync::Arc;

    pub(crate) fn serialize<S: Serializer>(
        bytes: &Arc<[u8]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        super::base64_bytes::serialize(bytes, serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<[u8]>, D::Error> {
        super::base64_bytes::deserialize(deserializer).map(Arc::from)
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
