//! Typed backend seam shared by native grant owners and guarded RPC relays.
//! These objects never encode credentials, groups, native grants or root claims.
use crate::operations::ItemState;
use crate::workspace_server::WorkspaceFileBackend;
use crate::{EntryName, Metadata, OpenMode};
use std::sync::Arc;
/// Observational inode identity, never an authority or grant constructor.
#[derive(Clone, Debug)]
pub struct DirectoryStat {
    pub metadata: Metadata,
    pub identity: [u64; 2],
    /// Observed native S_IFMT bits; never permissions or authority.
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
    pub kind: crate::recovery::RecoveryKind,
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
/// The authenticating host owns backend provenance and the live consumption
/// lease. A root relay must wrap its authenticated source connection and recheck
/// its private authority before each method; wire labels do not construct it.
/// Held source filesystem metadata. Counts use fragment_size units; no rights.
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
/// Genuine source-resident Read custody. Implementations must retain actual
/// original credentials and grant; descriptive observations cannot construct it.
pub trait WorkspaceHistoryReadSubjectBackend: Send + Sync + 'static {
    /// Release only this private controller cache alias after its admitted owner
    /// observed retirement. No File/OFD or creator aliases are bulk-deleted.
    fn retire_controller_cache(&self) -> Result<(), String> {
        Err("Owned controller cache retirement unavailable".into())
    }

    fn retire_daemon_handoff(&self, _ticket: [u8; 16]) -> Result<(), String> {
        Err("History Read handoff retirement unavailable".into())
    }
    fn arm_daemon_handoff(&self, _challenge: [u8; 32]) -> Result<[u8; 16], String> {
        Err("History Read handoff unavailable".into())
    }

    fn check_scope(&self) -> Result<(), String>;
    fn check(&self) -> Result<(), String>;
    fn observe(&self) -> Result<HistoryReadObservation, String>;
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
pub trait WorkspaceDirectoryBackend: Send + Sync + 'static {
    /// Source providers cannot infer canonical history from path/inode labels.
    /// # Errors
    /// Refuses absent independently enrolled Read/history authority.
    fn history_query(
        &self,
        _name: &EntryName,
        _expected: [u64; 2],
        _query: &[u8],
    ) -> Result<Vec<u8>, String> {
        Err("Enrolled original-source history Read unavailable".into())
    }

    /// # Errors
    /// Refuses unsupported providers, revoked Read scope or nonregular children.
    fn history_read_subject(
        &self,
        _name: &EntryName,
    ) -> Result<Arc<dyn WorkspaceHistoryReadSubjectBackend>, String> {
        Err("Original-source history Read custody unavailable".into())
    }

    fn set_created_permissions(
        &self,
        _name: &EntryName,
        _expected: [u64; 2],
        _mode: u32,
    ) -> Result<(), String> {
        Err("Own-created permissions authority unavailable".into())
    }

    /// Only a filtered own-created scope implements this. Ordinary Home cannot
    /// derive deletion/capture authority from caller names or inode labels.
    fn retire_created(
        &self,
        _name: &EntryName,
        _expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        Err("Own-created transient custody unavailable".into())
    }

    /// # Errors
    /// Refuses unsupported providers, revoked metadata scope or actual fsync IO.
    fn sync_directory(&self) -> Result<(), String> {
        Err("Held source directory sync unavailable".into())
    }

    /// # Errors
    /// Refuses unsupported providers or revoked genuine directory metadata scope.
    fn prepare_replace(
        &self,
        temporary: &EntryName,
        temporary_expected: [u64; 2],
        victim: &EntryName,
        victim_expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspacePendingReplacementBackend>, String> {
        let _ = (temporary, temporary_expected, victim, victim_expected);
        Err("Certified replacing source unavailable".into())
    }
    /// # Errors
    /// Refuses unsupported providers or revoked genuine directory metadata scope.
    fn statfs(&self) -> Result<DirectoryFilesystemStats, String> {
        Err("Source filesystem capacity unavailable".into())
    }
    /// # Errors
    /// Refuses revoked or unavailable native/root consumption authority.
    fn check(&self) -> Result<(), String>;
    /// Open one independent native cursor retaining this genuine directory grant.
    /// # Errors
    /// Refuses native enumeration admission or revoked scope.
    fn open_cursor(&self) -> Result<Arc<dyn WorkspaceDirectoryCursorBackend>, String>;
    /// # Errors
    /// Returns actual held directory metadata or current authority/IO failure.
    fn metadata(&self) -> Result<DirectoryStat, String>;
    /// # Errors
    /// Observes one child without following links or opening its bytes.
    fn stat(&self, name: &EntryName) -> Result<Option<DirectoryStat>, String>;
    /// Observe raw symlink bytes without granting target traversal or content access.
    /// # Errors
    /// Refuses substituted links, revoked scope, or native IO.
    fn readlink(&self, name: &EntryName, expected: [u64; 2]) -> Result<Vec<u8>, String>;
    /// Retain a regular inode for metadata only, without byte access.
    /// # Errors
    /// Refuses changed identity, wrong type, revoked scope or native IO.
    fn pin_metadata(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceMetadataBackend>, String>;
    /// # Errors
    /// Refuses stale cursors, revoked scope, snapshot limits or host IO.
    fn list(&self, cursor: u64) -> Result<DirectoryRows, String>;
    /// # Errors
    /// Returns actual native/relay rights, type, admission, revocation or IO failures.
    fn directory(&self, name: &EntryName) -> Result<Arc<dyn WorkspaceDirectoryBackend>, String>;
    /// # Errors
    /// Returns actual native/relay rights, type, admission, revocation or IO failures.
    fn open_file(
        &self,
        name: &EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceFileBackend>, String>;
    /// # Errors
    /// Returns actual native/relay rights, type, admission, revocation or IO failures.
    fn create_file(&self, name: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
    /// # Errors
    /// Returns actual native/relay rights, type, admission, revocation or IO failures.
    fn create_directory(&self, name: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
    /// # Errors
    /// Returns actual native/relay rights, type, admission, revocation or IO failures.

    /// # Errors
    /// Refuses missing private owner, foreign cursor or revoked parent scope.
    fn recoveries(&self, after: &[u8]) -> Result<RecoveryPage, String> {
        let _ = after;
        Err("Native recovery owner not admitted".into())
    }
    /// # Errors
    /// Refuses unrelated recovery, missing genuine grants, collision or revoked scope.
    fn restore_captured(
        &self,
        recovery: [u8; 16],
        name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let _ = (recovery, name);
        Err("Native recovery owner not admitted".into())
    }
    /// # Errors
    /// Returns actual native rename authority, admission or IO failures.
    /// Protected same-parent replacing save through the real signed coordinator.
    /// Ordinary directories have no implicit replacing authority.
    fn atomic_save(
        &self,
        _temporary: &EntryName,
        _expected: [u64; 2],
        _target: &EntryName,
    ) -> Result<(), String> {
        Err("Explicit AtomicSave coordinator unavailable".into())
    }
    fn rename_replace(
        &self,
        _name: &EntryName,
        _new_name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        Err("Certified replacing-save coordinator unavailable".into())
    }
    fn rename_no_replace(
        &self,
        name: &EntryName,
        new_name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
}
/// A held regular inode with metadata access only. No byte methods exist.
pub trait WorkspaceMetadataBackend: Send + Sync + 'static {
    /// Native/guarded providers return Some for both valid and revoked scope.
    fn live_scope(&self) -> Option<Result<(), String>> {
        None
    }
    /// # Errors
    /// Returns live held-object metadata or authority/IO failure.
    fn metadata(&self) -> Result<DirectoryStat, String>;
}
/// Real native durable outcomes (or their authenticated relay), not signed
/// collaborative file history. Neither record IDs nor updates confer authority.
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
/// The provider retains a genuine native pin; no bytes or ownership mutation.
pub trait WorkspaceReplacementPinBackend: Send + Sync + 'static {
    /// Release only this private controller cache alias after its admitted owner
    /// observed retirement. No File/OFD or creator aliases are bulk-deleted.
    fn retire_controller_cache(&self) -> Result<(), String> {
        Err("Owned controller cache retirement unavailable".into())
    }

    fn commit_with_history(&self, _receipt: &[u8]) -> Result<(), String> {
        Err("Certified durable history commit unavailable".into())
    }

    /// # Errors
    /// Refuses revoked source scope or changed capture/destination custody.
    fn check(&self) -> Result<(), String>;
    /// # Errors
    /// Reobserves the genuine pin under its current source/parent authority.
    fn observe(&self) -> Result<ReplacementObservation, String>;
}
pub trait WorkspaceJobBackend: Send + Sync + 'static {
    /// Release only this private controller cache alias after its admitted owner
    /// observed retirement. No File/OFD or creator aliases are bulk-deleted.
    fn retire_controller_cache(&self) -> Result<(), String> {
        Err("Owned controller cache retirement unavailable".into())
    }

    /// # Errors
    /// Only the actual native completed creation can mint transient custody.
    fn created_temporary(&self) -> Result<Arc<dyn WorkspaceCreatedTemporaryBackend>, String> {
        Err("Actual created temporary custody unavailable".into())
    }

    /// # Errors
    /// Refuses ordinary/uncertain jobs, absent root source binding or revoked scope.
    fn pin_replacement(&self) -> Result<Arc<dyn WorkspaceReplacementPinBackend>, String> {
        Err("Completed replacement custody unavailable".into())
    }
    /// Native/guarded providers return Some for both valid and revoked scope.
    fn live_scope(&self) -> Option<Result<(), String>> {
        None
    }
    /// # Errors
    /// Returns actual native publication, revocation or authenticated relay failures.
    fn poll(&self) -> Result<JobPoll, String>;
    /// # Errors
    /// Refuses revoked scope or native/relay cancellation failures.
    fn cancel(&self) -> Result<(), String>;
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
/// An independent held-directory cursor, with no byte or mutation rights.
pub trait WorkspaceDirectoryCursorBackend: Send + Sync + 'static {
    /// # Errors
    /// Refuses revoked session/ancestor authority.
    fn check(&self) -> Result<(), String>;
    /// # Errors
    /// Refuses sequence mismatch, native enumeration IO or revoked authority.
    fn page(&self, expected_sequence: u64) -> Result<DirectoryStreamPage, String>;
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
            crate::operations::NativeReplacementPurpose::ReplacementRename => {
                ReplacementPurpose::ReplacementRename
            }
            crate::operations::NativeReplacementPurpose::AtomicSave => {
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
/// Actual held reservation, or guarded forwarding through its existing channel.
/// No public data can construct a native reservation or root verifier.
pub trait WorkspacePendingReplacementBackend: Send + Sync + 'static {
    /// Release only this private controller cache alias after its admitted owner
    /// observed retirement. No File/OFD or creator aliases are bulk-deleted.
    fn retire_controller_cache(&self) -> Result<(), String> {
        Err("Owned controller cache retirement unavailable".into())
    }

    /// SAME submitted native job, replaying its actually observed terminal report
    /// once for a newly admitted root client after signed ownership verification.
    fn acknowledged_committed_job(&self) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        Err("Acknowledged source native terminal receipt unavailable".into())
    }
    /// Returns the actual submitted job's pin only after private signed history
    /// ownership verification. Descriptive receipts cannot construct this cap.
    fn acknowledged_committed_pin(
        &self,
    ) -> Result<Arc<dyn WorkspaceReplacementPinBackend>, String> {
        Err("Acknowledged source history ownership unavailable".into())
    }
    /// Retire only the separately owned daemon RPC transport, never the source namespace.
    fn retire_daemon_handoff(&self, _ticket: [u8; 16]) -> Result<(), String> {
        Err("source handoff retirement unavailable".into())
    }
    fn arm_daemon_handoff(&self, _challenge: [u8; 32]) -> Result<[u8; 16], String> {
        Err("Authenticated source pending route unavailable".into())
    }

    /// # Errors
    /// Revalidates the genuine source and still-held zero-effect work.
    fn check(&self) -> Result<(), String>;
    /// # Errors
    /// Returns only actual current native intent, not publication evidence.
    fn observe(&self) -> Result<PendingReplacementObservation, String>;
    /// # Errors
    /// Consumes once through the native sealed-verifier signed-prepare gate.
    fn start_with_history(&self, receipt: &[u8]) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
    /// # Errors
    /// Releases only this unsubmitted reservation; cannot undo committed effects.
    fn cancel(&self) -> Result<(), String>;
}

mod atomic_save_reservation;
pub use atomic_save_reservation::AtomicSaveReservation;

/// Closed result of a zero-effect AtomicSave reservation request.
/// Refused means an acknowledged no-reservation result; transport loss is Unknown.
pub enum AtomicSavePreparation<T> {
    Prepared(T),
    Refused(String),
    Unknown(String),
}

/// Actual source-held completed creation custody, not receipt observations.
pub trait WorkspaceCreatedTemporaryBackend: Send + Sync + 'static {
    fn set_permissions(&self, _mode: u32) -> Result<(), String> {
        Err("Actual creator permissions authority unavailable".into())
    }

    /// Source-resident creator custody plus the independently sealed selected
    /// replacement scope, never names or an incoming receipt as authority.
    fn prepare_atomic_save(
        &self,
    ) -> AtomicSavePreparation<Arc<dyn WorkspacePendingReplacementBackend>> {
        AtomicSavePreparation::Refused("Explicit AtomicSave authority unavailable".into())
    }
    fn prepare_replace(&self) -> Result<Arc<dyn WorkspacePendingReplacementBackend>, String> {
        Err("Actual selected replacement custody unavailable".into())
    }

    fn live_scope(&self) -> Option<Result<(), String>> {
        None
    }
    /// # Errors
    /// Refuses changed created inode, source revocation or invalid native receipt.
    fn check(&self) -> Result<(), String>;
    /// # Errors
    /// One-use cleanup retains bytes in protected recovery via a real native job.
    fn retire(&self) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
}
