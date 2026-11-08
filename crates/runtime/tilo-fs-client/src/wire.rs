//! The filesystem families' wire: one framed postcard value per call, reply
//! or release, carried on the same connected stream the capability was handed
//! over.
//!
//! A capability result crosses as an object id in the serving side's table.
//! Dropping the last client of an id releases it, which is what closes an open
//! file and releases a held native claim, so release is a frame of its own.
//!
//! A refusal under still-live scope is a reply without its payload (`None`),
//! never a transport error: only a peer that went away or a malformed frame is
//! an error, and then the whole connection is done.

use serde::{Deserialize, Serialize};

use crate::blobs::{BlobRef, ReadOutcome};
use crate::directory_types::{
    DirectoryFilesystemStats, DirectoryRows, DirectoryStat, DirectoryStreamPage,
    HistoryReadObservation, JobPoll, PendingReplacementObservation, RecoveryPage,
    ReplacementObservation,
};
use crate::{DirEntry, Metadata, OpenMode};

/// Largest frame either side accepts. One write carries at most a mebibyte;
/// the bound leaves room for the rest of a reply without ever being reached.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;

/// The root object every connection starts with.
pub const ROOT: u64 = 0;

#[derive(Debug, Serialize, Deserialize)]
pub enum Frame {
    Call {
        call: u64,
        object: u64,
        request: FsRequest,
    },
    Reply {
        call: u64,
        reply: Result<FsReply, String>,
    },
    /// The caller dropped the last handle to `object`.
    Release { object: u64 },
}

/// The native outcome of one operation on an entry. `Refused` means the
/// provider's current authority no longer covers it; the connection stays up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeEntryStatus {
    Ok,
    Refused,
}

/// An operation whose only content is its native outcome.
pub type Ack = NativeEntryStatus;

#[derive(Debug, Serialize, Deserialize)]
pub enum FsRequest {
    // WorkspaceFs
    Open {
        path: String,
        mode: OpenMode,
    },
    List {
        path: String,
    },
    Stat {
        path: String,
    },
    Mkdir {
        path: String,
    },
    Remove {
        path: String,
    },
    Directory,
    // FileHandle
    Read {
        offset: u64,
        max_len: u32,
    },
    Write {
        offset: u64,
        bytes: Vec<u8>,
    },
    Flush,
    HandleMetadata,
    Append {
        bytes: Vec<u8>,
    },
    CheckScope,
    // DirectoryHandle
    ListRows {
        cursor: u64,
    },
    OpenDirectory {
        name: Vec<u8>,
    },
    OpenFile {
        name: Vec<u8>,
        mode: OpenMode,
        expected: [u64; 2],
    },
    CreateFile {
        name: Vec<u8>,
    },
    CreateDirectory {
        name: Vec<u8>,
    },
    RenameNoReplace {
        name: Vec<u8>,
        new_name: Vec<u8>,
    },
    DirectoryMetadata,
    StatEntry {
        name: Vec<u8>,
    },
    PinEntry {
        name: Vec<u8>,
        expected: [u64; 2],
    },
    OpenCursor,
    Statfs,
    Readlink {
        name: Vec<u8>,
        expected: [u64; 2],
    },
    Recoveries {
        after: Vec<u8>,
    },
    RestoreCaptured {
        recovery: [u8; 16],
        name: Vec<u8>,
    },
    PrepareReplace {
        temporary: Vec<u8>,
        temporary_expected: [u64; 2],
        victim: Vec<u8>,
        victim_expected: [u64; 2],
    },
    SyncDirectory,
    RetireCreated {
        name: Vec<u8>,
        expected: [u64; 2],
    },
    SetCreatedPermissions {
        name: Vec<u8>,
        expected: [u64; 2],
        mode: u32,
    },
    RenameReplace {
        name: Vec<u8>,
        new_name: Vec<u8>,
    },
    AtomicSave {
        temporary: Vec<u8>,
        expected: [u64; 2],
        target: Vec<u8>,
    },
    HistoryReadSubject {
        name: Vec<u8>,
    },
    HistoryQuery {
        name: Vec<u8>,
        expected: [u64; 2],
        query: Vec<u8>,
    },
    // MetadataPin
    PinStat,
    PinCheckScope,
    // NativeJob
    JobPoll,
    JobCancel,
    JobCheckScope,
    JobPinReplacement,
    JobCreatedTemporary,
    // ReplacementPin
    ReplacementCommit {
        receipt: Vec<u8>,
    },
    ReplacementObserve,
    ReplacementCheck,
    // DirectoryCursor
    CursorPage {
        expected_sequence: u64,
    },
    // PendingReplacement
    PendingObserve,
    PendingStart {
        receipt: Vec<u8>,
    },
    PendingCancel,
    PendingCheckScope,
    PendingArm {
        challenge: [u8; 32],
    },
    PendingCommittedPin,
    PendingRetireHandoff {
        ticket: [u8; 16],
    },
    // CreatedTemporary
    TemporaryCheckScope,
    TemporaryRetire,
    TemporaryPrepareReplace,
    TemporarySetPermissions {
        mode: u32,
    },
    TemporaryPrepareAtomicSave,
    // HistoryReadSubject
    SubjectObserve,
    SubjectCheckScope,
    SubjectCheck,
    SubjectArm {
        challenge: [u8; 32],
    },
    SubjectRetireHandoff {
        ticket: [u8; 16],
    },
    // SessionBlobs
    SessionScope {
        workspace: String,
        app: String,
    },
    SessionRefreshPublic,
    SessionAttributeWrites {
        paths: Vec<String>,
        actor: String,
        label: String,
        color: [u8; 3],
        ttl_ms: u32,
    },
    // DocumentBlobs
    Put {
        bytes: Vec<u8>,
        mime: String,
    },
    Upload {
        mime: String,
    },
    Get {
        hash: String,
        offset: u64,
        max_len: u32,
    },
    Has {
        hash: String,
    },
    Size {
        hash: String,
    },
    PublicContent {
        name: String,
    },
    // PublicContent
    PublicKeep,
    PublicRelease,
    PublicRead {
        offset: u64,
        max_len: u32,
    },
    // BlobUpload
    UploadWrite {
        bytes: Vec<u8>,
    },
    UploadFinish,
}

/// A reply's content. Each request has one shape: the caller decodes the
/// variant it asked for and treats anything else as a lost capability. The
/// `Option` inside a payload variant is the native refusal under live scope.
#[derive(Debug, Serialize, Deserialize)]
pub enum FsReply {
    /// Done, with nothing to report.
    Empty,
    /// Done, with the native outcome alone.
    Ack(Ack),
    Handle(Option<u64>),
    Entries(Option<Vec<DirEntry>>),
    Metadata(Option<Metadata>),
    Bytes(Option<Vec<u8>>),
    Written(Option<u32>),
    Rows(Option<DirectoryRows>),
    Stat(Option<DirectoryStat>),
    /// Outer `None` is a refusal, `Some(None)` an absent entry.
    EntryStat(Option<Option<DirectoryStat>>),
    StreamPage(Option<DirectoryStreamPage>),
    FilesystemStats(Option<DirectoryFilesystemStats>),
    Recoveries(Option<RecoveryPage>),
    Poll(Option<JobPoll>),
    ReplacementObservation(ReplacementObservation),
    PendingObservation(PendingReplacementObservation),
    HistoryObservation(HistoryReadObservation),
    CommittedPin(Option<CommittedPin>),
    Ticket(Vec<u8>),
    OpenedFile(Option<OpenedFile>),
    Scope(u64),
    Refreshed(u32),
    Present(bool),
    Size(Option<u64>),
    Blob(BlobRef),
    BlobRead(ReadOutcome),
    PublicRead {
        read: ReadOutcome,
        hash: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenedFile {
    pub handle: u64,
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommittedPin {
    pub pin: u64,
    pub job: u64,
}
