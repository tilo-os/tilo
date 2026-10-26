use crate::{EntryName, Metadata};
use serde::{Deserialize, Serialize};

// Darwin's mode_t is u16; the wire represents mode bits as u32.
pub fn mode_bits(mode: impl Into<u32>) -> u32 {
    mode.into()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RecoveryKind {
    NeedsRevalidation,
    KnownStaged,
    UnknownNativeCommit,
    PublishedSourceRetained,
    /// A namespace-only capture retained in independently admitted root custody.
    QuarantinedNative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ItemState {
    Completed,
    Failed,
    Cancelled,
    CopiedSourceRetained,
    RecoveryRequired,
    OutcomeUnknown,
}

/// Observational inode identity, never an authority or grant constructor.
#[derive(Clone, Debug)]
pub struct DirectoryStat {
    pub metadata: Metadata,
    pub identity: [u64; 2],
    /// Observed native `S_IFMT` bits; never permissions or authority.
    pub file_type: u32,
    /// Original observed execute mode bits only (0111), never Execute authority.
    pub execute_bits: u16,
}
pub struct DirectoryRow {
    pub name: EntryName,
    pub metadata: Metadata,
}
pub struct DirectoryRows {
    pub entries: Vec<DirectoryRow>,
    pub next: u64,
}
/// Bounded journal observation; IDs and cursor bytes do not confer rights.
pub struct RecoveryRow {
    pub recovery: [u8; 16],
    pub operation: [u8; 16],
    pub original_name: EntryName,
    pub kind: RecoveryKind,
}
pub struct RecoveryPage {
    pub entries: Vec<RecoveryRow>,
    pub next: Vec<u8>,
    pub done: bool,
}
#[derive(Clone, Debug)]
pub enum JobUpdate {
    Progress {
        sequence: u64,
        item: usize,
        phase: String,
        bytes: u64,
    },
    Item {
        item: usize,
        state: ItemState,
        destination: Option<[u64; 2]>,
        error: Option<String>,
        recoveries: Vec<[u8; 16]>,
    },
    Finished {
        completed: usize,
        failed: usize,
        cancelled: usize,
    },
}
#[derive(Clone, Debug)]
pub struct JobPoll {
    pub operation_id: [u8; 16],
    pub updates: Vec<JobUpdate>,
}
/// Held source filesystem metadata. Counts use `fragment_size` units; no rights.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryFilesystemStats {
    pub blocks: u64,
    pub blocks_free: u64,
    pub blocks_available: u64,
    pub files: u64,
    pub files_free: u64,
    pub files_available: u64,
    pub block_size: u32,
    pub fragment_size: u32,
    pub name_max: u32,
    pub read_only: bool,
}
impl DirectoryFilesystemStats {
    /// # Errors
    /// Rejects impossible counts, absent units and unrepresentable byte totals.
    pub fn validate(&self) -> Result<(), String> {
        if self.block_size == 0
            || self.fragment_size == 0
            || self.name_max == 0
            || self.blocks_free > self.blocks
            || self.blocks_available > self.blocks_free
            || self.files_free > self.files
            || self.files_available > self.files_free
            || self
                .blocks
                .checked_mul(u64::from(self.fragment_size))
                .is_none()
        {
            return Err("Invalid source filesystem capacity".into());
        }
        Ok(())
    }
}
/// Original readable inode observations. These are not record IDs or grants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryReadObservation {
    pub source_owner: [u8; 32],
    pub root: [u64; 2],
    pub parent: [u64; 2],
    pub parents: Vec<Vec<u8>>,
    pub name: Vec<u8>,
    pub identity: [u64; 2],
}
impl HistoryReadObservation {
    /// Validate only the finite descriptive vocabulary, never mint authority.
    /// # Errors
    /// Refuses malformed native identities or raw component bounds.
    pub fn validate(&self) -> Result<(), String> {
        if self.source_owner == [0; 32]
            || self.root[1] == 0
            || self.parent[1] == 0
            || self.identity[1] == 0
            || self.parents.len() > 64
        {
            return Err("Invalid history Read observation".into());
        }
        let mut total = 0usize;
        for part in self.parents.iter().chain(std::iter::once(&self.name)) {
            if part.is_empty()
                || part.len() > 255
                || part == b"."
                || part == b".."
                || part.contains(&0)
                || part.contains(&b'/')
            {
                return Err("Invalid history Read component".into());
            }
            total = total
                .checked_add(part.len())
                .ok_or("History Read lineage overflow")?;
        }
        if total > 4096 {
            return Err("History Read lineage exceeds bound".into());
        }
        Ok(())
    }
}
/// Existing bounded history xattr vocabulary. Query names/cursors grant no
/// source, record or daemon authority; provider admission remains separate.
/// # Errors
/// Refuses arbitrary xattrs, malformed canonical queries or overlong names.
pub fn validate_history_query(query: &[u8]) -> Result<&str, String> {
    if query.len() > 127 || !query.is_ascii() {
        return Err("Invalid history query name".into());
    }
    let name = std::str::from_utf8(query).map_err(|_| "Invalid history query UTF-8")?;
    if name != "user.tilo.history" && name != "user.tilo.record-id" {
        fs_protocol::files::FileHistoryReadQueryV1::parse_xattr(name)
            .map_err(|_| "Invalid canonical history query")?;
    }
    Ok(name)
}
/// # Errors
/// Refuses oversized responses and invalid fixed record-ID lengths.
pub fn validate_history_response(query: &[u8], bytes: &[u8]) -> Result<(), String> {
    let name = validate_history_query(query)?;
    if bytes.len() > fs_protocol::files::MAX_HISTORY_READ_BYTES
        || (name == "user.tilo.record-id" && (bytes.len() != 16 || bytes == [0; 16]))
        || (name == "user.tilo.history" && (bytes.is_empty() || bytes.len() > 16 * 1024))
    {
        return Err("Invalid history response size".into());
    }
    Ok(())
}
/// Observed native namespace custody only, never a signed history receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementObservation {
    pub operation: [u8; 16],
    pub source_owner: [u8; 32],
    pub capture: [u8; 16],
    pub source: [u64; 2],
    pub victim: [u64; 2],
    pub destination: [u64; 2],
}
/// One bounded live enumeration page, not a stable or sorted snapshot.
#[derive(Debug)]
pub struct DirectoryStreamRow {
    pub name: EntryName,
    /// None records a native per-entry metadata refusal under still-live scope.
    pub stat: Option<DirectoryStat>,
}
#[derive(Debug)]
pub struct DirectoryStreamPage {
    pub entries: Vec<DirectoryStreamRow>,
    pub next_sequence: u64,
    pub done: bool,
}
/// Observational binding of actual zero-effect native prepared work. No grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplacementPurpose {
    ReplacementRename,
    AtomicSave,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingReplacementObservation {
    pub purpose: ReplacementPurpose,
    pub operation: [u8; 16],
    pub source_owner: [u8; 32],
    pub source: [u64; 2],
    pub expected_victim: [u64; 2],
    pub intent_digest: [u8; 32],
    pub archive_plan_digest: [u8; 32],
    /// Canonical native intent bytes, descriptive only; never decoded into Work.
    pub reservation_description: Vec<u8>,
}
impl PendingReplacementObservation {
    /// # Errors
    /// Rejects malformed observations; success never grants execution authority.
    pub fn validate(&self) -> Result<(), String> {
        if self.reservation_description.is_empty()
            || self.reservation_description.len() > 2 * 1024 * 1024
        {
            return Err("invalid native intent description length".into());
        }
        let mut digest = blake3::Hasher::new();
        digest.update(b"tilo-native-retained-replacement-intent-v2\0");
        digest.update(&self.reservation_description);
        if digest.finalize().as_bytes() != &self.intent_digest {
            return Err("native intent description digest mismatch".into());
        }
        if self.operation == [0; 16]
            || self.source_owner == [0; 32]
            || self.source[1] == 0
            || self.expected_victim[1] == 0
            || self.source == self.expected_victim
            || self.source[0] != self.expected_victim[0]
            || self.intent_digest == [0; 32]
            || self.archive_plan_digest == [0; 32]
        {
            return Err("Invalid prepared replacement observation".into());
        }
        let description = crate::external_history::NativeReservationDescription::decode(
            &self.reservation_description,
            self.operation,
            self.source_owner,
            self.intent_digest,
        )
        .map_err(|error| error.to_string())?;
        let actual_purpose = match description.purpose() {
            crate::external_history::NativeReplacementPurpose::ReplacementRename => {
                ReplacementPurpose::ReplacementRename
            }
            crate::external_history::NativeReplacementPurpose::AtomicSave => {
                ReplacementPurpose::AtomicSave
            }
        };
        if self.purpose != actual_purpose
            || description.source().identity() != Some(self.source)
            || description.victim().identity() != Some(self.expected_victim)
        {
            return Err(
                "Declared native replacement observation does not match canonical intent".into(),
            );
        }
        Ok(())
    }
}
/// Closed result of a zero-effect `AtomicSave` reservation request.
/// Refused means an acknowledged no-reservation result; transport loss is Unknown.
pub enum AtomicSavePreparation<T> {
    Prepared(T),
    Refused(String),
    Unknown(String),
}

mod atomic_save_reservation;
pub use atomic_save_reservation::AtomicSaveReservation;
