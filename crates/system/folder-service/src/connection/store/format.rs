//! Bounded private wire encoding. No caller-supplied serializer reaches the log.
use super::StoreError;
use serde::Serialize;
use std::io::{self, Write};
pub(super) const MAX_BYTES: usize = 128 * 1024;
struct Capped(Vec<u8>);
impl Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > MAX_BYTES)
        {
            return Err(io::Error::other("record bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    let mut writer = Capped(Vec::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| StoreError::LimitExceeded)?;
    Ok(writer.0)
}

use super::{
    Conflict, ConflictId, ConflictRecord, ConnectionId, ConnectionPolicy, ConnectionRecord,
    Decision, Encoding, EndpointBinding, EndpointIdentity, EndpointLocator, EntryState, Lifecycle,
    NativeName, OperationId, OperationIntent, OperationReceipt, Path, ReceiptStage, RecoveryEntry,
    RecoveryId, RecoveryReason, RecoveryState, RelativePath, Side, storage,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", content = "units", deny_unknown_fields)]
pub(super) enum NativePath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}
impl NativePath {
    pub fn locator(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::Unix(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::Windows(path.as_os_str().encode_wide().collect())
        }
    }
    pub fn to_locator(&self) -> Result<EndpointLocator, StoreError> {
        let path = match self {
            #[cfg(unix)]
            Self::Unix(bytes) if bytes.len() <= 32768 && !bytes.contains(&0) => {
                use std::os::unix::ffi::OsStringExt;
                std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes.clone()))
            }
            #[cfg(windows)]
            Self::Windows(units) if units.len() <= 32768 && !units.contains(&0) => {
                use std::os::windows::ffi::OsStringExt;
                std::path::PathBuf::from(std::ffi::OsString::from_wide(units))
            }
            _ => return Err(StoreError::Corrupt),
        };
        EndpointLocator::new(&path).map_err(|_| StoreError::Corrupt)
    }
    pub fn relative(path: &RelativePath) -> Self {
        match path.encoding() {
            Encoding::Unix => Self::Unix(
                path.components()
                    .iter()
                    .enumerate()
                    .flat_map(|(i, name)| {
                        let mut value = if i == 0 { Vec::new() } else { vec![b'/'] };
                        if let NativeName::Unix(v) = name {
                            value.extend(v);
                        }
                        value
                    })
                    .collect(),
            ),
            Encoding::Windows => Self::Windows(
                path.components()
                    .iter()
                    .enumerate()
                    .flat_map(|(i, name)| {
                        let mut value = if i == 0 { Vec::new() } else { vec![92] };
                        if let NativeName::Windows(v) = name {
                            value.extend(v);
                        }
                        value
                    })
                    .collect(),
            ),
        }
    }
    pub fn to_relative(&self) -> Result<RelativePath, StoreError> {
        match self {
            Self::Unix(v) => RelativePath::unix(v),
            Self::Windows(v) => RelativePath::windows(v),
        }
        .map_err(|_| StoreError::Corrupt)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum State {
    File {
        sha256: [u8; 32],
        bytes: u64,
        executable: bool,
    },
    Directory,
}
impl From<EntryState> for State {
    fn from(value: EntryState) -> Self {
        match value {
            EntryState::File {
                sha256,
                bytes,
                executable,
            } => Self::File {
                sha256,
                bytes,
                executable,
            },
            EntryState::Directory => Self::Directory,
        }
    }
}
impl From<State> for EntryState {
    fn from(value: State) -> Self {
        match value {
            State::File {
                sha256,
                bytes,
                executable,
            } => Self::File {
                sha256,
                bytes,
                executable,
            },
            State::Directory => Self::Directory,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub identity: storage::Identity,
    pub locator: NativePath,
}
impl From<EndpointBinding> for Binding {
    fn from(value: EndpointBinding) -> Self {
        Self {
            identity: value.identity.into(),
            locator: NativePath::locator(value.locator.path()),
        }
    }
}
impl Binding {
    fn decode(&self) -> Result<EndpointBinding, StoreError> {
        Ok(EndpointBinding {
            identity: EndpointIdentity {
                volume: self.identity.volume,
                root: self.identity.root,
            },
            locator: self.locator.to_locator()?,
        })
    }
}
/// Inert serialized IDs and history. These are never access capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChainOperationIdV1 {
    pub connection: [u8; 16],
    pub serial: u64,
}
impl ChainOperationIdV1 {
    pub(super) fn operation(self) -> Result<OperationId, StoreError> {
        if self.serial == 0 {
            return Err(StoreError::Corrupt);
        }
        Ok(OperationId {
            connection: ConnectionId(self.connection),
            serial: self.serial,
        })
    }
}
impl TryFrom<OperationId> for ChainOperationIdV1 {
    type Error = StoreError;
    fn try_from(id: OperationId) -> Result<Self, StoreError> {
        if id.serial == 0 {
            return Err(StoreError::Corrupt);
        }
        Ok(Self {
            connection: id.connection.0,
            serial: id.serial,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryChainRefV1 {
    pub anchor: ChainOperationIdV1,
    pub digest: [u8; 32],
    pub index: u8,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub predecessor: Option<ChainOperationIdV1>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryChainCursorV1 {
    pub anchor: ChainOperationIdV1,
    pub digest: [u8; 32],
    pub next_index: u8,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub predecessor: Option<ChainOperationIdV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub current: Option<ChainOperationIdV1>,
    pub progress_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryChainScheduleV1 {
    pub revision: u64,
    pub control_sequence: u64,
    pub state: DirectoryChainScheduleState,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum PauseOrCancel {
    Pause,
    Cancel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum DirectoryChainScheduleState {
    Running,
    Stopping { reason: PauseOrCancel },
    Stopped { reason: PauseOrCancel },
    Abandoning,
    Disconnected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ChainRetirementReason {
    Completed,
    Abandoned,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryChainRetirementV1 {
    pub reason: ChainRetirementReason,
    pub cursor: DirectoryChainCursorV1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ChainStopKind {
    NeverBegun,
    StagedOnly,
    PublishedAccounted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum DirectoryChainStopEvidenceV1 {
    Regular {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        evidence: Option<BoundaryEvidence>,
    },
    Directory {
        fact: DirectoryBoundaryFact,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryChainStopBoundaryV1 {
    pub schedule_revision: u64,
    pub operation_revision: u64,
    pub kind: ChainStopKind,
    pub evidence: DirectoryChainStopEvidenceV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryChainCommitV1 {
    pub member: DirectoryChainRefV1,
    pub expected_progress: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum DirectoryChainControlTransitionV1 {
    StopRequested { reason: PauseOrCancel },
    StopAcknowledged,
    Resume,
    AbandonRequested,
    Retire,
}

// Immutable review facts are device-local consent evidence, never live grants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewNameKeyV1 {
    pub path: NativePath,
    pub components: Vec<Vec<u8>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ReviewNamePolicyV1 {
    Exact {},
    Canonical { keys: Vec<ReviewNameKeyV1> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialChainReviewV1 {
    pub version: u8,
    pub connection: [u8; 16],
    pub original_revision: u64,
    pub fence: u64,
    pub preview: [u8; 32],
    pub endpoints: [storage::Identity; 2],
    pub encodings: [u8; 2],
    pub policies: [ReviewNamePolicyV1; 2],
    pub counts: [u32; 2],
    pub count: u32,
    pub root: [u8; 32],
    pub original: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialReviewSideV1 {
    pub ordinal: u32,
    #[serde(deserialize_with = "strict_review_state")]
    pub state: State,
}
// Empty side uses an explicit tag: omitted fields/null never invent absence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum InitialReviewEntrySideV1 {
    Absent {},
    Present { entry: InitialReviewSideV1 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialChainReviewEntryV1 {
    pub version: u8,
    pub connection: [u8; 16],
    pub origin: u64,
    pub ordinal: u32,
    pub path: NativePath,
    pub sides: [InitialReviewEntrySideV1; 2],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum InitialReviewCurrentV1 {
    BetweenMembers {
        step: u32,
        entry: u32,
        anchor: ChainOperationIdV1,
        digest: [u8; 32],
        next_index: u8,
        predecessor: ChainOperationIdV1,
    },
    BetweenSteps {
        last_step: u32,
        completion_digest: [u8; 32],
    },
    ReviewComplete {
        completion_digest: [u8; 32],
    },
    StepLimit {
        progress_digest: [u8; 32],
    },
    Continuation {
        operation: ChainOperationIdV1,
        provenance_digest: [u8; 32],
    },
    AbandoningChain {
        step: u32,
        entry: u32,
        anchor: ChainOperationIdV1,
        digest: [u8; 32],
        cut: ReviewAbandonCut,
    },
    None {},
    Chain {
        step: u32,
        entry: u32,
        anchor: ChainOperationIdV1,
        digest: [u8; 32],
        operation: ChainOperationIdV1,
    },
    Equality {
        step: u32,
        entry: u32,
        operation: ChainOperationIdV1,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialReviewCursorV1 {
    pub version: u8,
    pub connection: [u8; 16],
    pub review: [u8; 32],
    pub origin: u64,
    pub progress_revision: u64,
    pub schedule_revision: u64,
    pub control_sequence: u64,
    pub state: DirectoryChainScheduleState,
    pub next_step: u32,
    pub current: InitialReviewCurrentV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum InitialReviewLinkKindV1 {
    Chain {
        anchor: ChainOperationIdV1,
        digest: [u8; 32],
        review_revision: u64,
    },
    Equality {},
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialReviewLinkV1 {
    pub version: u8,
    pub connection: [u8; 16],
    pub review: [u8; 32],
    pub origin: u64,
    pub step: u32,
    pub entry: u32,
    pub entry_digest: [u8; 32],
    pub operation: ChainOperationIdV1,
    pub kind: InitialReviewLinkKindV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum InitialReviewControlTransitionV1 {
    StopRequested { reason: PauseOrCancel },
    StopAcknowledged {},
    Resume {},
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialReviewControlV1 {
    pub version: u8,
    pub connection: [u8; 16],
    pub review: [u8; 32],
    pub origin: u64,
    pub expected_progress: u64,
    pub expected_schedule: u64,
    pub control_sequence: u64,
    pub current: InitialReviewCurrentV1,
    pub transition: InitialReviewControlTransitionV1,
}
// Closed canonical inner control body, not a serializable Change wrapper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewedInnerControlV1 {
    pub connection: [u8; 16],
    pub anchor: ChainOperationIdV1,
    pub digest: [u8; 32],
    pub expected_progress: u64,
    pub expected_schedule: u64,
    pub control_sequence: u64,
    pub transition: DirectoryChainControlTransitionV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EqualityUntouchedV1 {
    pub version: u8,
    pub connection: [u8; 16],
    pub review: [u8; 32],
    pub origin: u64,
    pub step: u32,
    pub entry: u32,
    pub operation: ChainOperationIdV1,
    pub admission_digest: [u8; 32],
    pub intent_digest: [u8; 32],
    pub before_revision: u64,
    pub before_progress: u64,
    pub schedule_revision: u64,
    pub control_sequence: u64,
    pub fence: u64,
    pub stage: u8,
    pub resulting_revision: u64,
    pub resulting_progress: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum InitialReviewStopBoundaryV1 {
    EqualityUntouched { fact: EqualityUntouchedV1 },
}
impl InitialReviewStopBoundaryV1 {
    pub fn fact(&self) -> &EqualityUntouchedV1 {
        let Self::EqualityUntouched { fact } = self;
        fact
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Connection {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub initial_empty: Option<InitialEmptyBaselineV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub directory_chain: Option<DirectoryChainCursorV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub directory_chain_schedule: Option<DirectoryChainScheduleV1>,
    pub id: [u8; 16],
    pub revision: u64,
    pub source: Binding,
    pub destination: Binding,
    pub retention: u64,
    pub lifecycle: u8,
    pub baseline: Option<u64>,
    pub baseline_root: Option<[u8; 32]>,
    pub baseline_count: u32,
    pub fence: u64,
    pub serial: u64,
    pub pending: Option<u64>,
}
impl Connection {
    pub fn new(
        id: ConnectionId,
        source: EndpointBinding,
        destination: EndpointBinding,
        policy: ConnectionPolicy,
    ) -> Result<Self, StoreError> {
        let value = Self {
            initial_empty: None,
            directory_chain: None,
            directory_chain_schedule: None,
            id: id.0,
            revision: 1,
            source: source.into(),
            destination: destination.into(),
            retention: policy.deletion_retention_seconds,
            lifecycle: 0,
            baseline: None,
            baseline_root: None,
            baseline_count: 0,
            fence: 0,
            serial: 0,
            pending: None,
        };
        value.decode()?;
        Ok(value)
    }
    pub fn decode(&self) -> Result<ConnectionRecord, StoreError> {
        super::initial_baseline::validate_history(self)?;
        let source = self.source.decode()?;
        let destination = self.destination.decode()?;
        if self.baseline_count > 4096
            || (self.baseline.is_none()
                && (self.baseline_root.is_some() || self.baseline_count != 0))
            || (self.baseline_count > 0 && self.baseline_root.is_none())
            || self.revision == 0
            || self.retention != 2_592_000
            || source.identity == destination.identity
            || source.locator == destination.locator
            || source.identity.volume == [0; 32]
            || destination.identity.volume == [0; 32]
            || source.identity.root == [0; 32]
            || destination.identity.root == [0; 32]
        {
            return Err(StoreError::Corrupt);
        }
        let lifecycle = match self.lifecycle {
            0 => Lifecycle::Active,
            1 => Lifecycle::Disconnecting,
            2 => Lifecycle::Disconnected,
            _ => return Err(StoreError::Corrupt),
        };
        if (self.lifecycle == 0) != (self.fence == 0)
            || self
                .pending
                .is_some_and(|serial| serial == 0 || serial > self.serial)
        {
            return Err(StoreError::Corrupt);
        }
        Ok(ConnectionRecord {
            id: ConnectionId(self.id),
            revision: self.revision,
            source,
            destination,
            policy: ConnectionPolicy {
                deletion_retention_seconds: self.retention,
            },
            lifecycle,
            baseline_revision: self.baseline,
            fence_generation: self.fence,
        })
    }
}
pub(super) const READ_QUANTUM: u64 = 4 * 1024 * 1024;
pub(super) const READ_LIMIT: u64 = 1024 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeBudget {
    pub version: u8,
    pub reserved: u64,
    pub sequence: u64,
}
impl NativeBudget {
    pub fn initial() -> Self {
        Self {
            version: 1,
            reserved: 0,
            sequence: 0,
        }
    }
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1
            || self.reserved > READ_LIMIT
            || self.reserved % READ_QUANTUM != 0
            || self.sequence > 256
            || (self.sequence == 0) != (self.reserved == 0)
            || self
                .sequence
                .checked_mul(READ_QUANTUM)
                .is_none_or(|n| n > self.reserved)
        {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialReviewFact {
    pub preview: [u8; 32],
    pub selection: [u8; 32],
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BoundaryEvidence {
    pub attempt: u32,
    pub displaced: Option<Vec<u8>>,
    pub outcome: Option<Vec<u8>>,
}
impl BoundaryEvidence {
    fn validate(&self) -> Result<(), StoreError> {
        use super::super::transaction::native::descriptor as d;
        if !(1..=8).contains(&self.attempt) {
            return Err(StoreError::Corrupt);
        }
        if let Some(bytes) = &self.displaced {
            if !matches!(
                d::decode(bytes).map_err(|_| StoreError::Corrupt)?.body,
                d::Body::Displaced(_)
            ) {
                return Err(StoreError::Corrupt);
            }
        }
        if let Some(bytes) = &self.outcome {
            if !matches!(
                d::decode(bytes).map_err(|_| StoreError::Corrupt)?.body,
                d::Body::Outcome(_)
            ) {
                return Err(StoreError::Corrupt);
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(in crate::connection) enum DirectoryBoundaryFact {
    Untouched,
    Staged {
        attempt: u32,
    },
    Published {
        attempt: u32,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        outcome: Option<Vec<u8>>,
    },
}
impl DirectoryBoundaryFact {
    pub(in crate::connection) fn kind(&self) -> u8 {
        match self {
            Self::Untouched => 0,
            Self::Staged { .. } => 1,
            Self::Published { .. } => 3,
        }
    }
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        self.validate_mode(false)
    }
    pub(super) fn validate_history_candidate(&self) -> Result<(), StoreError> {
        self.validate_mode(true)
    }
    fn validate_mode(&self, historical: bool) -> Result<(), StoreError> {
        match self {
            Self::Staged { attempt } | Self::Published { attempt, .. }
                if !(1..=8).contains(attempt) =>
            {
                return Err(StoreError::Corrupt);
            }
            Self::Published {
                outcome: Some(bytes),
                ..
            } => {
                if !matches!(
                    (if historical {
                        super::super::transaction::native::descriptor::directory::decode_history(
                            bytes,
                        )
                    } else {
                        super::super::transaction::native::descriptor::directory::decode(bytes)
                    })
                    .map_err(|_| StoreError::Corrupt)?
                    .body,
                    super::super::transaction::native::descriptor::directory::Body::Outcome(_)
                ) {
                    return Err(StoreError::Corrupt);
                }
            }
            _ => (),
        }
        Ok(())
    }
}
/// Historical explicit selection, distinct from the observed reconciliation decision.
/// Only the dedicated producer may turn a freshly verified selection into admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum SelectedVersion {
    Tombstone {
        operation: u64,
        revision: u64,
        side: u8,
        snapshot: [u8; 32],
    },
    Recovery {
        operation: u64,
        slot: u32,
        content: State,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SelectedConflict {
    pub serial: u64,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum SelectedAction {
    Restore {
        version: u8,
        target: u8,
        replace: bool,
        revision: u64,
        fence: u64,
        selected: SelectedVersion,
        conflict: Option<SelectedConflict>,
    },
}
impl SelectedAction {
    fn validate(&self, operation: &Operation) -> Result<(), StoreError> {
        let Self::Restore {
            version,
            target,
            revision,
            selected,
            conflict,
            ..
        } = self;
        if *version != 1
            || *target > 1
            || *revision != operation.expected
            || operation.native_budget.is_none()
            || conflict.as_ref().is_some_and(|value| {
                value.serial == 0 || value.serial >= operation.serial || value.revision == 0
            })
        {
            return Err(StoreError::Corrupt);
        }
        match selected {
            SelectedVersion::Recovery {
                operation: serial,
                slot,
                content,
            } if *serial > 0
                && *serial < operation.serial
                && *slot < 128
                && matches!(content, State::File { .. }) => {}
            SelectedVersion::Tombstone {
                operation: serial,
                revision: origin_revision,
                side,
                snapshot,
            } if *serial > 0
                && *serial < operation.serial
                && *side <= 1
                && *origin_revision > 0
                && *snapshot != [0; 32]
                && conflict.as_ref().is_some_and(|value| {
                    value.serial >= *serial
                        && (value.serial != *serial || value.revision == *origin_revision)
                }) => {}
            _ => return Err(StoreError::Corrupt),
        }
        Ok(())
    }
}
/// Exact historical consents retained by a new completion, never live authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SelectedAttentionRefV1 {
    pub operation: u64,
    pub operation_revision: u64,
    pub applied_revision: u64,
    pub fence: u64,
    pub outcome: [u8; 32],
    pub conflict: u64,
    pub conflict_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SelectedAttentionCarryV1 {
    pub version: u8,
    pub prior: Vec<SelectedAttentionRefV1>,
}
impl SelectedAttentionCarryV1 {
    pub fn validate(&self) -> Result<(), StoreError> {
        let mut previous = 0;
        let mut conflicts = std::collections::BTreeSet::new();
        if self.version != 1 || self.prior.is_empty() || self.prior.len() > 127 {
            return Err(StoreError::Corrupt);
        }
        for fact in &self.prior {
            if fact.operation <= previous
                || fact.applied_revision == 0
                || fact.operation_revision < fact.applied_revision
                || fact.outcome == [0; 32]
                || fact.conflict == 0
                || fact.conflict_revision == 0
                || !conflicts.insert(fact.conflict)
            {
                return Err(StoreError::Corrupt);
            }
            previous = fact.operation;
        }
        Ok(())
    }
}
fn selected_attention_carry<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<SelectedAttentionCarryV1, D::Error> {
    let value = SelectedAttentionCarryV1::deserialize(deserializer)?;
    value
        .validate()
        .map_err(|_| serde::de::Error::custom("invalid selected attention carry"))?;
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SelectedAppliedFact {
    pub revision: u64,
    pub fence: u64,
    pub outcome: [u8; 32],
    pub attention: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Operation {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub directory_chain_anchor: Option<Vec<u8>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub directory_chain_ref: Option<DirectoryChainRefV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub directory_chain_retired: Option<DirectoryChainRetirementV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub directory_chain_stop_boundary: Option<DirectoryChainStopBoundaryV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub native_kind: Option<NativeOperationKind>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub native_directory_boundary: Option<DirectoryBoundaryFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_applied: Option<SelectedAppliedFact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_capture_displaced: Vec<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_capture: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<SelectedAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_boundary_evidence: Option<BoundaryEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_boundary: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_identity: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_review: Option<InitialReviewFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_budget: Option<NativeBudget>,
    pub connection: [u8; 16],
    pub serial: u64,
    pub revision: u64,
    pub expected: u64,
    pub baseline: Option<u64>,
    pub path: NativePath,
    pub source: Option<State>,
    pub destination: Option<State>,
    pub source_binding: storage::Identity,
    pub destination_binding: storage::Identity,
    pub decision: u8,
    pub stage: u8,
    pub recovery: Vec<u32>,
}
impl Operation {
    pub fn new(
        intent: OperationIntent,
        serial: u64,
        connection: &Connection,
    ) -> Result<Self, StoreError> {
        let decision = match intent.decision {
            Decision::VerifyEqual => 0,
            Decision::InitialPreview => 5,
            Decision::Propagate { from: Side::Source } => 1,
            Decision::Propagate {
                from: Side::Destination,
            } => 2,
            Decision::RecoverDeletion { at: Side::Source } => 3,
            Decision::RecoverDeletion {
                at: Side::Destination,
            } => 4,
            Decision::Conflict(kind) => 10 + conflict_code(kind),
            _ => return Err(StoreError::RecoveryRequired),
        };
        Ok(Self {
            directory_chain_anchor: None,
            directory_chain_ref: None,
            directory_chain_retired: None,
            directory_chain_stop_boundary: None,
            native_kind: None,
            selected_applied: None,
            native_capture_displaced: Vec::new(),
            native_capture: None,
            action: None,
            native_budget: None,
            initial_review: None,
            native_identity: None,
            native_directory_boundary: None,
            native_boundary: None,
            native_boundary_evidence: None,
            connection: intent.connection.0,
            serial,
            revision: next(intent.expected_revision)?,
            expected: intent.expected_revision,
            baseline: intent.expected_baseline,
            path: NativePath::relative(&intent.path),
            source: intent.source_before.map(Into::into),
            destination: intent.destination_before.map(Into::into),
            source_binding: connection.source.identity.clone(),
            destination_binding: connection.destination.identity.clone(),
            decision,
            stage: 0,
            recovery: Vec::new(),
        })
    }
    pub fn validate_capture(
        &self,
        capture: &super::super::transaction::native::descriptor::ConflictSnapshot,
    ) -> Result<(), StoreError> {
        let endpoint = |value: &storage::Identity| {
            let mut bytes = [0; 64];
            bytes[..32].copy_from_slice(&value.volume);
            bytes[32..].copy_from_slice(&value.root);
            bytes
        };
        if self.native_budget.is_none()
            || capture.connection != self.connection
            || capture.operation != self.serial
            || capture.admission != next(self.expected)?
            || capture.revision > self.revision
            || capture.baseline != self.baseline
            || capture.path != self.path.to_relative()?
            || capture.endpoints
                != [
                    endpoint(&self.source_binding),
                    endpoint(&self.destination_binding),
                ]
        {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
    fn validate_native_identity(&self) -> Result<(), StoreError> {
        if self.native_kind == Some(NativeOperationKind::DirectoryV1) {
            return self.validate_directory_identity();
        }
        use super::super::transaction::native::descriptor as d;
        let Some(bytes) = &self.native_identity else {
            return Ok(());
        };
        let fact = d::Admission::decode(bytes).map_err(|_| StoreError::Corrupt)?;
        let endpoint = |identity: &storage::Identity| {
            let mut value = [0; 64];
            value[..32].copy_from_slice(&identity.volume);
            value[32..].copy_from_slice(&identity.root);
            value
        };
        let matches_state = |fact: &d::State, planned: &Option<State>| match (fact, planned) {
            (d::State::Absent, None) => true,
            (
                d::State::Regular { content, .. },
                Some(State::File {
                    sha256,
                    bytes,
                    executable,
                }),
            ) => {
                content.digest == *sha256
                    && content.length == *bytes
                    && content.executable == *executable
            }
            _ => false,
        };
        if self.native_budget.is_none()
            || fact.connection != self.connection
            || fact.operation != self.serial
            || fact.revision != self.expected
            || fact.baseline != self.baseline
            || fact.decision != self.decision
            || fact.direction != u8::from(matches!(self.decision, 2 | 3))
            || fact.path != self.path.to_relative()?
            || fact.endpoints
                != [
                    endpoint(&self.source_binding),
                    endpoint(&self.destination_binding),
                ]
            || !matches_state(&fact.states[0], &self.source)
            || !matches_state(&fact.states[1], &self.destination)
        {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep operation/action/revision joins and the complete atomic durable-row transition together."
    )]
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
    )]
    pub fn decode(&self) -> Result<OperationReceipt, StoreError> {
        if let Some(bytes) = &self.native_capture {
            let capture =
                super::super::transaction::native::descriptor::ConflictSnapshot::decode(bytes)
                    .map_err(|_| StoreError::Corrupt)?;
            self.validate_capture(&capture)?;
            use super::super::transaction::native::descriptor as d;
            if let Some(SelectedAction::Restore {
                target, replace, ..
            }) = &self.action
            {
                if *replace
                    || !matches!(self.stage, 0 | 1 | 10)
                    || capture.versions.len() != 2
                    || capture.versions.iter().any(|version| version.role != 0)
                    || !capture.versions.iter().any(|version| {
                        version.side == *target && matches!(version.state, d::State::Regular { .. })
                    })
                {
                    return Err(StoreError::Corrupt);
                }
            }
            if self.native_capture_displaced.len()
                != capture
                    .versions
                    .iter()
                    .filter(|version| version.displaced.is_some())
                    .count()
            {
                return Err(StoreError::Corrupt);
            }
            for bytes in &self.native_capture_displaced {
                let clue = d::decode(bytes).map_err(|_| StoreError::Corrupt)?;
                let d::Body::Displaced(body) = &clue.body else {
                    return Err(StoreError::Corrupt);
                };
                if clue.header.connection != self.connection || clue.header.operation != self.serial || body.barriers != 3
                    || !capture.versions.iter().any(|version| version.side == body.side && version.displaced == Some(clue.digest)
                        && matches!(&version.state, d::State::Regular { object, .. } | d::State::Unsupported { object, .. } if object == &body.object)) {
                    return Err(StoreError::Corrupt);
                }
            }
        } else if !self.native_capture_displaced.is_empty() {
            return Err(StoreError::Corrupt);
        }
        if let Some(action) = &self.action {
            action.validate(self)?;
        }
        self.validate_native_identity()?;
        if self.native_boundary.is_some() && (self.stage != 7 || self.native_budget.is_none()) {
            return Err(StoreError::Corrupt);
        }
        if self.native_kind == Some(NativeOperationKind::DirectoryV1) {
            if self.native_boundary_evidence.is_some() {
                return Err(StoreError::Corrupt);
            }
            match (&self.native_directory_boundary, self.native_boundary) {
                (None, None) => (),
                (Some(fact), Some(kind)) if fact.kind() == kind => {
                    if super::chain::absent_first_directory(self)? {
                        fact.validate_history_candidate()?;
                    } else {
                        fact.validate()?;
                    }
                }
                _ => return Err(StoreError::Corrupt),
            }
        } else {
            if self.native_directory_boundary.is_some() {
                return Err(StoreError::Corrupt);
            }
            match (self.native_boundary, &self.native_boundary_evidence) {
                (None, None) => {}
                (Some(0), None) if self.native_identity.is_some() && self.recovery.is_empty() => {}
                (Some(1..=3), Some(evidence)) => evidence.validate()?,
                _ => return Err(StoreError::Corrupt),
            }
        }
        if let Some(budget) = &self.native_budget {
            budget.validate()?;
        }
        if let Some(review) = &self.initial_review {
            if self.native_budget.is_none()
                || self.baseline.is_some()
                || review.preview == [0; 32]
                || review.selection == [0; 32]
                || review.revision != self.expected
            {
                return Err(StoreError::Corrupt);
            }
        }
        let decision = match self.decision {
            0 => Decision::VerifyEqual,
            1 => Decision::Propagate { from: Side::Source },
            2 => Decision::Propagate {
                from: Side::Destination,
            },
            3 => Decision::RecoverDeletion { at: Side::Source },
            4 => Decision::RecoverDeletion {
                at: Side::Destination,
            },
            5 if self.action.is_some() => Decision::InitialPreview,
            10..=14 => Decision::Conflict(conflict_from(self.decision - 10)?),
            _ => return Err(StoreError::Corrupt),
        };
        let stage = match self.stage {
            0 => ReceiptStage::Intent,
            1 => ReceiptStage::Preserved,
            2 => ReceiptStage::Published,
            3 => ReceiptStage::Verified,
            4 => ReceiptStage::BaselineCommitted,
            5 => ReceiptStage::Aborted,
            7 if self.native_boundary.is_some() => ReceiptStage::AccountedPending,
            8 if self.native_capture.is_some() && self.action.is_none() => {
                ReceiptStage::CapturedConflict
            }
            10 if self.action.is_some() && self.native_capture.is_some() => {
                ReceiptStage::SelectedCollision
            }
            9 if self.action.is_some()
                && self.native_capture.is_none()
                && self.selected_applied.as_ref().is_some_and(|fact| {
                    fact.revision <= self.revision
                        && fact.revision > self.expected
                        && fact.outcome != [0; 32]
                        && self.action.as_ref().is_some_and(|action| {
                            let SelectedAction::Restore { fence, .. } = action;
                            *fence == fact.fence
                        })
                }) =>
            {
                ReceiptStage::SelectedApplied
            }
            6 if self.recovery.is_empty()
                && self.action.as_ref().is_some_and(|action| {
                    let SelectedAction::Restore { target, .. } = action;
                    if *target == 0 {
                        self.source.is_none()
                    } else {
                        self.destination.is_none()
                    }
                }) =>
            {
                ReceiptStage::PreparedNoDisplacement
            }
            6 if self.native_kind == Some(NativeOperationKind::DirectoryV1) => {
                ReceiptStage::PreparedNoDisplacement
            }
            6 if self.action.is_none()
                && self.recovery.is_empty()
                && matches!(
                    (self.decision, &self.source, &self.destination),
                    (1, Some(State::File { .. }), None) | (2, None, Some(State::File { .. }))
                ) =>
            {
                ReceiptStage::PreparedNoDisplacement
            }
            _ => return Err(StoreError::Corrupt),
        };
        if (self.selected_applied.is_some() && self.stage != 9)
            || self.serial == 0
            || self.recovery.len() > 128
            || self
                .recovery
                .iter()
                .zip(self.recovery.iter().skip(1))
                .any(|(a, b)| a >= b)
        {
            return Err(StoreError::Corrupt);
        }
        let id = OperationId {
            connection: ConnectionId(self.connection),
            serial: self.serial,
        };
        Ok(OperationReceipt {
            action: match (&self.action, self.native_capture.is_some()) {
                (Some(action), true) => {
                    super::ExecutionAction::SelectedCollision(super::SelectedExecution {
                        action: action.clone(),
                    })
                }
                (Some(action), false) => {
                    super::ExecutionAction::Selected(super::SelectedExecution {
                        action: action.clone(),
                    })
                }
                (None, true) => super::ExecutionAction::Captured,
                (None, false) => super::ExecutionAction::Reconcile,
            },
            id,
            revision: self.revision,
            stage,
            intent: OperationIntent {
                connection: id.connection,
                expected_revision: self.expected,
                expected_baseline: self.baseline,
                path: self.path.to_relative()?,
                source_before: self.source.clone().map(Into::into),
                destination_before: self.destination.clone().map(Into::into),
                decision,
            },
            recovery: self
                .recovery
                .iter()
                .map(|slot| RecoveryId {
                    operation: id,
                    slot: *slot,
                })
                .collect(),
        })
    }
}
fn conflict_code(kind: Conflict) -> u8 {
    match kind {
        Conflict::DivergentEdits => 0,
        Conflict::EditDelete => 1,
        Conflict::FileDirectory => 2,
        Conflict::NameCollision => 3,
        Conflict::UnrepresentableName => 4,
    }
}
fn conflict_from(code: u8) -> Result<Conflict, StoreError> {
    match code {
        0 => Ok(Conflict::DivergentEdits),
        1 => Ok(Conflict::EditDelete),
        2 => Ok(Conflict::FileDirectory),
        3 => Ok(Conflict::NameCollision),
        4 => Ok(Conflict::UnrepresentableName),
        _ => Err(StoreError::Corrupt),
    }
}
pub(super) fn next(value: u64) -> Result<u64, StoreError> {
    value.checked_add(1).ok_or(StoreError::LimitExceeded)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::connection) struct DirectoryAncestorPatchV1 {
    pub version: u8,
    pub admission: [u8; 32],
    pub prefixes: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::connection) enum NativeOperationKind {
    DirectoryV1,
}
/// Exact registered native name and immutable descriptor clues, never authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeRegistration {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub native_kind: Option<NativeOperationKind>,
    pub connection: [u8; 16],
    pub serial: u64,
    pub attempt: u32,
    pub nonce: [u8; 16],
    pub side: u8,
    pub admission: u64,
    pub baseline: Option<u64>,
    pub fence: u64,
    pub intent: Option<Vec<u8>>,
    pub stage: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_displaced: Option<Vec<u8>>,
}
impl NativeRegistration {
    pub fn key(&self) -> Vec<u8> {
        let mut key = key(5, self.connection, self.serial);
        key.extend(self.attempt.to_be_bytes());
        key
    }
    fn validate_scalars(&self) -> Result<(), StoreError> {
        if self.serial == 0
            || !(1..=8).contains(&self.attempt)
            || self.nonce == [0; 16]
            || self.side > 1
            || self.admission == 0
        {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
    /// Structural candidate only: append/open/checkpoint must join an Operation.
    pub(super) fn validate_history_candidate(&self) -> Result<(), StoreError> {
        if self.native_kind == Some(NativeOperationKind::DirectoryV1) && self.baseline.is_none() {
            self.validate_scalars()?;
            self.validate_directory_mode(true)
        } else {
            self.validate()
        }
    }
    pub fn validate(&self) -> Result<(), StoreError> {
        self.validate_scalars()?;
        if self.native_kind == Some(NativeOperationKind::DirectoryV1) {
            return self.validate_directory();
        }
        if let Some(bytes) = &self.intent {
            use super::super::transaction::native::descriptor::{self, Body};
            let decoded = descriptor::decode(bytes).map_err(|_| StoreError::Corrupt)?;
            if !matches!(decoded.body, Body::Intent(_))
                || decoded.header.connection != self.connection
                || decoded.header.operation != self.serial
                || decoded.header.attempt != u64::from(self.attempt)
                || decoded.header.nonce != self.nonce
                || decoded.header.admission != self.admission
                || decoded.header.baseline != self.baseline
                || decoded.header.fence != self.fence
            {
                return Err(StoreError::Corrupt);
            }
            if let Some(stage) = &self.stage {
                let stage = descriptor::decode(stage).map_err(|_| StoreError::Corrupt)?;
                if !matches!(stage.body, Body::Stage(_)) {
                    return Err(StoreError::Corrupt);
                }
                stage
                    .joined(&decoded, None, None)
                    .map_err(|_| StoreError::Corrupt)?;
            }
            let parse = |value: &Option<Vec<u8>>| {
                value
                    .as_ref()
                    .map(|bytes| descriptor::decode(bytes).map_err(|_| StoreError::Corrupt))
                    .transpose()
            };
            if let Some(outcome) = parse(&self.outcome)? {
                if !matches!(outcome.body, Body::Outcome(_)) {
                    return Err(StoreError::Corrupt);
                }
                outcome
                    .joined(
                        &decoded,
                        parse(&self.stage)?.as_ref(),
                        parse(&self.outcome_displaced)?.as_ref(),
                    )
                    .map_err(|_| StoreError::Corrupt)?;
            } else if self.outcome_displaced.is_some() {
                return Err(StoreError::Corrupt);
            }
        } else if self.stage.is_some() || self.outcome.is_some() || self.outcome_displaced.is_some()
        {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
}
impl Operation {
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join descriptor operation correlation to the canonical operation serial."
    )]
    fn validate_directory_identity(&self) -> Result<(), StoreError> {
        use super::super::transaction::native::descriptor::directory::{
            Admission, HistoricalAdmission,
        };
        let bytes = self.native_identity.as_deref().ok_or(StoreError::Corrupt)?;
        if self.baseline.is_none() && self.directory_chain_ref.is_some() {
            let selection = super::chain::first_selection(self)?;
            if selection.baseline.is_some() {
                return Err(StoreError::Corrupt);
            }
        } else {
            Admission::decode(bytes).map_err(|_| StoreError::Corrupt)?;
        }
        let fact = HistoricalAdmission::decode(bytes).map_err(|_| StoreError::Corrupt)?;
        let endpoint = |v: &storage::Identity| {
            let mut bytes = [0; 64];
            bytes[..32].copy_from_slice(&v.volume);
            bytes[32..].copy_from_slice(&v.root);
            bytes
        };
        if self.native_budget.is_none()
            || self.action.is_some()
            || self.initial_review.is_some()
            || self.selected_applied.is_some()
            || self.native_capture.is_some()
            || !self.native_capture_displaced.is_empty()
            || !self.recovery.is_empty()
            || fact.connection != self.connection
            || fact.operation != self.serial
            || fact.revision != self.expected
            || fact.baseline != self.baseline
            || fact.decision != self.decision
            || fact.path != self.path.to_relative()?
            || fact.endpoints
                != [
                    endpoint(&self.source_binding),
                    endpoint(&self.destination_binding),
                ]
            || fact.states[0].as_ref().map(|_| State::Directory) != self.source
            || fact.states[1].as_ref().map(|_| State::Directory) != self.destination
        {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
}
impl NativeRegistration {
    fn validate_directory(&self) -> Result<(), StoreError> {
        self.validate_directory_mode(false)
    }
    fn validate_directory_mode(&self, historical: bool) -> Result<(), StoreError> {
        use super::super::transaction::native::descriptor::directory::{self, Body};
        let parse = |bytes: &[u8]| {
            if historical {
                directory::decode_history(bytes)
            } else {
                directory::decode(bytes)
            }
        };
        if self.outcome_displaced.is_some() {
            return Err(StoreError::Corrupt);
        }
        let Some(bytes) = &self.intent else {
            return if self.stage.is_none() && self.outcome.is_none() {
                Ok(())
            } else {
                Err(StoreError::Corrupt)
            };
        };
        let intent = parse(bytes).map_err(|_| StoreError::Corrupt)?;
        let Body::Intent(body) = &intent.body else {
            return Err(StoreError::Corrupt);
        };
        if intent.header.connection != self.connection
            || intent.header.operation != self.serial
            || intent.header.attempt != u64::from(self.attempt)
            || intent.header.nonce != self.nonce
            || intent.header.admission != self.admission
            || intent.header.baseline != self.baseline
            || intent.header.fence != self.fence
            || self.side != 1 - body.direction
        {
            return Err(StoreError::Corrupt);
        }
        if let Some(bytes) = &self.stage {
            let stage = parse(bytes).map_err(|_| StoreError::Corrupt)?;
            stage.joined(&intent).map_err(|_| StoreError::Corrupt)?;
            if let Some(bytes) = &self.outcome {
                parse(bytes)
                    .and_then(|outcome| outcome.joined_outcome(&intent, &stage))
                    .map_err(|_| StoreError::Corrupt)?;
            }
        } else if self.outcome.is_some() {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
}
/// Durable exact originating object facts. This row cannot recreate a guard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeRecoveryFact {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured: Option<Vec<u8>>,
    pub connection: [u8; 16],
    pub serial: u64,
    pub slot: u32,
    pub attempt: u32,
    pub fact: Vec<u8>,
    pub displaced: Vec<u8>,
}
impl NativeRecoveryFact {
    pub fn key(&self) -> Vec<u8> {
        let mut key = key(6, self.connection, self.serial);
        key.extend(self.slot.to_be_bytes());
        key
    }
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
    )]
    pub fn validate(&self) -> Result<(), StoreError> {
        use super::super::transaction::native::descriptor as d;
        let fact = d::RecoveryFact::decode(&self.fact).map_err(|_| StoreError::Corrupt)?;
        if let Some(bytes) = &self.captured {
            let capture = d::ConflictSnapshot::decode(bytes).map_err(|_| StoreError::Corrupt)?;
            if capture.connection != self.connection
                || capture.operation != self.serial
                || fact.recovery.connection != self.connection
                || fact.recovery.operation != self.serial
                || fact.recovery.slot != self.slot
                || fact.intent != capture.binding_digest().map_err(|_| StoreError::Corrupt)?
                || Some(&fact.endpoint) != capture.endpoints.get(usize::from(fact.side))
            {
                return Err(StoreError::Corrupt);
            }
            let version = capture
                .versions
                .iter()
                .find(|version| {
                    version.side == fact.side && version.role.checked_add(1) == Some(fact.role)
                })
                .ok_or(StoreError::Corrupt)?;
            if !matches!(&version.state, d::State::Regular { object, content, .. } if object == &fact.object && content == &fact.content)
            {
                return Err(StoreError::Corrupt);
            }
            if let Some(digest) = version.displaced {
                let displaced = d::decode(&self.displaced).map_err(|_| StoreError::Corrupt)?;
                if displaced.digest != digest
                    || displaced.header.connection != self.connection
                    || displaced.header.operation != self.serial
                    || displaced.header.attempt != u64::from(self.attempt)
                    || !matches!(&displaced.body, d::Body::Displaced(body) if body.object == fact.object && body.side == fact.side && body.barriers == 3)
                {
                    return Err(StoreError::Corrupt);
                }
            } else if !self.displaced.is_empty() || self.attempt != 0 {
                return Err(StoreError::Corrupt);
            }
            return Ok(());
        }
        let displaced = d::decode(&self.displaced).map_err(|_| StoreError::Corrupt)?;
        if fact.recovery.connection != self.connection
            || fact.recovery.operation != self.serial
            || fact.recovery.slot != self.slot
            || displaced.header.connection != self.connection
            || displaced.header.operation != self.serial
            || displaced.header.attempt != u64::from(self.attempt)
            || !matches!(displaced.body, d::Body::Displaced(_))
        {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewKey {
    pub connection: [u8; 16],
    pub review: [u8; 32],
    pub origin: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum PageRef {
    Empty {},
    Page { hash: [u8; 32] },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum BaselineVersion {
    Absent {},
    Present {
        revision: u64,
        root: PageRef,
        count: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum LineageRef {
    Absent {},
    Present {
        origin: [u8; 32],
        index: u32,
        head: [u8; 32],
        version: BaselineVersion,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewCut {
    pub progress_revision: u64,
    pub schedule_revision: u64,
    pub control_sequence: u64,
    pub current_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ReviewPatch<T> {
    Absent {},
    Present { value: T },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ReviewAbandonCut {
    Member {
        operation: ChainOperationIdV1,
    },
    BetweenMembers {
        next_index: u8,
        predecessor: ChainOperationIdV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum CommitReceiptV1 {
    EqualityVerified {},
    Regular { attempt: u32, outcome: [u8; 32] },
    Directory { attempt: u32, outcome: [u8; 32] },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommitWitness {
    pub operation: ChainOperationIdV1,
    pub precommit_revision: u64,
    pub result_revision: u64,
    pub intent_digest: [u8; 32],
    pub admission_digest: [u8; 32],
    pub budget: NativeBudget,
    pub receipt: CommitReceiptV1,
    pub before: BaselineVersion,
    pub after: BaselineVersion,
    pub existing_ancestors: ReviewPatch<ExistingAncestorPatchV1>,
    pub directory_ancestors: ReviewPatch<DirectoryAncestorPatchV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ReviewedSelectionV1 {
    Chain {
        anchor: ChainOperationIdV1,
        digest: [u8; 32],
        index: u8,
        final_member: bool,
    },
    Equality {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ReviewedResultProvenanceV1 {
    ReviewStep {
        step: u32,
        entry: u32,
        entry_digest: [u8; 32],
        selected: ReviewedSelectionV1,
    },
    Continuation {
        link_digest: [u8; 32],
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewedBaselineResultV1 {
    pub version: u8,
    pub key: ReviewKey,
    pub provenance: ReviewedResultProvenanceV1,
    pub cut: ReviewCut,
    pub witness: CommitWitness,
    pub lineage: LineageRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ReviewArchiveRefV1 {
    Absent {},
    Present {
        anchor: ChainOperationIdV1,
        core_digest: [u8; 32],
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ReviewTerminalV1 {
    Completed {
        operation: ChainOperationIdV1,
        commit_digest: [u8; 32],
        archive: ReviewArchiveRefV1,
    },
    Abandoned {
        anchor: ChainOperationIdV1,
        core_digest: [u8; 32],
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialReviewStepCompletedV1 {
    pub version: u8,
    pub key: ReviewKey,
    pub step: u32,
    pub entry: u32,
    pub entry_digest: [u8; 32],
    pub first_operation: ChainOperationIdV1,
    pub terminal: ReviewTerminalV1,
    pub resulting_progress: u64,
    pub schedule_revision: u64,
    pub control_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewRemainingV1 {
    pub unselected: u32,
    pub structural: u32,
    pub conflict: u32,
    pub abandoned: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ReviewLastStepV1 {
    None {},
    Step { step: u32, digest: [u8; 32] },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialReviewProgressV1 {
    pub version: u8,
    pub key: ReviewKey,
    pub completed_steps: u32,
    pub covered: u32,
    pub remaining: ReviewRemainingV1,
    pub coverage_digest: [u8; 32],
    pub last_step: ReviewLastStepV1,
    pub progress_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InitialReviewCompletedV1 {
    pub version: u8,
    pub key: ReviewKey,
    pub completed_steps: u32,
    pub entries: u32,
    pub coverage_digest: [u8; 32],
    pub last_step_digest: [u8; 32],
    pub baseline: BaselineVersion,
    pub lineage: LineageRef,
    pub progress_revision: u64,
    pub schedule_revision: u64,
    pub control_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum PostReviewKindV1 {
    Ordinary {},
    SelectedRestore { action_digest: [u8; 32] },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PostReviewOperationV1 {
    pub version: u8,
    pub key: ReviewKey,
    pub completion_digest: [u8; 32],
    pub operation: ChainOperationIdV1,
    pub intent_digest: [u8; 32],
    pub admission_digest: [u8; 32],
    pub expected: u64,
    pub baseline: BaselineVersion,
    pub lineage: LineageRef,
    pub kind: PostReviewKindV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewResultCutV1 {
    pub progress_revision: u64,
    pub schedule_revision: u64,
    pub control_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ArchiveCreatedV1 {
    Baseline {
        operation: ChainOperationIdV1,
        witness_digest: [u8; 32],
    },
    Retirement {
        before: ReviewCut,
        inner: ReviewedInnerControlV1,
        result: ReviewResultCutV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArchiveCore {
    pub version: u8,
    pub key: ReviewKey,
    pub endpoints: [storage::Identity; 2],
    pub fence: u64,
    pub anchor: ChainOperationIdV1,
    pub digest: [u8; 32],
    pub link_digest: [u8; 32],
    pub retirement: DirectoryChainRetirementV1,
    pub baseline: BaselineVersion,
    pub created: ArchiveCreatedV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChainCompletionBaselineV1 {
    pub core: ArchiveCore,
    pub ordinal: u32,
    pub previous_set: [u8; 32],
    pub set: [u8; 32],
    pub lineage: LineageRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OriginArchiveV1 {
    pub anchor: ChainOperationIdV1,
    pub core_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChainBaselineLineageV1 {
    pub version: u8,
    pub key: ReviewKey,
    pub endpoints: [storage::Identity; 2],
    pub fence: u64,
    pub origin: [u8; 32],
    pub origin_version: BaselineVersion,
    pub origin_archive: OriginArchiveV1,
    pub head_index: u32,
    pub head_digest: [u8; 32],
    pub current_version: BaselineVersion,
    pub archives: u32,
    pub set: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChainBaselineSupersessionV1 {
    pub version: u8,
    pub key: ReviewKey,
    pub endpoints: [storage::Identity; 2],
    pub fence: u64,
    pub index: u32,
    pub previous: LineageRef,
    pub operation: ChainOperationIdV1,
    pub witness_digest: [u8; 32],
    pub admission_digest: [u8; 32],
    pub receipt_digest: [u8; 32],
    pub result: BaselineVersion,
    pub archives_before: u32,
    pub set_before: [u8; 32],
    pub archives_after: u32,
    pub set_after: [u8; 32],
    pub head: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewedCandidateRootsV1 {
    pub version: u8,
    pub connection: [u8; 16],
    pub expected: u64,
    pub checkpoint: PageRef,
    pub review: PageRef,
    pub before: BaselineVersion,
    pub after: BaselineVersion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum CoverageEffectV1 {
    Member {
        index: u8,
    },
    ExistingAncestor {
        prefix: u8,
        receipt_digest: [u8; 32],
    },
    Equality {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum CoverageDecisionV1 {
    Unselected {},
    Structural {},
    Conflict {},
    Abandoned {
        step: u32,
        core_digest: [u8; 32],
    },
    Committed {
        operation: ChainOperationIdV1,
        commit_digest: [u8; 32],
        effect: CoverageEffectV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CoverageEntryV1 {
    pub ordinal: u32,
    pub entry_digest: [u8; 32],
    pub decision: CoverageDecisionV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum ReviewedBaselineProvenanceV1 {
    ReviewedStep {
        link_digest: [u8; 32],
        operation: ChainOperationIdV1,
        step: u32,
        entry: u32,
        expected_progress: u64,
        expected_schedule: u64,
        control_sequence: u64,
        lineage: LineageRef,
    },
    Continuation {
        provenance_digest: [u8; 32],
        operation: ChainOperationIdV1,
        expected_progress: u64,
        expected_schedule: u64,
        control_sequence: u64,
        lineage: LineageRef,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewedBaselineBodyV1 {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub directory_chain: Option<DirectoryChainCommitV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub directory_ancestors: Option<DirectoryAncestorPatchV1>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "existing_ancestor_receipt"
    )]
    pub existing_ancestors: Option<ExistingAncestorPatchV1>,
    pub connection: [u8; 16],
    pub expected: u64,
    pub root: Option<[u8; 32]>,
    pub count: u32,
    pub source: storage::Identity,
    pub destination: storage::Identity,
    pub initial_review: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "table", content = "value", deny_unknown_fields)]
pub(super) enum Row {
    ReviewedBaselineResultV1(ReviewedBaselineResultV1),
    InitialReviewStepCompletedV1(InitialReviewStepCompletedV1),
    InitialReviewProgressV1(InitialReviewProgressV1),
    InitialReviewCompletedV1(InitialReviewCompletedV1),
    PostReviewOperationV1(PostReviewOperationV1),
    ChainCompletionBaselineV1(ChainCompletionBaselineV1),
    ChainBaselineLineageV1(ChainBaselineLineageV1),
    ChainBaselineSupersessionV1(ChainBaselineSupersessionV1),
    ReviewedCandidateRootsV1(ReviewedCandidateRootsV1),
    InitialChainReview(InitialChainReviewV1),
    InitialChainReviewEntry(InitialChainReviewEntryV1),
    InitialReviewCursor(InitialReviewCursorV1),
    InitialReviewLink(InitialReviewLinkV1),
    InitialReviewStop(InitialReviewStopBoundaryV1),
    Connection(Connection),
    Operation(Operation),
    Recovery(Recovery),
    Conflict(ConflictData),
    Baseline(BaselineEntry),
    NativeRegistration(NativeRegistration),
    NativeRecoveryFact(NativeRecoveryFact),
}
impl Row {
    pub fn key(&self) -> Vec<u8> {
        match self {
            Self::ReviewedBaselineResultV1(r) => {
                key(12, r.key.connection, r.witness.operation.serial)
            }
            Self::InitialReviewStepCompletedV1(r) => {
                let mut k = key(13, r.key.connection, r.key.origin);
                k.extend(r.step.to_be_bytes());
                k
            }
            Self::InitialReviewProgressV1(r) => key(14, r.key.connection, r.key.origin),
            Self::InitialReviewCompletedV1(r) => key(15, r.key.connection, r.key.origin),
            Self::PostReviewOperationV1(r) => key(16, r.key.connection, r.operation.serial),
            Self::ChainCompletionBaselineV1(r) => {
                key(17, r.core.key.connection, r.core.anchor.serial)
            }
            Self::ChainBaselineLineageV1(r) => key(18, r.key.connection, r.key.origin),
            Self::ChainBaselineSupersessionV1(r) => key(19, r.key.connection, u64::from(r.index)),
            Self::ReviewedCandidateRootsV1(r) => key(20, r.connection, r.expected),
            Self::InitialChainReview(r) => key(7, r.connection, r.original_revision),
            Self::InitialChainReviewEntry(r) => {
                let mut k = key(8, r.connection, r.origin);
                k.extend(r.ordinal.to_be_bytes());
                k
            }
            Self::InitialReviewCursor(r) => key(9, r.connection, r.origin),
            Self::InitialReviewLink(r) => key(10, r.connection, r.operation.serial),
            Self::InitialReviewStop(r) => key(11, r.fact().connection, r.fact().operation.serial),
            Self::NativeRegistration(n) => n.key(),
            Self::NativeRecoveryFact(n) => n.key(),
            Self::Connection(c) => key(0, c.id, 0),
            Self::Operation(o) => key(1, o.connection, o.serial),
            Self::Recovery(r) => r.key(),
            Self::Conflict(c) => key(3, c.connection, c.serial),
            Self::Baseline(b) => {
                let mut k = key(4, b.connection, b.revision);
                k.extend(b.ordinal.to_be_bytes());
                k
            }
        }
    }
    pub fn validate(&self) -> Result<(), StoreError> {
        match self {
            Self::ReviewedBaselineResultV1(r) => r.validate()?,
            Self::InitialReviewStepCompletedV1(r) => r.validate()?,
            Self::InitialReviewProgressV1(r) => r.validate()?,
            Self::InitialReviewCompletedV1(r) => r.validate()?,
            Self::PostReviewOperationV1(r) => r.validate()?,
            Self::ChainCompletionBaselineV1(r) => r.validate()?,
            Self::ChainBaselineLineageV1(r) => r.validate()?,
            Self::ChainBaselineSupersessionV1(r) => r.validate()?,
            Self::ReviewedCandidateRootsV1(r) => r.validate()?,
            Self::InitialChainReview(r) => r.validate()?,
            Self::InitialChainReviewEntry(r) => r.validate()?,
            Self::InitialReviewCursor(r) => r.validate()?,
            Self::InitialReviewLink(r) => r.validate()?,
            Self::InitialReviewStop(r) => r.validate()?,
            Self::NativeRegistration(n) => {
                n.validate_history_candidate()?;
            }
            Self::NativeRecoveryFact(n) => {
                n.validate()?;
            }
            Self::Connection(c) => {
                c.decode()?;
            }
            Self::Operation(o) => {
                o.decode()?;
            }
            Self::Recovery(r) => {
                r.decode()?;
            }
            Self::Conflict(c) => {
                c.decode()?;
            }
            Self::Baseline(b) => {
                b.path.to_relative()?;
                if b.revision == 0 || b.ordinal >= 4096 {
                    return Err(StoreError::Corrupt);
                }
            }
        }
        Ok(())
    }
}
pub(super) fn key(table: u8, connection: [u8; 16], serial: u64) -> Vec<u8> {
    let mut key = vec![table];
    key.extend(connection);
    key.extend(serial.to_be_bytes());
    key
}
/// Constant-size historical receipt; never reconstructs live authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::connection) struct ExistingAncestorPatchV1 {
    pub version: u8,
    pub admission: [u8; 32],
    pub prefixes: u64,
}
fn existing_ancestor_receipt<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ExistingAncestorPatchV1>, D::Error> {
    let receipt = ExistingAncestorPatchV1::deserialize(deserializer)?;
    if receipt.version != 1 || receipt.prefixes == 0 || receipt.prefixes & 1 != 0 {
        return Err(serde::de::Error::custom(
            "invalid existing-ancestor receipt",
        ));
    }
    Ok(Some(receipt))
}
/// Historical evidence of explicit empty review and a native verified commit.
/// This bounded fact is never a live endpoint or transaction capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::connection) struct InitialEmptyBaselineV1 {
    pub(super) version: u8,
    pub(super) connection: [u8; 16],
    pub(super) revision: u64,
    pub(super) source: storage::Identity,
    pub(super) destination: storage::Identity,
    pub(super) preview: [u8; 32],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
pub(super) enum Change {
    ReviewedBaselineCommit {
        body: ReviewedBaselineBodyV1,
        provenance: ReviewedBaselineProvenanceV1,
    },
    ReviewedContinuationIntent {
        operation: Operation,
        provenance: PostReviewOperationV1,
        expected_progress: u64,
        expected_schedule: u64,
        control_sequence: u64,
    },
    ReviewedChainRetirement {
        key: ReviewKey,
        before: ReviewCut,
        inner: ReviewedInnerControlV1,
        baseline: BaselineVersion,
        lineage: LineageRef,
    },
    InitialReviewAccepted {
        review: InitialChainReviewV1,
        cursor: InitialReviewCursorV1,
    },
    ReviewedIntent {
        operation: Operation,
        link: InitialReviewLinkV1,
        expected_progress: u64,
        expected_schedule: u64,
        control_sequence: u64,
    },
    ReviewedChainControl {
        control: InitialReviewControlV1,
        inner: ReviewedInnerControlV1,
    },
    InitialReviewControl {
        control: InitialReviewControlV1,
    },
    ReviewedOrdinaryStopBoundary {
        boundary: InitialReviewStopBoundaryV1,
    },
    InitialEmptyBaseline {
        fact: InitialEmptyBaselineV1,
    },
    DirectoryChainStopBoundary {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        expected_schedule: u64,
        fence: u64,
        kind: ChainStopKind,
        evidence: DirectoryChainStopEvidenceV1,
    },
    DirectoryChainControl {
        connection: [u8; 16],
        anchor: ChainOperationIdV1,
        digest: [u8; 32],
        expected_progress: u64,
        expected_schedule: u64,
        control_sequence: u64,
        transition: DirectoryChainControlTransitionV1,
    },
    SelectedCaptureDrain {
        entry: Recovery,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        fact: Option<NativeRecoveryFact>,
    },
    SelectedUntouched {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        operation_expected: u64,
        fence: u64,
    },
    SelectedApplied {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        fence: u64,
        attempt: u32,
        outcome: [u8; 32],
    },
    SelectedAppliedCarryV1 {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        fence: u64,
        attempt: u32,
        outcome: [u8; 32],
        #[serde(deserialize_with = "selected_attention_carry")]
        carry: SelectedAttentionCarryV1,
    },
    NativeCaptureDrain {
        entry: Recovery,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        fact: Option<NativeRecoveryFact>,
    },
    NativeConflictComplete {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        fence: u64,
        conflict_expected: u64,
        capture: [u8; 32],
    },
    NativeSelectedCollision {
        snapshot: Vec<u8>,
        expected: u64,
    },
    SelectedCollisionComplete {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        fence: u64,
        conflict_expected: u64,
        capture: [u8; 32],
    },
    NativeCapture {
        snapshot: Vec<u8>,
        displaced: Vec<Vec<u8>>,
        expected: u64,
        kind: u8,
    },
    NativeDrain {
        entry: Recovery,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        source: BoundaryEvidence,
        fact: Option<NativeRecoveryFact>,
    },
    SelectedDrain {
        entry: Recovery,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        source: BoundaryEvidence,
        fact: Option<NativeRecoveryFact>,
    },
    DirectoryBoundary {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        fact: DirectoryBoundaryFact,
    },
    NativeBoundary {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        kind: u8,
        #[serde(default)]
        evidence: Option<BoundaryEvidence>,
    },
    SelectedBoundary {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        operation_expected: u64,
        fence: u64,
        kind: u8,
        evidence: BoundaryEvidence,
    },
    NativeOutcome {
        registration: NativeRegistration,
        expected: u64,
        verified: bool,
        #[serde(default)]
        reconstructed: bool,
    },
    IncompleteRecovery {
        connection: [u8; 16],
        serial: u64,
        slot: u32,
        expected: u64,
        fence: u64,
    },
    ReadReserve {
        #[serde(default)]
        recovery_only: bool,
        #[serde(default)]
        operation_expected: Option<u64>,
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        fence: u64,
        previous: NativeBudget,
        next: NativeBudget,
    },
    NativeBound {
        fact: NativeRecoveryFact,
        expected: u64,
        fence: u64,
    },
    NativeReserve {
        registration: NativeRegistration,
        expected: u64,
    },
    NativePrepared {
        registration: NativeRegistration,
        expected: u64,
    },
    Create {
        record: Connection,
    },
    Intent {
        operation: Operation,
    },
    Fence {
        connection: [u8; 16],
        expected: u64,
        generation: u64,
    },
    Disconnect {
        connection: [u8; 16],
        expected: u64,
        generation: u64,
    },
    Reserve {
        entry: Recovery,
        expected: u64,
        fence: u64,
    },
    Bytes {
        entry: Recovery,
        expected: u64,
        fence: u64,
    },
    Published {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        source: storage::Identity,
        destination: storage::Identity,
    },
    Verified {
        connection: [u8; 16],
        serial: u64,
        expected: u64,
        source: storage::Identity,
        destination: storage::Identity,
    },
    BaselineCommit {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        directory_chain: Option<DirectoryChainCommitV1>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        directory_ancestors: Option<DirectoryAncestorPatchV1>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "existing_ancestor_receipt"
        )]
        existing_ancestors: Option<ExistingAncestorPatchV1>,
        connection: [u8; 16],
        expected: u64,
        root: Option<[u8; 32]>,
        count: u32,
        source: storage::Identity,
        destination: storage::Identity,
        initial_review: bool,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Event {
    pub schema: u32,
    pub predecessor: [u8; 32],
    pub change: Change,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LogHeader {
    pub schema: u32,
    pub identity: storage::Identity,
    pub generation: u64,
    pub first_sequence: u64,
    pub predecessor: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Child {
    pub hash: [u8; 32],
    pub first: Vec<u8>,
    pub last: Vec<u8>,
    pub count: u64,
    pub level: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "node", content = "rows", deny_unknown_fields)]
pub(super) enum Node {
    Leaf(Vec<Row>),
    Index(Vec<Child>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageNode {
    pub schema: u32,
    pub node: Node,
}
impl PageNode {
    pub fn reference(&self, hash: [u8; 32]) -> Result<Child, StoreError> {
        if self.schema != 1 {
            return Err(StoreError::Corrupt);
        }
        match &self.node {
            Node::Leaf(rows) => {
                if rows.is_empty() || rows.len() > 128 {
                    return Err(StoreError::Corrupt);
                }
                let mut previous = None;
                for row in rows {
                    row.validate()?;
                    let key = row.key();
                    if previous.as_ref().is_some_and(|p| p >= &key) {
                        return Err(StoreError::Corrupt);
                    }
                    previous = Some(key);
                }
                Ok(Child {
                    hash,
                    first: rows.first().ok_or(StoreError::Corrupt)?.key(),
                    last: rows.last().ok_or(StoreError::Corrupt)?.key(),
                    count: rows.len() as u64,
                    level: 0,
                })
            }
            Node::Index(children) => {
                if children.is_empty() || children.len() > 32 {
                    return Err(StoreError::Corrupt);
                }
                let first = children.first().ok_or(StoreError::Corrupt)?;
                let last = children.last().ok_or(StoreError::Corrupt)?;
                if first.level >= 7 {
                    return Err(StoreError::LimitExceeded);
                }
                let mut previous = None;
                let mut count = 0_u64;
                for child in children {
                    if child.level != first.level
                        || !(25..=29).contains(&child.first.len())
                        || !(25..=29).contains(&child.last.len())
                        || child.first > child.last
                        || child.count == 0
                        || previous.is_some_and(|p| p >= &child.first)
                    {
                        return Err(StoreError::Corrupt);
                    }
                    count = count
                        .checked_add(child.count)
                        .ok_or(StoreError::LimitExceeded)?;
                    previous = Some(&child.last);
                }
                Ok(Child {
                    hash,
                    first: first.first.clone(),
                    last: last.last.clone(),
                    count,
                    level: first.level + 1,
                })
            }
        }
    }
}
pub(super) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
pub(super) fn hex(bytes: &[u8]) -> String {
    fn digit(value: u8) -> char {
        char::from(if value < 10 {
            b'0' + value
        } else {
            b'a' + value - 10
        })
    }
    bytes
        .iter()
        .flat_map(|byte| [digit(byte >> 4), digit(byte & 15)])
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct WindowsFileId {
    pub volume_serial: u64,
    pub file_id: [u8; 16],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileIdentity {
    Unix(u64),
    Windows(WindowsFileId),
}
impl FileIdentity {
    pub fn require_platform(self) -> Result<Self, StoreError> {
        if matches!(self, Self::Windows(_)) != cfg!(windows) {
            return Err(StoreError::IdentityChanged);
        }
        Ok(self)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ObjectIdentity {
    pub root: storage::Identity,
    pub file_id: FileIdentity,
}
impl Serialize for ObjectIdentity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ObjectIdentity", 2)?;
        state.serialize_field("root", &self.root)?;
        match self.file_id {
            FileIdentity::Unix(inode) => state.serialize_field("inode", &inode)?,
            FileIdentity::Windows(id) => state.serialize_field("windows_file_id_v1", &id)?,
        }
        state.end()
    }
}
// Missing fields default to None; an explicitly present null is invalid.
pub(super) fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}
impl<'de> Deserialize<'de> for ObjectIdentity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            root: storage::Identity,
            #[serde(default, deserialize_with = "present")]
            inode: Option<u64>,
            #[serde(default, deserialize_with = "present")]
            windows_file_id_v1: Option<WindowsFileId>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let file_id = match (wire.inode, wire.windows_file_id_v1) {
            (Some(inode), None) => FileIdentity::Unix(inode),
            (None, Some(id)) => FileIdentity::Windows(id),
            _ => {
                return Err(serde::de::Error::custom(
                    "exactly one native identity required",
                ));
            }
        };
        Ok(Self {
            root: wire.root,
            file_id,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CapturedSlot {
    pub snapshot: Vec<u8>,
    pub version: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Recovery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured: Option<CapturedSlot>,
    pub connection: [u8; 16],
    pub serial: u64,
    pub slot: u32,
    pub side: u8,
    pub path: NativePath,
    pub reason: u8,
    pub state: u8,
    pub content: Option<State>,
    pub object: Option<ObjectIdentity>,
    pub retained: u64,
}
impl Recovery {
    pub fn id(&self) -> RecoveryId {
        RecoveryId {
            operation: OperationId {
                connection: ConnectionId(self.connection),
                serial: self.serial,
            },
            slot: self.slot,
        }
    }
    pub fn key(&self) -> Vec<u8> {
        let mut value = key(2, self.connection, self.serial);
        value.extend(self.slot.to_be_bytes());
        value
    }
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
    )]
    pub fn decode(&self) -> Result<RecoveryEntry, StoreError> {
        if let Some(origin) = &self.captured {
            use super::super::transaction::native::descriptor as d;
            let snapshot =
                d::ConflictSnapshot::decode(&origin.snapshot).map_err(|_| StoreError::Corrupt)?;
            let version = snapshot
                .versions
                .get(usize::from(origin.version))
                .ok_or(StoreError::Corrupt)?;
            if snapshot.connection != self.connection
                || snapshot.operation != self.serial
                || snapshot.path != self.path.to_relative()?
                || version.side != self.side
                || self.reason != 2
                || !matches!(version.state, d::State::Regular { .. })
            {
                return Err(StoreError::Corrupt);
            }
        }
        if let Some(object) = &self.object {
            object.file_id.require_platform()?;
        }
        let original_side = match self.side {
            0 => Side::Source,
            1 => Side::Destination,
            _ => return Err(StoreError::Corrupt),
        };
        let reason = match self.reason {
            0 => RecoveryReason::NormalDeletion,
            1 => RecoveryReason::DisplacedVersion,
            2 => RecoveryReason::Conflict(ConflictId {
                connection: ConnectionId(self.connection),
                serial: self.serial,
            }),
            _ => return Err(StoreError::Corrupt),
        };
        let state = match self.state {
            0 => RecoveryState::Reserved,
            1 => RecoveryState::BytesVerified,
            2 => RecoveryState::Attached,
            3 => RecoveryState::PurgeIntent,
            4 => RecoveryState::Purged,
            5 => RecoveryState::RetainedIncomplete,
            _ => return Err(StoreError::Corrupt),
        };
        if self.serial == 0
            || self.slot >= 128
            || (matches!(self.state, 0 | 5) && (self.content.is_some() || self.object.is_some()))
            || (!matches!(self.state, 0 | 5)
                && (!matches!(self.content, Some(State::File { .. })) || self.object.is_none()))
            || (matches!(self.state, 0 | 1 | 5) && self.retained != 0)
        {
            return Err(StoreError::Corrupt);
        }
        Ok(RecoveryEntry {
            id: self.id(),
            original_side,
            original_path: self.path.to_relative()?,
            state,
            reason,
            content: self.content.clone().map(Into::into),
            retained_seconds: self.retained,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BaselineEntry {
    pub connection: [u8; 16],
    pub revision: u64,
    pub ordinal: u32,
    pub path: NativePath,
    pub state: State,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConflictData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_capture: Option<Vec<u8>>,
    pub connection: [u8; 16],
    pub serial: u64,
    pub revision: u64,
    pub kind: u8,
    pub path: NativePath,
    pub source: Option<State>,
    pub destination: Option<State>,
    pub preserved: Vec<u32>,
    pub resolved: bool,
}
impl ConflictData {
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
    )]
    pub fn decode(&self) -> Result<ConflictRecord, StoreError> {
        if self.serial == 0 || self.revision == 0 || self.preserved.len() > 128 {
            return Err(StoreError::Corrupt);
        }
        let id = ConflictId {
            connection: ConnectionId(self.connection),
            serial: self.serial,
        };
        let captured = self
            .native_capture
            .as_deref()
            .map(super::super::transaction::native::descriptor::ConflictSnapshot::decode)
            .transpose()
            .map_err(|_| StoreError::Corrupt)?;
        if captured.as_ref().is_some_and(|value| {
            value.connection != self.connection
                || value.operation != self.serial
                || Some(&value.path) != self.path.to_relative().ok().as_ref()
                || value.revision >= self.revision
        }) {
            return Err(StoreError::Corrupt);
        }
        Ok(ConflictRecord {
            captured,
            id,
            revision: self.revision,
            kind: conflict_from(self.kind)?,
            path: self.path.to_relative()?,
            source: self.source.clone().map(Into::into),
            destination: self.destination.clone().map(Into::into),
            preserved: self
                .preserved
                .iter()
                .map(|slot| RecoveryId {
                    operation: OperationId {
                        connection: id.connection,
                        serial: id.serial,
                    },
                    slot: *slot,
                })
                .collect(),
            resolved: self.resolved,
        })
    }
}

impl From<RelativePath> for NativePath {
    fn from(path: RelativePath) -> Self {
        Self::relative(&path)
    }
}

#[cfg(test)]
mod native_identity_tests {
    use super::*;
    fn root() -> storage::Identity {
        storage::Identity {
            volume: [1; 32],
            root: [2; 32],
        }
    }
    #[test]
    fn windows_identity_wire_retains_upper_half_and_serial() {
        let prefix = serde_json::to_string(&root()).unwrap();
        let a = format!(
            r#"{{"root":{prefix},"windows_file_id_v1":{{"volume_serial":7,"file_id":[0,0,0,0,0,0,0,0,1,0,0,0,0,0,0,0]}}}}"#
        );
        let b = a.replace("0,0,1,0", "0,0,2,0");
        let decoded: ObjectIdentity =
            serde_json::from_str(&a).expect("full Windows identity shape");
        assert_eq!(serde_json::to_string(&decoded).unwrap(), a);
        assert_ne!(decoded, serde_json::from_str::<ObjectIdentity>(&b).unwrap());
        assert_ne!(
            decoded,
            serde_json::from_str::<ObjectIdentity>(&a.replace("serial\":7", "serial\":8")).unwrap()
        );
    }
    #[test]
    fn unix_identity_encoding_stays_exact_and_ambiguous_shapes_refuse() {
        let prefix = serde_json::to_string(&root()).unwrap();
        let legacy = format!(r#"{{"root":{prefix},"inode":42}}"#);
        let decoded: ObjectIdentity = serde_json::from_str(&legacy).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), legacy);
        for bad in [legacy.replace("42}", "42,\"inode\":43}"), legacy.replace("42}", "42,\"unknown\":0}"), legacy.replace("42}", "42,\"windows_file_id_v1\":null}"), legacy.replace("42}", "42,\"windows_file_id_v1\":{\"volume_serial\":7,\"file_id\":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}}") ] {
            assert!(serde_json::from_str::<ObjectIdentity>(&bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn native_identity_requires_matching_platform_and_bounded_payload() {
        let root = serde_json::to_string(&root()).unwrap();
        let unix = format!(r#"{{"root":{root},"inode":9}}"#);
        let windows = format!(
            r#"{{"root":{root},"windows_file_id_v1":{{"volume_serial":7,"file_id":[0,0,0,0,0,0,0,0,1,0,0,0,0,0,0,0]}}}}"#
        );
        let unix: ObjectIdentity = serde_json::from_str(&unix).unwrap();
        let win: ObjectIdentity = serde_json::from_str(&windows).unwrap();
        assert_eq!(unix.file_id.require_platform().is_ok(), !cfg!(windows));
        assert_eq!(win.file_id.require_platform().is_ok(), cfg!(windows));
        assert!(encode(&win).unwrap().len() < 4096);
        for bad in [
            windows.replace("7,", "7,\"volume_serial\":8,"),
            windows.replace("7,", "7,\"unknown\":8,"),
            windows.replace(
                "\"windows_file_id_v1\":",
                "\"inode\":null,\"windows_file_id_v1\":",
            ),
            windows.replace("[0,0,0,0,0,0,0,0,1,0,0,0,0,0,0,0]", "[0,0]"),
        ] {
            assert!(
                serde_json::from_str::<ObjectIdentity>(&bad).is_err(),
                "{bad}"
            );
        }
    }
}

// serde's internally tagged unit visitor discards unknown keys. Decode through
// empty struct variants so every new chain enum is strict without changing its
// public-in-module unit variants or serialized spelling. Legacy enums retain
// their existing grammar.
macro_rules! strict_chain_units {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                #[serde(tag = "kind", deny_unknown_fields)]
                enum Wire { $($variant {}),+ }
                Ok(match Wire::deserialize(deserializer)? { $(Wire::$variant {} => Self::$variant),+ })
            }
        }
    };
}
strict_chain_units!(PauseOrCancel { Pause, Cancel });
strict_chain_units!(ChainRetirementReason {
    Completed,
    Abandoned
});
strict_chain_units!(ChainStopKind {
    NeverBegun,
    StagedOnly,
    PublishedAccounted
});
impl<'de> Deserialize<'de> for DirectoryChainScheduleState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", deny_unknown_fields)]
        enum Wire {
            Running {},
            Stopping { reason: PauseOrCancel },
            Stopped { reason: PauseOrCancel },
            Abandoning {},
            Disconnected {},
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Running {} => Self::Running,
            Wire::Stopping { reason } => Self::Stopping { reason },
            Wire::Stopped { reason } => Self::Stopped { reason },
            Wire::Abandoning {} => Self::Abandoning,
            Wire::Disconnected {} => Self::Disconnected,
        })
    }
}
impl<'de> Deserialize<'de> for DirectoryChainControlTransitionV1 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", deny_unknown_fields)]
        enum Wire {
            StopRequested { reason: PauseOrCancel },
            StopAcknowledged {},
            Resume {},
            AbandonRequested {},
            Retire {},
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::StopRequested { reason } => Self::StopRequested { reason },
            Wire::StopAcknowledged {} => Self::StopAcknowledged,
            Wire::Resume {} => Self::Resume,
            Wire::AbandonRequested {} => Self::AbandonRequested,
            Wire::Retire {} => Self::Retire,
        })
    }
}

fn strict_review_state<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<State, D::Error> {
    #[derive(Deserialize)]
    #[serde(tag = "kind", deny_unknown_fields)]
    enum Wire {
        Directory {},
        File {
            sha256: [u8; 32],
            bytes: u64,
            executable: bool,
        },
    }
    Ok(match Wire::deserialize(deserializer)? {
        Wire::Directory {} => State::Directory,
        Wire::File {
            sha256,
            bytes,
            executable,
        } => State::File {
            sha256,
            bytes,
            executable,
        },
    })
}

impl Row {
    /// Closed semantic page edges. Each durable fact exposes at most two roots;
    /// only a checked transient candidate singleton exposes four.
    pub(super) fn review_roots(&self) -> Result<Vec<[u8; 32]>, StoreError> {
        self.validate()?;
        let roots = match self {
            Self::Connection(c) => vec![c.baseline_root],
            Self::InitialChainReview(r) => vec![Some(r.root)],
            Self::ReviewedBaselineResultV1(r) => {
                vec![r.witness.before.root(), r.witness.after.root()]
            }
            Self::InitialReviewCompletedV1(r) => vec![r.baseline.root()],
            Self::PostReviewOperationV1(r) => vec![r.baseline.root()],
            Self::ChainCompletionBaselineV1(r) => vec![r.core.baseline.root()],
            Self::ChainBaselineLineageV1(r) => {
                vec![r.origin_version.root(), r.current_version.root()]
            }
            Self::ChainBaselineSupersessionV1(r) => vec![
                match &r.previous {
                    LineageRef::Absent {} => None,
                    LineageRef::Present { version, .. } => version.root(),
                },
                r.result.root(),
            ],
            Self::ReviewedCandidateRootsV1(r) => vec![
                r.checkpoint.root(),
                r.review.root(),
                r.before.root(),
                r.after.root(),
            ],
            _ => Vec::new(),
        };
        Ok(roots.into_iter().flatten().collect())
    }
}
impl Change {
    pub(super) fn review_roots(&self) -> Result<Vec<[u8; 32]>, StoreError> {
        let roots = match self {
            Self::BaselineCommit { root, .. } => vec![*root],
            Self::InitialReviewAccepted { review, .. } => vec![Some(review.root)],
            Self::ReviewedBaselineCommit { body, provenance } => {
                let lineage = match provenance {
                    ReviewedBaselineProvenanceV1::ReviewedStep { lineage, .. }
                    | ReviewedBaselineProvenanceV1::Continuation { lineage, .. } => lineage,
                };
                vec![
                    body.root,
                    match lineage {
                        LineageRef::Absent {} => None,
                        LineageRef::Present { version, .. } => version.root(),
                    },
                ]
            }
            Self::ReviewedContinuationIntent { provenance, .. } => vec![provenance.baseline.root()],
            Self::ReviewedChainRetirement { baseline, .. } => vec![baseline.root()],
            _ => Vec::new(),
        };
        Ok(roots.into_iter().flatten().collect())
    }
}
