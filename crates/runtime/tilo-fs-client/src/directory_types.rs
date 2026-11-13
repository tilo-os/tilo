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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectoryStat {
    pub metadata: Metadata,
    pub identity: [u64; 2],
    /// Observed native `S_IFMT` bits; never permissions or authority.
    pub file_type: u32,
    /// Original observed execute mode bits only (0111), never Execute authority.
    pub execute_bits: u16,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct DirectoryRow {
    pub name: EntryName,
    pub metadata: Metadata,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct DirectoryRows {
    pub entries: Vec<DirectoryRow>,
    pub next: u64,
}
/// Bounded journal observation; IDs and cursor bytes do not confer rights.
#[derive(Debug, Serialize, Deserialize)]
pub struct RecoveryRow {
    pub recovery: [u8; 16],
    pub operation: [u8; 16],
    pub original_name: EntryName,
    pub kind: RecoveryKind,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct RecoveryPage {
    pub entries: Vec<RecoveryRow>,
    pub next: Vec<u8>,
    pub done: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobPoll {
    pub operation_id: [u8; 16],
    pub updates: Vec<JobUpdate>,
}
/// Held source filesystem metadata. Counts use `fragment_size` units; no rights.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
/// One bounded live enumeration page, not a stable or sorted snapshot.
#[derive(Debug, Serialize, Deserialize)]
pub struct DirectoryStreamRow {
    pub name: EntryName,
    /// None records a native per-entry metadata refusal under still-live scope.
    pub stat: Option<DirectoryStat>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct DirectoryStreamPage {
    pub entries: Vec<DirectoryStreamRow>,
    pub next_sequence: u64,
    pub done: bool,
}
