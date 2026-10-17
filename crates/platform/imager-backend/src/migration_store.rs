//! Private device-local index for migration operations.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use serde::{Deserialize, Serialize};

use super::destination_security;
use super::migration::{CopyProgress, CopyReport, OwnedTemporary, PinnedDirectory};
use super::migration_durability;
use super::migration_worker::{MigrationJob, WorkerOutcome};
use super::nofollow::{metadata_nofollow, same_directory_identity};

const STORE_FILE: &str = "operations.json";
const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OPERATIONS: usize = 256;
const ISSUE_PREVIEW: usize = 8;
const MAX_ISSUE_BYTES: u64 = 128 * 1024;
static NEXT_REPORT: AtomicU64 = AtomicU64::new(0);

// Display snapshots have their own identity. Equal counts/previews must never
// let an acknowledgement attach a previous attempt's full issue details.
fn report_identity() -> String {
    format!(
        "{}-{:?}-{}",
        std::process::id(),
        std::time::SystemTime::now(),
        NEXT_REPORT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", content = "units", deny_unknown_fields)]
pub enum NativePath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl NativePath {
    pub fn encode(path: &Path) -> Self {
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

    pub fn decode(&self) -> Result<PathBuf, String> {
        let units = match self {
            Self::Unix(units) => units.len(),
            Self::Windows(units) => units.len(),
        };
        if units > 4096 {
            return Err("persisted migration path exceeds bounds".into());
        }
        match self {
            #[cfg(unix)]
            Self::Unix(bytes) if !bytes.contains(&0) => {
                use std::os::unix::ffi::OsStringExt;
                Ok(PathBuf::from(OsString::from_vec(bytes.clone())))
            }
            #[cfg(windows)]
            Self::Windows(units) if !units.contains(&0) => {
                use std::os::windows::ffi::OsStringExt;
                Ok(PathBuf::from(OsString::from_wide(units)))
            }
            _ => Err("persisted migration path has a platform mismatch or NUL".into()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    AwaitingReview,
    Ready,
    Running,
    Interrupted,
    Cancelled,
    Incomplete,
    Failed,
    Complete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredProgress {
    verified_files: u64,
    verified_bytes: u64,
    current_relative_path: NativePath,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIssue {
    path: NativePath,
    message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReport {
    #[serde(default)]
    identity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    verified_files: Vec<NativePath>,
    #[serde(default)]
    verified_count: usize,
    verified_bytes: u64,
    #[serde(default)]
    issue_count: usize,
    #[serde(default)]
    details: Option<String>,
    skipped: Vec<StoredIssue>,
    failed: Vec<StoredIssue>,
    cancelled: bool,
    scan_completed: bool,
    metadata_limitations: Vec<String>,
}

impl StoredReport {
    fn from_report(report: CopyReport) -> Self {
        Self {
            identity: Some(report_identity()),
            verified_count: report.verified_files.len(),
            verified_files: Vec::new(),
            verified_bytes: report.verified_bytes,
            issue_count: report.skipped.len() + report.failed.len(),
            details: None,
            skipped: report
                .skipped
                .into_iter()
                .map(|(path, message)| StoredIssue {
                    path: NativePath::encode(&path),
                    message,
                })
                .collect(),
            failed: report
                .failed
                .into_iter()
                .map(|(path, message)| StoredIssue {
                    path: NativePath::encode(&path),
                    message,
                })
                .collect(),
            cancelled: report.cancelled,
            scan_completed: report.scan_completed,
            metadata_limitations: report.metadata_limitations,
        }
    }
}

/// A local display/restart record. The journal remains the authority for every
/// resumed byte and root identity; this index never authorizes overwrite.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRecord {
    id: String,
    source: NativePath,
    destination: NativePath,
    journal: NativePath,
    review_revision: u64,
    approved_revision: Option<u64>,
    state: OperationState,
    progress: Option<StoredProgress>,
    report: Option<StoredReport>,
    failure: Option<String>,
}

impl OperationRecord {
    pub fn new(job: &MigrationJob) -> Self {
        Self {
            id: job.operation_id().to_string(),
            source: NativePath::encode(job.source()),
            destination: NativePath::encode(job.destination()),
            journal: NativePath::encode(job.journal()),
            review_revision: 1,
            approved_revision: None,
            state: OperationState::AwaitingReview,
            progress: None,
            report: None,
            failure: None,
        }
    }

    pub fn issue_count(&self) -> usize {
        self.report
            .as_ref()
            .map_or(0, |r| r.issue_count.max(r.skipped.len() + r.failed.len()))
    }

    pub fn retain_detail_reference(&mut self, saved: &Self) {
        if self.id == saved.id
            && self.state == saved.state
            && let (Some(current), Some(saved)) = (&mut self.report, &saved.report)
            && current.details.is_none()
            && current.identity.is_some()
            && current.identity == saved.identity
            && current.issue_count == saved.issue_count
        {
            current.details.clone_from(&saved.details);
        }
    }

    pub fn compact(&mut self) {
        if let Some(report) = &mut self.report {
            report.verified_count = report.verified_count.max(report.verified_files.len());
            report.verified_files.clear();
            report.issue_count = report
                .issue_count
                .max(report.skipped.len() + report.failed.len());
            report.skipped.truncate(ISSUE_PREVIEW);
            report
                .failed
                .truncate(ISSUE_PREVIEW.saturating_sub(report.skipped.len()));
        }
    }

    pub fn persistence_failed(&mut self, error: &str) {
        self.state = OperationState::Incomplete;
        self.failure = Some(format!(
            "The result could not be saved locally: {error}. Reopen and revalidate before continuing."
        ));
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn source(&self) -> Result<PathBuf, String> {
        self.source.decode()
    }

    pub fn destination(&self) -> Result<PathBuf, String> {
        self.destination.decode()
    }

    pub fn journal(&self) -> Result<PathBuf, String> {
        self.journal.decode()
    }

    #[must_use]
    pub fn state(&self) -> OperationState {
        self.state
    }

    #[must_use]
    pub fn verified_bytes(&self) -> u64 {
        self.report.as_ref().map_or_else(
            || self.progress.as_ref().map_or(0, |p| p.verified_bytes),
            |r| r.verified_bytes,
        )
    }

    #[must_use]
    pub fn verified_file_count(&self) -> usize {
        self.report.as_ref().map_or_else(
            || {
                self.progress
                    .as_ref()
                    .map_or(0, |p| p.verified_files as usize)
            },
            |r| r.verified_count.max(r.verified_files.len()),
        )
    }

    pub fn current_relative_path(&self) -> Result<Option<PathBuf>, String> {
        self.progress
            .as_ref()
            .map(|progress| progress.current_relative_path.decode())
            .transpose()
    }

    pub fn issues(&self) -> Result<Vec<(PathBuf, String)>, String> {
        let Some(report) = &self.report else {
            return Ok(Vec::new());
        };
        report
            .skipped
            .iter()
            .chain(&report.failed)
            .map(|issue| Ok((issue.path.decode()?, issue.message.clone())))
            .collect()
    }

    #[must_use]
    pub fn metadata_limitations(&self) -> &[String] {
        self.report
            .as_ref()
            .map_or(&[], |report| report.metadata_limitations.as_slice())
    }

    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn approve(&mut self) {
        self.approved_revision = Some(self.review_revision);
        self.state = OperationState::Ready;
    }

    pub fn start_job(&self) -> Result<MigrationJob, String> {
        if self.state != OperationState::Ready
            || self.approved_revision != Some(self.review_revision)
        {
            return Err("source and destination must be reviewed before copying".into());
        }
        MigrationJob::fresh(
            self.id.clone(),
            self.source()?,
            self.destination()?,
            self.journal()?,
        )
    }

    pub fn retry_job(&self) -> Result<MigrationJob, String> {
        if self.approved_revision != Some(self.review_revision)
            || !matches!(
                self.state,
                OperationState::Interrupted
                    | OperationState::Cancelled
                    | OperationState::Incomplete
                    | OperationState::Failed
            )
        {
            return Err("migration is not ready for an explicit retry".into());
        }
        MigrationJob::fresh(
            self.id.clone(),
            self.source()?,
            self.destination()?,
            self.journal()?,
        )?
        .retry()
    }

    pub fn mark_running(&mut self) -> Result<(), String> {
        if self.approved_revision != Some(self.review_revision)
            || !matches!(
                self.state,
                OperationState::Ready
                    | OperationState::Interrupted
                    | OperationState::Cancelled
                    | OperationState::Incomplete
                    | OperationState::Failed
            )
        {
            return Err("migration cannot start without current reviewed grants".into());
        }
        self.state = OperationState::Running;
        self.progress = None;
        self.report = None;
        self.failure = None;
        Ok(())
    }

    pub fn apply_progress(&mut self, progress: &CopyProgress) {
        if self.state == OperationState::Running {
            self.progress = Some(StoredProgress {
                verified_files: progress.verified_files,
                verified_bytes: progress.verified_bytes,
                current_relative_path: NativePath::encode(&progress.current_relative_path),
            });
        }
    }

    pub fn apply_outcome(&mut self, outcome: WorkerOutcome) {
        match outcome {
            WorkerOutcome::Complete(report) if report.complete() => {
                self.state = OperationState::Complete;
                self.report = Some(StoredReport::from_report(report));
            }
            WorkerOutcome::Complete(report) | WorkerOutcome::Incomplete(report) => {
                self.state = if report.cancelled {
                    OperationState::Cancelled
                } else {
                    OperationState::Incomplete
                };
                self.report = Some(StoredReport::from_report(report));
            }
            WorkerOutcome::Failed(error) => {
                self.state = OperationState::Failed;
                self.failure = Some(error);
            }
            WorkerOutcome::Interrupted => self.state = OperationState::Interrupted,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreDocument {
    version: u32,
    operations: Vec<OperationRecord>,
}

/// Handle-pinned, owner-private local operation storage.
pub struct MigrationStore {
    root_path: PathBuf,
    root: Arc<PinnedDirectory>,
    // The descriptor lock survives while any live worker owns this store.
    ownership: advisory_lock::FileLock,
    ownership_identity: Metadata,
    // Fields drop in declaration order: report release after the descriptor
    // lock closes, even when detached readers or writers own the final Arc.
    #[cfg(any(test, feature = "test-support"))]
    ownership_released: test_support::OwnershipReleased,
}

impl MigrationStore {
    pub fn open_default() -> Result<Self, String> {
        let root = tilo_paths::data_local_dir()
            .ok_or_else(|| "no private local data directory is available".to_string())?
            .join("tilo-imager")
            .join("migrations");
        Self::open_at(&root)
    }

    pub fn open_at(path: &Path) -> Result<Self, String> {
        let root = ensure_private_root(path).map_err(|error| {
            format!(
                "private migration state is unavailable at {}: {error}",
                path.display()
            )
        })?;
        let name = Path::new("ownership.lock");
        // Never remove the lock pathname: unlinking a held lock allows a second
        // inode/owner. A dead process releases its OS lock automatically.
        if matches!(metadata_nofollow(&root.file, name), Err(e) if e.kind() == io::ErrorKind::NotFound)
        {
            let temporary = OwnedTemporary::create(&root, OsStr::new("ownership.lock"))
                .map_err(|e| e.to_string())?;
            match cap_primitives::fs::hard_link(
                &root.file,
                Path::new(&temporary.name),
                &root.file,
                name,
            ) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        let expected = metadata_nofollow(&root.file, name).map_err(|e| e.to_string())?;
        if !expected.is_file() || expected.is_symlink() {
            return Err("migration ownership lock is not a regular private file".into());
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .follow(FollowSymlinks::No)
            .nonblock(true);
        let ownership =
            cap_primitives::fs::open(&root.file, name, &options).map_err(|e| e.to_string())?;
        destination_security::check_private_file(&ownership).map_err(|e| e.to_string())?;
        let ownership_identity = Metadata::from_file(&ownership).map_err(|e| e.to_string())?;
        if !same_directory_identity(&expected, &ownership_identity) {
            return Err("migration ownership lock was replaced".into());
        }
        let ownership = advisory_lock::FileLock::try_exclusive(ownership)
            .map_err(|e| format!("folder imports are busy in another window or process: {e}"))?;
        let current = metadata_nofollow(&root.file, name).map_err(|e| e.to_string())?;
        if !same_directory_identity(&current, &ownership_identity) {
            return Err("migration ownership lock was replaced".into());
        }
        Ok(Self {
            ownership,
            ownership_identity,
            root_path: path.to_path_buf(),
            root,
            #[cfg(any(test, feature = "test-support"))]
            ownership_released: test_support::OwnershipReleased::default(),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root_path
    }

    fn validate_ownership(&self) -> Result<(), String> {
        destination_security::check_private_file(&self.ownership).map_err(|e| e.to_string())?;
        let current = metadata_nofollow(&self.root.file, Path::new("ownership.lock"))
            .map_err(|e| e.to_string())?;
        if !current.is_file()
            || current.is_symlink()
            || !same_directory_identity(&current, &self.ownership_identity)
        {
            return Err("migration ownership lock changed; stop and reopen folder imports".into());
        }
        Ok(())
    }

    /// Continuation shares the index's pinned root and exclusive ownership.
    pub fn load_continuation_bytes(&self) -> Result<Option<Vec<u8>>, String> {
        self.validate_ownership()?;
        self.root
            .validate_destination()
            .map_err(|e| e.to_string())?;
        let name = Path::new("desktop-continuation.json");
        let expected = match metadata_nofollow(&self.root.file, name) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if !expected.is_file() || expected.is_symlink() || expected.len() > 256 * 1024 {
            return Err("continuation is not a bounded regular file".into());
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let mut file =
            cap_primitives::fs::open(&self.root.file, name, &options).map_err(|e| e.to_string())?;
        destination_security::check_private_file(&file).map_err(|e| e.to_string())?;
        if !same_directory_identity(
            &expected,
            &Metadata::from_file(&file).map_err(|e| e.to_string())?,
        ) {
            return Err("continuation changed while opening".into());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(256 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 256 * 1024 {
            return Err("continuation exceeds bounds".into());
        }
        Ok(Some(bytes))
    }

    pub fn save_continuation_bytes(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > 256 * 1024 {
            return Err("continuation exceeds bounds".into());
        }
        self.validate_ownership()?;
        self.root
            .validate_destination()
            .map_err(|e| e.to_string())?;
        let name = OsStr::new("desktop-continuation.json");
        let mut temporary = OwnedTemporary::create(&self.root, name).map_err(|e| e.to_string())?;
        temporary.file.write_all(bytes).map_err(|e| e.to_string())?;
        migration_durability::sync_file(&temporary.file).map_err(|e| e.to_string())?;
        temporary.validate().map_err(|e| e.to_string())?;
        self.validate_ownership()?;
        self.root
            .validate_destination()
            .map_err(|e| e.to_string())?;
        cap_primitives::fs::rename(
            &self.root.file,
            Path::new(&temporary.name),
            &self.root.file,
            Path::new(name),
        )
        .map_err(|e| e.to_string())?;
        migration_durability::sync_directory(&self.root.file).map_err(|e| e.to_string())
    }

    pub fn load(&self) -> Result<Vec<OperationRecord>, String> {
        self.validate_ownership()?;
        self.root
            .validate_destination()
            .map_err(|e| e.to_string())?;
        destination_security::check_private_file(&self.root.file).map_err(|e| e.to_string())?;
        let expected = match metadata_nofollow(&self.root.file, Path::new(STORE_FILE)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("cannot inspect migration state: {error}")),
        };
        if !expected.is_file() || expected.is_symlink() || expected.len() > MAX_STORE_BYTES {
            return Err("migration state is not a bounded regular file".into());
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = cap_primitives::fs::open(&self.root.file, Path::new(STORE_FILE), &options)
            .map_err(|e| format!("cannot open migration state: {e}"))?;
        destination_security::check_private_file(&file).map_err(|e| e.to_string())?;
        let actual = Metadata::from_file(&file).map_err(|e| e.to_string())?;
        if !same_directory_identity(&expected, &actual) {
            return Err("migration state changed while opening".into());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_STORE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err("migration state exceeds its size bound".into());
        }
        let mut document: StoreDocument = serde_json::from_slice(&bytes)
            .map_err(|e| format!("migration state is invalid: {e}"))?;
        if document.version != 1 || document.operations.len() > MAX_OPERATIONS {
            return Err("unsupported migration state schema or operation count".into());
        }
        for operation in &mut document.operations {
            if let Some(report) = &mut operation.report
                && report.identity.is_none()
            {
                report.identity = Some(report_identity());
            }
            operation.source()?;
            operation.destination()?;
            MigrationJob::fresh(
                operation.id.clone(),
                operation.source()?,
                operation.destination()?,
                operation.journal()?,
            )?;
            if operation.state == OperationState::Running {
                operation.state = OperationState::Interrupted;
            }
        }
        Ok(document.operations)
    }

    fn save_issues(&self, operation: &OperationRecord) -> Result<String, String> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|time| time.as_nanos())
            .ok_or_else(|| "system time is before the Unix epoch".to_string())?;
        let name = format!("{}.{stamp}.issues", operation.id);
        let mut temporary =
            OwnedTemporary::create(&self.root, OsStr::new(&name)).map_err(|e| e.to_string())?;
        if let Some(report) = &operation.report {
            for issue in report.skipped.iter().chain(&report.failed) {
                let bytes = serde_json::to_vec(issue).map_err(|e| e.to_string())?;
                if bytes.len() as u64 > MAX_ISSUE_BYTES {
                    return Err("copy issue exceeds bounded record size".into());
                }
                temporary
                    .file
                    .write_all(&bytes)
                    .and_then(|()| temporary.file.write_all(b"\n"))
                    .map_err(|e| e.to_string())?;
            }
        }
        migration_durability::sync_file(&temporary.file).map_err(|e| e.to_string())?;
        temporary.validate().map_err(|e| e.to_string())?;
        self.validate_ownership()?;
        cap_primitives::fs::hard_link(
            &self.root.file,
            Path::new(&temporary.name),
            &self.root.file,
            Path::new(&name),
        )
        .map_err(|e| e.to_string())?;
        migration_durability::sync_directory(&self.root.file).map_err(|e| e.to_string())?;
        Ok(name)
    }

    pub fn issue_page(
        &self,
        operation: &OperationRecord,
        offset: u64,
    ) -> Result<IssuePage, String> {
        self.validate_ownership()?;
        self.root
            .validate_destination()
            .map_err(|e| e.to_string())?;
        MigrationJob::fresh(
            operation.id.clone(),
            operation.source()?,
            operation.destination()?,
            operation.journal()?,
        )?;
        let name = operation
            .report
            .as_ref()
            .and_then(|r| r.details.as_deref())
            .ok_or_else(|| {
                "Full issue details were not saved; recheck the journal before continuing."
                    .to_string()
            })?;
        if !name.starts_with(&format!("{}.", operation.id))
            || !name.ends_with(".issues")
            || name.len() > 255
            || !matches!(
                Path::new(name).components().next(),
                Some(Component::Normal(_))
            )
            || Path::new(name).components().count() != 1
        {
            return Err("copy issue reference is invalid".into());
        }
        let expected =
            metadata_nofollow(&self.root.file, Path::new(&name)).map_err(|e| e.to_string())?;
        if !expected.is_file() || expected.is_symlink() {
            return Err("copy issue details are not a regular file".into());
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let mut file = cap_primitives::fs::open(&self.root.file, Path::new(&name), &options)
            .map_err(|e| e.to_string())?;
        destination_security::check_private_file(&file).map_err(|e| e.to_string())?;
        if !same_directory_identity(
            &expected,
            &Metadata::from_file(&file).map_err(|e| e.to_string())?,
        ) {
            return Err("copy details changed while opening".into());
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut reader = io::BufReader::new(file);
        let mut issues = Vec::new();
        let mut next = offset;
        for _ in 0..32 {
            let mut bytes = Vec::new();
            reader
                .by_ref()
                .take(MAX_ISSUE_BYTES + 2)
                .read_until(b'\n', &mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.is_empty() {
                break;
            }
            if bytes.len() as u64 > MAX_ISSUE_BYTES + 1 || bytes.last() != Some(&b'\n') {
                return Err("invalid bounded copy issue record".into());
            }
            next += bytes.len() as u64;
            let issue: StoredIssue = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            issues.push((issue.path.decode()?, issue.message));
        }
        Ok(IssuePage {
            issues,
            offset,
            next: (next < expected.len()).then_some(next),
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn save(&self, operations: &[OperationRecord]) -> Result<(), String> {
        self.save_snapshot(operations).map(drop)
    }

    fn save_snapshot(
        &self,
        operations: &[OperationRecord],
    ) -> Result<Vec<OperationRecord>, String> {
        #[cfg(any(test, feature = "test-support"))]
        test_support::before_save(&self.root_path);
        self.validate_ownership()?;
        if operations.len() > MAX_OPERATIONS {
            return Err("too many migration operations to persist".into());
        }
        self.root
            .validate_destination()
            .map_err(|e| e.to_string())?;
        destination_security::check_private_file(&self.root.file).map_err(|e| e.to_string())?;
        let mut compact = operations.to_vec();
        for operation in &mut compact {
            MigrationJob::fresh(
                operation.id.clone(),
                operation.source()?,
                operation.destination()?,
                operation.journal()?,
            )?;
            if let Some(report) = &operation.report {
                let count = report.skipped.len() + report.failed.len();
                if count > ISSUE_PREVIEW && count >= report.issue_count {
                    let name = self.save_issues(operation)?;
                    if let Some(report) = &mut operation.report {
                        report.details = Some(name);
                    }
                }
            }
            operation.compact();
        }
        let document = StoreDocument {
            version: 1,
            operations: compact,
        };
        let bytes = serde_json::to_vec(&document).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err("migration state exceeds its size bound".into());
        }
        let mut temporary = OwnedTemporary::create(&self.root, OsStr::new(STORE_FILE))
            .map_err(|e| e.to_string())?;
        temporary
            .file
            .write_all(&bytes)
            .map_err(|e| e.to_string())?;
        migration_durability::sync_file(&temporary.file).map_err(|e| e.to_string())?;
        temporary.validate().map_err(|e| e.to_string())?;
        cap_primitives::fs::rename(
            &self.root.file,
            Path::new(&temporary.name),
            &self.root.file,
            Path::new(STORE_FILE),
        )
        .map_err(|e| e.to_string())?;
        migration_durability::sync_directory(&self.root.file).map_err(|e| e.to_string())?;
        Ok(document.operations)
    }
}

pub struct IssuePage {
    pub issues: Vec<(PathBuf, String)>,
    pub offset: u64,
    pub next: Option<u64>,
}

type SaveRequest = (
    Vec<OperationRecord>,
    std::sync::mpsc::Sender<Result<Vec<OperationRecord>, String>>,
);

/// All index writes are serialized off the event thread. The writer owns the
/// store lock until every client (including a detached copy worker) exits and
/// every queued checkpoint has completed.
#[derive(Clone)]
pub struct StoreWriter {
    sender: std::sync::mpsc::Sender<SaveRequest>,
}
impl StoreWriter {
    pub fn new(store: Arc<MigrationStore>) -> Result<Self, String> {
        let (sender, receiver) = std::sync::mpsc::channel::<SaveRequest>();
        std::thread::Builder::new()
            .name("imager-copy-state".into())
            .spawn(move || {
                let mut failure: Option<String> = None;
                let mut previous = Vec::new();
                while let Ok((mut operations, reply)) = receiver.recv() {
                    // A queued snapshot may predate the preceding save's ACK.
                    // Preserve immutable detail references created by that save;
                    // never carry forward operation state or copy authority.
                    for (operation, saved) in operations.iter_mut().zip(&previous) {
                        operation.retain_detail_reference(saved);
                    }
                    let result = if let Some(error) = &failure {
                        Err(error.clone())
                    } else {
                        store.save_snapshot(&operations)
                    };
                    match &result {
                        Ok(saved) => previous.clone_from(saved),
                        Err(error) => failure = Some(error.clone()),
                    }
                    let _ = reply.send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { sender })
    }

    pub fn save(
        &self,
        operations: Vec<OperationRecord>,
    ) -> Result<std::sync::mpsc::Receiver<Result<Vec<OperationRecord>, String>>, String> {
        let (reply, receiver) = std::sync::mpsc::channel();
        self.sender
            .send((operations, reply))
            .map_err(|_| "local copy state writer stopped".to_string())?;
        Ok(receiver)
    }

    pub fn checkpoint(
        &self,
        operations: Vec<OperationRecord>,
    ) -> Result<Vec<OperationRecord>, String> {
        self.save(operations)?
            .recv()
            .map_err(|_| "local copy state writer lost its checkpoint response".to_string())?
    }
}

pub fn ensure_private_root(path: &Path) -> io::Result<Arc<PinnedDirectory>> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "migration state path must be absolute",
        ));
    }
    let mut existing = path.to_path_buf();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                existing = existing
                    .parent()
                    .ok_or_else(|| io::Error::other("migration state has no existing ancestor"))?
                    .to_path_buf();
            }
            Err(error) => return Err(error),
        }
    }
    let mut current = PinnedDirectory::open_absolute(&existing)?;
    let missing = path
        .strip_prefix(&existing)
        .map_err(|_| io::Error::other("migration state ancestry changed"))?;
    for component in missing.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "migration state contains a non-normal component",
            ));
        };
        current = match metadata_nofollow(&current.file, Path::new(name)) {
            Ok(metadata) => current.open_child(name, &metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => current.create_child(name)?,
            Err(error) => return Err(error),
        };
    }
    current.validate_destination()?;
    destination_security::check_private_file(&current.file)?;
    Ok(current)
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use super::MigrationStore;
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::{Mutex, OnceLock, mpsc},
    };
    type Hook = (usize, mpsc::Sender<()>, mpsc::Receiver<()>);
    static HOOKS: OnceLock<Mutex<HashMap<PathBuf, Hook>>> = OnceLock::new();
    #[derive(Default)]
    pub struct OwnershipReleased(Mutex<Vec<mpsc::Sender<()>>>);

    impl Drop for OwnershipReleased {
        fn drop(&mut self) {
            for observer in self.0.get_mut().unwrap().drain(..) {
                let _ = observer.send(());
            }
        }
    }

    pub fn ownership_release(store: &MigrationStore) -> mpsc::Receiver<()> {
        let (sender, receiver) = mpsc::channel();
        store.ownership_released.0.lock().unwrap().push(sender);
        receiver
    }

    pub fn hold_save(path: &Path, nth: usize) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        HOOKS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap()
            .insert(path.to_owned(), (nth, entered_tx, release_rx));
        (entered_rx, release_tx)
    }
    pub fn before_save(path: &Path) {
        let hook = {
            let mut hooks = HOOKS.get_or_init(Mutex::default).lock().unwrap();
            if let Some((nth, _, _)) = hooks.get_mut(path) {
                *nth -= 1;
                if *nth == 0 { hooks.remove(path) } else { None }
            } else {
                None
            }
        };
        if let Some((_, entered, release)) = hook {
            let _ = entered.send(());
            tilo_test_env::recv("the save release", &release);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::migration::{CopyProgress, CopyReport};

    fn fixture() -> (tempfile::TempDir, MigrationStore, MigrationJob) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let state = base.join("state");
        let source = base.join("source");
        let destination_parent = base.join("destination-parent");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination_parent).unwrap();
        let store = MigrationStore::open_at(&state).unwrap();
        let job = MigrationJob::fresh(
            "op_1",
            source,
            destination_parent.join("Imported"),
            state.join("op_1.journal"),
        )
        .unwrap();
        (temp, store, job)
    }

    /// Issue details across attempts, compaction and paging.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn issue_details_and_pages_stay_attempt_scoped_and_bounded() {
        // Phase `later_attempt_with_same_counts_cannot_inherit_earlier_details`.
        {
            let (_temp, store, job) = fixture();
            let mut earlier = OperationRecord::new(&job);
            earlier.apply_outcome(WorkerOutcome::Incomplete(CopyReport {
                failed: (0..40)
                    .map(|n| (PathBuf::from(format!("file-{n}")), "old reason".into()))
                    .collect(),
                ..CopyReport::default()
            }));
            store.save(&[earlier]).unwrap();
            let saved = store.load().unwrap().remove(0);
            let mut later = OperationRecord::new(&job);
            later.apply_outcome(WorkerOutcome::Incomplete(CopyReport {
                failed: (0..40)
                    .map(|n| {
                        (
                            PathBuf::from(format!("file-{n}")),
                            if n < 8 { "old reason" } else { "new reason" }.into(),
                        )
                    })
                    .collect(),
                ..CopyReport::default()
            }));
            later.compact();
            later.retain_detail_reference(&saved);
            assert!(
                store.issue_page(&later, 0).is_err(),
                "a different attempt inherited an earlier issue page"
            );
        }

        // Phase `queued_legacy_compaction_cannot_lose_the_new_detail_reference`.
        {
            let (_temp, store, job) = fixture();
            let store = Arc::new(store);
            let writer = StoreWriter::new(Arc::clone(&store)).unwrap();
            let mut legacy = OperationRecord::new(&job);
            legacy.apply_outcome(WorkerOutcome::Incomplete(CopyReport {
                failed: (0..40)
                    .map(|n| (PathBuf::from(format!("legacy-{n}")), "reason".into()))
                    .collect(),
                ..CopyReport::default()
            }));
            let first = writer.save(vec![legacy.clone()]).unwrap();
            legacy.compact();
            let second = writer.save(vec![legacy]).unwrap();
            first.recv().unwrap().unwrap();
            second.recv().unwrap().unwrap();
            let saved = store.load().unwrap().remove(0);
            assert_eq!(store.issue_page(&saved, 0).unwrap().issues.len(), 32);
        }

        // Phase `saved_issue_pages_are_immutable_across_later_attempts`.
        {
            let (_temp, store, job) = fixture();
            let mut operation = OperationRecord::new(&job);
            operation.apply_outcome(WorkerOutcome::Incomplete(CopyReport {
                failed: (0..40)
                    .map(|n| (PathBuf::from(format!("old-{n}")), "old reason".into()))
                    .collect(),
                ..CopyReport::default()
            }));
            store.save(&[operation.clone()]).unwrap();
            let previous = store.load().unwrap().remove(0);
            operation.apply_outcome(WorkerOutcome::Incomplete(CopyReport {
                failed: (0..40)
                    .map(|n| (PathBuf::from(format!("new-{n}")), "new reason".into()))
                    .collect(),
                ..CopyReport::default()
            }));
            store.save(&[operation]).unwrap();
            assert_eq!(
                store.issue_page(&previous, 0).unwrap().issues[0].0,
                PathBuf::from("old-0")
            );
        }

        // Phase `every_issue_remains_available_in_bounded_pages_after_restart`.
        {
            let (_temp, store, job) = fixture();
            let mut operation = OperationRecord::new(&job);
            operation.apply_outcome(WorkerOutcome::Incomplete(CopyReport {
                failed: (0..1000)
                    .map(|n| (PathBuf::from(format!("file-{n}")), format!("reason-{n}")))
                    .collect(),
                scan_completed: true,
                ..CopyReport::default()
            }));
            store.save(&[operation]).unwrap();
            let reopened = store.load().unwrap().remove(0);
            assert_eq!(reopened.issue_count(), 1000);
            assert!(reopened.issues().unwrap().len() <= 8);
            assert!(fs::metadata(store.root().join(STORE_FILE)).unwrap().len() < 65536);
            let mut offset = 0;
            let mut count = 0;
            loop {
                let page = store.issue_page(&reopened, offset).unwrap();
                assert!(page.issues.len() <= 32);
                for (path, reason) in page.issues {
                    assert_eq!(path, PathBuf::from(format!("file-{count}")));
                    assert_eq!(reason, format!("reason-{count}"));
                    count += 1;
                }
                if let Some(next) = page.next {
                    offset = next;
                } else {
                    break;
                }
            }
            assert_eq!(count, 1000);
        }

        // Phase `index_does_not_duplicate_unbounded_verified_paths`.
        {
            let (_temp, store, job) = fixture();
            let mut operation = OperationRecord::new(&job);
            let report = CopyReport {
                verified_files: (0..10000)
                    .map(|n| PathBuf::from(format!("{n:08}-{}", "x".repeat(200))))
                    .collect(),
                scan_completed: true,
                ..CopyReport::default()
            };
            operation.apply_outcome(WorkerOutcome::Complete(report));
            store.save(&[operation]).unwrap();
            assert!(
                fs::metadata(store.root().join(STORE_FILE)).unwrap().len() < 65536,
                "full paths duplicated in index"
            );
            assert_eq!(store.load().unwrap()[0].verified_file_count(), 10000);
        }
    }

    #[test]
    fn store_lock_process_helper() {
        let Some(root) = std::env::var_os("TILO_COPY_STORE_LOCK_TEST") else {
            return;
        };
        let root = PathBuf::from(root);
        let _store = MigrationStore::open_at(&root).unwrap();
        fs::write(root.join("owner-ready"), b"ready").unwrap();
        loop {
            std::thread::park();
        }
    }

    /// Store lock ownership.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn store_ownership_is_exclusive_and_released_on_process_death() {
        // Phase `ownership_releases_after_process_death_without_removing_lock_file`.
        {
            let (temp, store, _job) = fixture();
            let root = store.root().to_owned();
            drop(store);
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    &format!(
                        "{}::store_lock_process_helper",
                        module_path!().split_once("::").unwrap().1
                    ),
                ])
                .env("TILO_COPY_STORE_LOCK_TEST", &root)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let deadline = tilo_test_env::hang_deadline();
            while !root.join("owner-ready").exists() {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "child did not acquire store lock in {}",
                        temp.path().display()
                    );
                }
                std::thread::yield_now();
            }
            assert!(MigrationStore::open_at(&root).is_err());
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(root.join("ownership.lock").is_file());
            assert!(MigrationStore::open_at(&root).is_ok());
        }

        // Phase `second_owner_cannot_load_or_normalize_live_operation`.
        {
            let (_temp, store, job) = fixture();
            let mut operation = OperationRecord::new(&job);
            operation.approve();
            operation.mark_running().unwrap();
            store.save(&[operation]).unwrap();
            let before = fs::read(store.root().join(STORE_FILE)).unwrap();
            assert!(
                MigrationStore::open_at(store.root()).is_err(),
                "second owner acquired live store"
            );
            assert_eq!(fs::read(store.root().join(STORE_FILE)).unwrap(), before);
            let root = store.root().to_owned();
            drop(store);
            let reopened = MigrationStore::open_at(&root).unwrap();
            assert_eq!(
                reopened.load().unwrap()[0].state(),
                OperationState::Interrupted
            );
        }
    }

    /// Interrupted reopen, grants, incomplete reports, retries and native names.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn operation_records_reopen_review_and_round_trip_truthfully() {
        // Phase `running_operation_reopens_as_interrupted`.
        // Catches treating an operation index's stale Running marker as evidence
        // that work completed while the application was closed.
        {
            let (_temp, store, job) = fixture();
            let mut operation = OperationRecord::new(&job);
            operation.approve();
            operation.mark_running().unwrap();
            store.save(&[operation]).unwrap();

            let reopened = store.load().unwrap();
            assert_eq!(reopened.len(), 1);
            assert_eq!(reopened[0].state(), OperationState::Interrupted);
            assert_ne!(reopened[0].state(), OperationState::Complete);
        }

        // Phase `changed_grants_require_a_fresh_review`.
        // Catches changing a reviewed root while retaining approval for the old
        // source/destination pair.
        {
            let (temp, _store, job) = fixture();
            let mut operation = OperationRecord::new(&job);
            operation.approve();
            assert!(operation.start_job().is_ok());

            let changed = MigrationJob::fresh(
                "op_1",
                job.source().to_path_buf(),
                temp.path()
                    .canonicalize()
                    .unwrap()
                    .join("somewhere-else/Imported"),
                job.journal().to_path_buf(),
            )
            .unwrap();
            let changed_operation = OperationRecord::new(&changed);

            assert_eq!(changed_operation.state(), OperationState::AwaitingReview);
            assert!(changed_operation.start_job().is_err());
        }

        // Phase `incomplete_report_round_trips_with_paths_and_disclosures`.
        // Catches loss of terminal detail needed to distinguish partial failure
        // from verified completion after restart.
        {
            let (_temp, store, job) = fixture();
            let mut operation = OperationRecord::new(&job);
            operation.approve();
            operation.mark_running().unwrap();
            operation.apply_progress(&CopyProgress {
                verified_files: 1,
                verified_bytes: 4,
                current_relative_path: "ok.txt".into(),
            });
            operation.apply_outcome(WorkerOutcome::Incomplete(CopyReport {
                verified_files: vec!["ok.txt".into()],
                verified_bytes: 4,
                skipped: vec![("linked".into(), "symbolic link".into())],
                failed: vec![("private".into(), "permission denied".into())],
                cancelled: false,
                scan_completed: true,
                metadata_limitations: vec!["ACLs are not preserved".into()],
            }));
            store.save(&[operation]).unwrap();

            let reopened = store.load().unwrap().remove(0);
            assert_eq!(reopened.state(), OperationState::Incomplete);
            assert_eq!(reopened.verified_bytes(), 4);
            assert_eq!(
                reopened.issues().unwrap(),
                vec![
                    ("linked".into(), "symbolic link".into()),
                    ("private".into(), "permission denied".into())
                ]
            );
            assert_eq!(reopened.metadata_limitations(), &["ACLs are not preserved"]);
        }

        // Phase `retry_progress_replaces_the_prior_incomplete_attempt`.
        // Catches a retry continuing to display the previous attempt's counts,
        // issues and current path after the new worker reports fresh progress.
        {
            let (_temp, _store, job) = fixture();
            let mut operation = OperationRecord::new(&job);
            operation.approve();
            operation.mark_running().unwrap();
            operation.apply_progress(&CopyProgress {
                verified_files: 7,
                verified_bytes: 70,
                current_relative_path: "old.txt".into(),
            });
            operation.apply_outcome(WorkerOutcome::Incomplete(CopyReport {
                verified_files: vec!["old.txt".into()],
                verified_bytes: 70,
                skipped: vec![("old-link".into(), "symbolic link".into())],
                failed: vec![("old-private".into(), "permission denied".into())],
                cancelled: false,
                scan_completed: true,
                metadata_limitations: vec!["old limitation".into()],
            }));

            operation.mark_running().unwrap();
            operation.apply_progress(&CopyProgress {
                verified_files: 2,
                verified_bytes: 12,
                current_relative_path: "new.txt".into(),
            });

            assert_eq!(operation.state(), OperationState::Running);
            assert_eq!(operation.verified_file_count(), 2);
            assert_eq!(operation.verified_bytes(), 12);
            assert_eq!(
                operation.current_relative_path().unwrap(),
                Some("new.txt".into())
            );
            assert!(operation.issues().unwrap().is_empty());
            assert!(operation.metadata_limitations().is_empty());
        }

        // Phase `native_non_utf8_grants_round_trip_losslessly`.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            let (temp, store, _job) = fixture();
            let base = temp.path().canonicalize().unwrap();
            let source = base.join(std::ffi::OsString::from_vec(vec![b's', 0xff]));
            let job = MigrationJob::fresh(
                "native",
                source.clone(),
                base.join("destination-parent/native"),
                store.root().join("native.journal"),
            )
            .unwrap();
            store.save(&[OperationRecord::new(&job)]).unwrap();

            assert_eq!(store.load().unwrap()[0].source().unwrap(), source);
        }

        // Phase `creating_private_state_does_not_chmod_existing_parent`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let temp = tempfile::tempdir().unwrap();
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o751)).unwrap();
            let before = fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777;
            let parent = temp.path().canonicalize().unwrap();
            let store = MigrationStore::open_at(&parent.join("migration-state")).unwrap();
            let after = fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777;

            assert_eq!(before, after);
            assert_eq!(
                fs::metadata(store.root()).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
