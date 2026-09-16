//! Private device-local index for migration operations.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::OpenOptionsFollowExt;
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", content = "units", deny_unknown_fields)]
enum NativePath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl NativePath {
    fn encode(path: &Path) -> Self {
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

    fn decode(&self) -> Result<PathBuf, String> {
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
pub(super) enum OperationState {
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
    verified_files: Vec<NativePath>,
    verified_bytes: u64,
    skipped: Vec<StoredIssue>,
    failed: Vec<StoredIssue>,
    cancelled: bool,
    scan_completed: bool,
    metadata_limitations: Vec<String>,
}

impl StoredReport {
    fn from_report(report: CopyReport) -> Self {
        Self {
            verified_files: report
                .verified_files
                .iter()
                .map(|path| NativePath::encode(path))
                .collect(),
            verified_bytes: report.verified_bytes,
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
pub(super) struct OperationRecord {
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
    pub(super) fn new(job: &MigrationJob) -> Self {
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

    pub(super) fn source(&self) -> Result<PathBuf, String> {
        self.source.decode()
    }

    pub(super) fn destination(&self) -> Result<PathBuf, String> {
        self.destination.decode()
    }

    pub(super) fn journal(&self) -> Result<PathBuf, String> {
        self.journal.decode()
    }

    #[must_use]
    pub(super) fn state(&self) -> OperationState {
        self.state
    }

    #[must_use]
    pub(super) fn verified_bytes(&self) -> u64 {
        self.report.as_ref().map_or_else(
            || self.progress.as_ref().map_or(0, |p| p.verified_bytes),
            |r| r.verified_bytes,
        )
    }

    #[must_use]
    pub(super) fn verified_file_count(&self) -> usize {
        self.report.as_ref().map_or_else(
            || {
                self.progress
                    .as_ref()
                    .map_or(0, |p| p.verified_files as usize)
            },
            |r| r.verified_files.len(),
        )
    }

    pub(super) fn current_relative_path(&self) -> Result<Option<PathBuf>, String> {
        self.progress
            .as_ref()
            .map(|progress| progress.current_relative_path.decode())
            .transpose()
    }

    pub(super) fn issues(&self) -> Result<Vec<(PathBuf, String)>, String> {
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
    pub(super) fn metadata_limitations(&self) -> &[String] {
        self.report
            .as_ref()
            .map_or(&[], |report| report.metadata_limitations.as_slice())
    }

    #[must_use]
    pub(super) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub(super) fn approve(&mut self) {
        self.approved_revision = Some(self.review_revision);
        self.state = OperationState::Ready;
    }

    pub(super) fn start_job(&self) -> Result<MigrationJob, String> {
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

    pub(super) fn retry_job(&self) -> Result<MigrationJob, String> {
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

    pub(super) fn mark_running(&mut self) -> Result<(), String> {
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
        self.failure = None;
        Ok(())
    }

    pub(super) fn apply_progress(&mut self, progress: &CopyProgress) {
        if self.state == OperationState::Running {
            self.progress = Some(StoredProgress {
                verified_files: progress.verified_files,
                verified_bytes: progress.verified_bytes,
                current_relative_path: NativePath::encode(&progress.current_relative_path),
            });
        }
    }

    pub(super) fn apply_outcome(&mut self, outcome: WorkerOutcome) {
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
pub(super) struct MigrationStore {
    root_path: PathBuf,
    root: Arc<PinnedDirectory>,
}

impl MigrationStore {
    pub(super) fn open_default() -> Result<Self, String> {
        let root = dirs::data_local_dir()
            .ok_or_else(|| "no private local data directory is available".to_string())?
            .join("qos-imager")
            .join("migrations");
        Self::open_at(&root)
    }

    pub(super) fn open_at(path: &Path) -> Result<Self, String> {
        let root = ensure_private_root(path).map_err(|error| {
            format!(
                "private migration state is unavailable at {}: {error}",
                path.display()
            )
        })?;
        Ok(Self {
            root_path: path.to_path_buf(),
            root,
        })
    }

    #[must_use]
    pub(super) fn root(&self) -> &Path {
        &self.root_path
    }

    pub(super) fn load(&self) -> Result<Vec<OperationRecord>, String> {
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
            operation.source()?;
            operation.destination()?;
            operation.journal()?;
            if operation.state == OperationState::Running {
                operation.state = OperationState::Interrupted;
            }
        }
        Ok(document.operations)
    }

    pub(super) fn save(&self, operations: &[OperationRecord]) -> Result<(), String> {
        if operations.len() > MAX_OPERATIONS {
            return Err("too many migration operations to persist".into());
        }
        self.root
            .validate_destination()
            .map_err(|e| e.to_string())?;
        destination_security::check_private_file(&self.root.file).map_err(|e| e.to_string())?;
        let document = StoreDocument {
            version: 1,
            operations: operations.to_vec(),
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
        migration_durability::sync_directory(&self.root.file).map_err(|e| e.to_string())
    }
}

fn ensure_private_root(path: &Path) -> io::Result<Arc<PinnedDirectory>> {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::setup::migration::{CopyProgress, CopyReport};

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

    // Catches treating an operation index's stale Running marker as evidence
    // that work completed while the application was closed.
    #[test]
    fn running_operation_reopens_as_interrupted() {
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

    // Catches changing a reviewed root while retaining approval for the old
    // source/destination pair.
    #[test]
    fn changed_grants_require_a_fresh_review() {
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

    // Catches loss of terminal detail needed to distinguish partial failure
    // from verified completion after restart.
    #[test]
    fn incomplete_report_round_trips_with_paths_and_disclosures() {
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

    #[cfg(unix)]
    #[test]
    fn native_non_utf8_grants_round_trip_losslessly() {
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

    #[cfg(unix)]
    #[test]
    fn creating_private_state_does_not_chmod_existing_parent() {
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
