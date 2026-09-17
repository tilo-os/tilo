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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Connection {
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Operation {
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
    pub fn decode(&self) -> Result<OperationReceipt, StoreError> {
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
            _ => return Err(StoreError::Corrupt),
        };
        if self.serial == 0
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
#[serde(tag = "table", content = "value", deny_unknown_fields)]
pub(super) enum Row {
    Connection(Connection),
    Operation(Operation),
    Recovery(Recovery),
    Conflict(ConflictData),
    Baseline(BaselineEntry),
}
impl Row {
    pub fn key(&self) -> Vec<u8> {
        match self {
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
pub(super) enum Change {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObjectIdentity {
    pub root: storage::Identity,
    pub inode: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Recovery {
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
    pub fn decode(&self) -> Result<RecoveryEntry, StoreError> {
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
            _ => return Err(StoreError::Corrupt),
        };
        if self.serial == 0
            || self.slot >= 128
            || (self.state == 0 && (self.content.is_some() || self.object.is_some()))
            || (self.state != 0
                && (!matches!(self.content, Some(State::File { .. })) || self.object.is_none()))
            || (self.state < 2 && self.retained != 0)
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
    pub fn decode(&self) -> Result<ConflictRecord, StoreError> {
        if self.serial == 0 || self.revision == 0 || self.preserved.len() > 128 {
            return Err(StoreError::Corrupt);
        }
        let id = ConflictId {
            connection: ConnectionId(self.connection),
            serial: self.serial,
        };
        Ok(ConflictRecord {
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
