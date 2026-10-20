//! Existing capability provider for an independently admitted native directory.
//! Names are raw components. Mutation jobs retain the canonical durable owner.
use crate::directory_backend::{
    DirectoryRow, DirectoryRows, DirectoryStat, JobPoll, JobUpdate, WorkspaceDirectoryBackend,
    WorkspaceJobBackend, WorkspaceMetadataBackend,
};
use crate::operations::FsUpdate;
use crate::operations::{ConflictPolicy, FsJob, FsOperation};
use crate::workspace_server::WorkspaceFileBackend;
use crate::{
    DirectoryGrant, DirectoryPage, EntryName, FsError, FsSession, GrantedEntry, Metadata,
    NativeFileAccess, OpenMode, PageCursor,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// Opaque native directory custody; cannot be recovered from a selected file.
pub struct DirectoryCapability {
    session: FsSession,
    root: DirectoryGrant,
    cursor: Mutex<(u64, Option<(u64, PageCursor)>)>,
}
impl DirectoryCapability {
    /// # Errors
    /// Refuses a stale, foreign or non-enumerable actual directory grant.
    pub fn new(session: FsSession, root: DirectoryGrant) -> Result<Self, FsError> {
        session.check_directory(&root)?;
        Ok(Self {
            session,
            root,
            cursor: Mutex::new((0, None)),
        })
    }
    pub(crate) fn check(&self) -> Result<(), FsError> {
        self.session.check_directory(&self.root)
    }
    /// # Errors
    /// Returns canonical live scope or native metadata errors.
    pub fn metadata(&self) -> Result<Metadata, FsError> {
        self.session
            .metadata(&GrantedEntry::Directory(self.root.clone()))
    }
    /// # Errors
    /// Returns Busy rather than truncating a snapshot, or native live/cursor errors.
    pub fn list(&self, cursor: Option<PageCursor>) -> Result<DirectoryPage, FsError> {
        self.session.list(&self.root, cursor)
    }
    /// # Errors
    /// Refuses non-directory children and canonical live/name/native errors.
    pub fn directory(&self, name: &EntryName) -> Result<Arc<Self>, FsError> {
        let GrantedEntry::Directory(root) = self.session.lookup(&self.root, name)? else {
            return Err(FsError::Unsupported("directory child required"));
        };
        Ok(Arc::new(Self::new(self.session.clone(), root)?))
    }
    /// Open only an existing no-follow regular child. Write truncates; Append
    /// atomically appends; `ReadWriteExisting` and `WriteExisting` preserve length. Missing files are never
    /// silently created: creation must have an actual durable native job.
    /// # Errors
    /// Refuses absent rights, wrong type, substituted inode, stale scope or IO.
    pub fn open_file(
        &self,
        name: &EntryName,
        mode: OpenMode,
    ) -> Result<Arc<dyn WorkspaceFileBackend>, String> {
        let observed = self
            .session
            .stat_child(&self.root, name)
            .map_err(|e| e.to_string())?;
        self.open_expected(name, mode, observed.identity)
    }
    fn open_expected(
        &self,
        name: &EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceFileBackend>, String> {
        let access = match mode {
            OpenMode::Read => NativeFileAccess::Read,
            OpenMode::Write | OpenMode::Append | OpenMode::WriteExisting => NativeFileAccess::Write,
            OpenMode::ReadWriteExisting => NativeFileAccess::ReadWrite,
        };
        let file = self
            .session
            .lookup_file_for(&self.root, name, access)
            .map_err(|e| e.to_string())?;
        let actual = self
            .session
            .entry_identity(&GrantedEntry::File(file.clone()))
            .map_err(|e| e.to_string())?;
        if actual != expected {
            return Err("native inode changed before open effect".into());
        }
        crate::selected_file::open_granted(self.session.clone(), file, mode)
    }
    /// Create an empty child through a canonical durable job; never overwrites.
    /// # Errors
    /// Returns absent operation-store, queue, live authority or job admission errors.
    pub fn create_file(&self, name: EntryName) -> Result<FsJob, FsError> {
        self.check()?;
        self.session.start(FsOperation::CreateFile {
            parent: self.root.clone(),
            name,
            bytes: Vec::new(),
            conflict: ConflictPolicy::Fail,
        })
    }
    /// # Errors
    /// Returns canonical durable-job admission errors; collisions are job failures.
    pub fn create_directory(&self, name: EntryName) -> Result<FsJob, FsError> {
        self.check()?;
        self.session.start(FsOperation::CreateDirectory {
            parent: self.root.clone(),
            name,
            conflict: ConflictPolicy::Fail,
        })
    }

    /// # Errors
    /// Requires current enumerable parent and its actual protected native owner.
    pub fn recoveries(
        &self,
        after: &[u8],
    ) -> Result<crate::directory_backend::RecoveryPage, FsError> {
        let scope = self.session.recovery_scope(&self.root)?;
        let page = self.session.operation_store()?.scoped_page(&scope, after)?;
        self.check()?;
        Ok(page)
    }
    /// # Errors
    /// Resolves only this held parent journal scope and live original grants.
    pub fn restore_captured(&self, recovery: [u8; 16], name: EntryName) -> Result<FsJob, FsError> {
        let owner = self.session.operation_store()?;
        let id = crate::recovery::RecoveryId::from_bytes(recovery);
        let (_, saved) = owner.find_recovery(&id)?;
        let source = saved.source.as_ref().ok_or(FsError::Denied)?;
        let scope = self.session.recovery_scope(&self.root)?;
        if source.root != scope.root || source.parents != scope.parents {
            return Err(FsError::Denied);
        }
        owner.resolve(
            &id,
            &self.session,
            crate::recovery::RecoveryResolution::RestoreCaptured {
                parent: self.root.clone(),
                name,
            },
        )
    }
    /// Raw retained-directory removal remains unsupported; selected temporary
    /// cleanup uses its separate genuine created-temporary capability.
    /// # Errors
    /// Always refuses without admitting a namespace effect or Purge authority.
    pub fn remove(
        &self,
        _name: &EntryName,
        _expected: [u64; 2],
        _directory: bool,
    ) -> Result<FsJob, FsError> {
        Err(FsError::Unsupported("retained directory removal"))
    }
    /// Same-parent rename with NOREPLACE. This is not POSIX replacing rename or
    /// an atomic-save promise. Failure/recovery belongs to the durable native job.
    /// # Errors
    /// Returns canonical lookup/admission errors; effects have typed job outcomes.
    pub fn rename_no_replace(
        &self,
        name: &EntryName,
        new_name: EntryName,
    ) -> Result<FsJob, FsError> {
        let entry = self.session.lookup(&self.root, name)?;
        self.session.start(FsOperation::Rename {
            entry,
            name: new_name,
        })
    }
}

#[derive(Default)]
struct NativeTerminal {
    item: Option<JobUpdate>,
    finished: Option<JobUpdate>,
    refused: bool,
}
impl NativeTerminal {
    fn record(&mut self, updates: &[JobUpdate]) {
        if self.refused {
            return;
        }
        for update in updates {
            match update {
                JobUpdate::Progress { .. } => {}
                JobUpdate::Item { item: 0, .. }
                    if self.item.is_none() && self.finished.is_none() =>
                {
                    self.item = Some(update.clone());
                }
                JobUpdate::Finished { .. } if self.item.is_some() && self.finished.is_none() => {
                    self.finished = Some(update.clone());
                }
                _ => {
                    self.refused = true;
                    self.item = None;
                    self.finished = None;
                    return;
                }
            }
        }
    }
    fn completed(&self, operation_id: [u8; 16]) -> Result<JobPoll, String> {
        let (Some(item), Some(finished)) = (&self.item, &self.finished) else {
            return Err("Actual successful native terminal updates unavailable".into());
        };
        if self.refused
            || !matches!(
                item,
                JobUpdate::Item {
                    item: 0,
                    state: crate::operations::ItemState::Completed,
                    destination: Some(_),
                    error: None,
                    ..
                }
            )
            || !matches!(
                finished,
                JobUpdate::Finished {
                    completed: 1,
                    failed: 0,
                    cancelled: 0
                }
            )
        {
            return Err("Actual successful native terminal updates unavailable".into());
        }
        Ok(JobPoll {
            operation_id,
            updates: vec![item.clone(), finished.clone()],
        })
    }
}
struct NativeJob {
    created: Mutex<Option<Arc<CreatedTemporary>>>,
    directory: DirectoryCapability,
    job: Mutex<FsJob>,
    replacement_pin: Mutex<Option<std::sync::Weak<ReplacementPin>>>,
    terminal: Mutex<NativeTerminal>,
}
impl DirectoryCapability {
    #[cfg(test)]
    pub(crate) fn pending_for_transport_test(
        &self,
        prepared: crate::operations::PreparedNativeReplacement,
    ) -> Result<Arc<dyn crate::directory_backend::WorkspacePendingReplacementBackend>, String> {
        let directory = Self::new(self.session.clone(), self.root.clone())
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(PendingReplacement::new(directory, prepared)?))
    }
    #[cfg(test)]
    pub(crate) fn poisoned_pending_for_transport_test(
        &self,
        prepared: crate::operations::PreparedNativeReplacement,
    ) -> Result<Arc<dyn crate::directory_backend::WorkspacePendingReplacementBackend>, String> {
        let directory = Self::new(self.session.clone(), self.root.clone())
            .map_err(|error| error.to_string())?;
        let pending = Arc::new(PendingReplacement::new(directory, prepared)?);
        Self::poison_pending_state_for_transport_test(&pending);
        Ok(pending)
    }
    #[cfg(test)]
    fn poison_pending_state_for_transport_test(pending: &PendingReplacement) {
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = pending.state.lock().unwrap();
            panic!("intentionally poison this actual native pending state for custody regression");
        }));
        assert!(poisoned.is_err());
    }
    #[cfg(test)]
    pub(crate) fn job_for_transport_test(
        &self,
        job: FsJob,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        self.retain_job(job)
    }
    fn retain_job(&self, job: FsJob) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let directory =
            Self::new(self.session.clone(), self.root.clone()).map_err(|e| e.to_string())?;
        Ok(Arc::new(NativeJob {
            directory,
            job: Mutex::new(job),
            created: Mutex::new(None),
            replacement_pin: Mutex::new(None),
            terminal: Mutex::new(NativeTerminal::default()),
        }))
    }
}
impl WorkspaceDirectoryBackend for DirectoryCapability {
    fn history_read_subject(
        &self,
        name: &EntryName,
    ) -> Result<Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>, String> {
        let subject = self
            .session
            .history_read_subject(&self.root, name)
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HistoryReadSubject { subject }))
    }

    fn sync_directory(&self) -> Result<(), String> {
        self.session
            .sync_directory(&self.root)
            .map_err(|error| error.to_string())
    }

    fn prepare_replace(
        &self,
        _: &EntryName,
        _: [u64; 2],
        _: &EntryName,
        _: [u64; 2],
    ) -> Result<Arc<dyn crate::directory_backend::WorkspacePendingReplacementBackend>, String> {
        self.check().map_err(|error| error.to_string())?;
        Err("Replacement requires actual created-temporary custody".into())
    }
    fn statfs(&self) -> Result<crate::directory_backend::DirectoryFilesystemStats, String> {
        self.session
            .directory_statfs(&self.root)
            .map_err(|e| e.to_string())
    }
    fn open_cursor(
        &self,
    ) -> Result<Arc<dyn crate::directory_backend::WorkspaceDirectoryCursorBackend>, String> {
        Ok(Arc::new(
            self.session
                .open_directory_cursor(&self.root)
                .map_err(|e| e.to_string())?,
        ))
    }
    fn check(&self) -> Result<(), String> {
        self.check().map_err(|e| e.to_string())
    }
    fn metadata(&self) -> Result<DirectoryStat, String> {
        self.session
            .entry_stat(&GrantedEntry::Directory(self.root.clone()))
            .map_err(|e| e.to_string())
    }

    fn stat(&self, name: &EntryName) -> Result<Option<DirectoryStat>, String> {
        match self.session.stat_child(&self.root, name) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(FsError::NotFound) => {
                self.check().map_err(|e| e.to_string())?;
                Ok(None)
            }
            Err(error) => Err(error.to_string()),
        }
    }
    fn readlink(&self, name: &EntryName, expected: [u64; 2]) -> Result<Vec<u8>, String> {
        self.session
            .readlink_child(&self.root, name, expected)
            .map_err(|e| e.to_string())
    }
    fn pin_metadata(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceMetadataBackend>, String> {
        let entry = self
            .session
            .lookup(&self.root, name)
            .map_err(|e| e.to_string())?;
        let stat = self.session.entry_stat(&entry).map_err(|e| e.to_string())?;
        if stat.metadata.kind != crate::EntryKind::File || stat.identity != expected {
            return Err("metadata pin identity/type differs".into());
        }
        Ok(Arc::new(MetadataPin {
            session: self.session.clone(),
            entry,
        }))
    }
    fn list(&self, token: u64) -> Result<DirectoryRows, String> {
        let mut state = self
            .cursor
            .lock()
            .map_err(|_| "directory cursor lock poisoned".to_string())?;
        let position = if token == 0 {
            None
        } else {
            let (expected, position) = state.1.as_ref().ok_or("stale directory cursor")?;
            if *expected != token {
                return Err("foreign directory cursor".into());
            }
            Some(position.clone())
        };
        let page = self.list(position).map_err(|e| e.to_string())?;
        state.0 = state.0.checked_add(1).ok_or("cursor sequence exhausted")?;
        let next = if let Some(position) = page.next {
            let next = state.0;
            state.1 = Some((next, position));
            next
        } else {
            state.1 = None;
            0
        };
        let rows = DirectoryRows {
            entries: page
                .entries
                .into_iter()
                .map(|entry| DirectoryRow {
                    name: entry.name,
                    metadata: entry.metadata,
                })
                .collect(),
            next,
        };
        drop(state);
        Ok(rows)
    }
    fn directory(&self, name: &EntryName) -> Result<Arc<dyn WorkspaceDirectoryBackend>, String> {
        Ok(self.directory(name).map_err(|e| e.to_string())?)
    }
    fn open_file(
        &self,
        name: &EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceFileBackend>, String> {
        self.open_expected(name, mode, expected)
    }
    fn create_file(&self, name: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        self.retain_job(self.create_file(name).map_err(|e| e.to_string())?)
    }
    fn create_directory(&self, name: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        self.retain_job(self.create_directory(name).map_err(|e| e.to_string())?)
    }

    fn recoveries(&self, after: &[u8]) -> Result<crate::directory_backend::RecoveryPage, String> {
        self.recoveries(after).map_err(|error| error.to_string())
    }
    fn restore_captured(
        &self,
        recovery: [u8; 16],
        name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        self.retain_job(
            self.restore_captured(recovery, name)
                .map_err(|error| error.to_string())?,
        )
    }
    fn rename_no_replace(
        &self,
        name: &EntryName,
        new_name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        self.retain_job(
            self.rename_no_replace(name, new_name)
                .map_err(|e| e.to_string())?,
        )
    }
}
struct ReplacementPin {
    directory: DirectoryCapability,
    pin: crate::recovery::NativeReplacementPin,
}
impl crate::directory_backend::WorkspaceReplacementPinBackend for ReplacementPin {
    fn commit_with_history(&self, bytes: &[u8]) -> Result<(), String> {
        let receipt = crate::external_history::CommittedReceipt::decode(bytes)
            .map_err(|error| error.to_string())?;
        self.pin
            .commit_with_history(&self.directory.session, &self.directory.root, &receipt)
            .map_err(|error| error.to_string())
    }

    fn check(&self) -> Result<(), String> {
        self.pin
            .check(&self.directory.session, &self.directory.root)
            .map_err(|e| e.to_string())
    }
    fn observe(&self) -> Result<crate::directory_backend::ReplacementObservation, String> {
        self.check()?;
        let value = crate::directory_backend::ReplacementObservation {
            operation: *self.pin.operation_id().as_bytes(),
            source_owner: self.pin.source_owner(),
            capture: *self.pin.capture_id().as_bytes(),
            source: self.pin.source_identity(),
            victim: self.pin.victim_identity(),
            destination: self.pin.destination_identity(),
        };
        self.check()?;
        Ok(value)
    }
}
impl NativeJob {
    fn retained_replacement_pin(&self) -> Result<Arc<ReplacementPin>, String> {
        self.directory.check().map_err(|e| e.to_string())?;
        let mut cached = self
            .replacement_pin
            .lock()
            .map_err(|_| "replacement pin lock poisoned")?;
        if let Some(pin) = cached.as_ref().and_then(std::sync::Weak::upgrade) {
            crate::directory_backend::WorkspaceReplacementPinBackend::check(pin.as_ref())?;
            return Ok(pin);
        }
        let operation = self
            .job
            .lock()
            .map_err(|_| "native job lock poisoned")?
            .operation_id();
        let owner = self
            .directory
            .session
            .operation_store()
            .map_err(|e| e.to_string())?;
        let pin = owner
            .pin_replacement(&self.directory.session, &operation, &self.directory.root)
            .map_err(|e| e.to_string())?;
        let directory =
            DirectoryCapability::new(self.directory.session.clone(), self.directory.root.clone())
                .map_err(|e| e.to_string())?;
        let pin = Arc::new(ReplacementPin { directory, pin });
        crate::directory_backend::WorkspaceReplacementPinBackend::check(pin.as_ref())?;
        *cached = Some(Arc::downgrade(&pin));
        drop(cached);
        Ok(pin)
    }
}
struct ReceiptClient(Arc<AtomicUsize>);
impl Drop for ReceiptClient {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct AcknowledgedJob {
    _pin: Arc<dyn crate::directory_backend::WorkspaceReplacementPinBackend>,
    _client: ReceiptClient,
    native: Arc<NativeJob>,
    terminal: Mutex<Option<JobPoll>>,
    operation: [u8; 16],
}
impl WorkspaceJobBackend for AcknowledgedJob {
    fn live_scope(&self) -> Option<Result<(), String>> {
        self.native.live_scope()
    }
    fn pin_replacement(
        &self,
    ) -> Result<Arc<dyn crate::directory_backend::WorkspaceReplacementPinBackend>, String> {
        self.native.pin_replacement()
    }
    fn poll(&self) -> Result<JobPoll, String> {
        self.native
            .live_scope()
            .ok_or("Missing native job scope")??;
        let mut terminal = self
            .terminal
            .lock()
            .map_err(|_| "Acknowledged job lock poisoned")?;
        self.native
            .live_scope()
            .ok_or("Missing native job scope")??;
        Ok(terminal.take().unwrap_or(JobPoll {
            operation_id: self.operation,
            updates: vec![],
        }))
    }
    fn cancel(&self) -> Result<(), String> {
        Err("Acknowledged native replacement is already committed".into())
    }
}
impl WorkspaceJobBackend for NativeJob {
    fn created_temporary(
        &self,
    ) -> Result<Arc<dyn crate::directory_backend::WorkspaceCreatedTemporaryBackend>, String> {
        let mut cached = self
            .created
            .lock()
            .map_err(|_| "Created custody lock poisoned")?;
        self.directory.check().map_err(|error| error.to_string())?;
        if let Some(value) = cached.as_ref() {
            crate::directory_backend::WorkspaceCreatedTemporaryBackend::check(value.as_ref())?;
            return Ok(value.clone());
        }
        self.directory.check().map_err(|error| error.to_string())?;
        let native = self
            .job
            .lock()
            .map_err(|_| "Native job lock poisoned")?
            .created_temporary()
            .map_err(|error| error.to_string())?;
        let directory =
            DirectoryCapability::new(self.directory.session.clone(), self.directory.root.clone())
                .map_err(|error| error.to_string())?;
        let value = CreatedTemporary {
            directory,
            native,
            atomic: Mutex::new(AtomicReservation::Empty),
        };
        let value = Arc::new(value);
        *cached = Some(value.clone());
        crate::directory_backend::WorkspaceCreatedTemporaryBackend::check(value.as_ref())?;
        drop(cached);
        Ok(value)
    }

    fn pin_replacement(
        &self,
    ) -> Result<Arc<dyn crate::directory_backend::WorkspaceReplacementPinBackend>, String> {
        Ok(self.retained_replacement_pin()?)
    }
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.directory.check().map_err(|e| e.to_string()))
    }
    fn poll(&self) -> Result<JobPoll, String> {
        self.directory.check().map_err(|e| e.to_string())?;
        let mut job = self
            .job
            .lock()
            .map_err(|_| "native job lock poisoned".to_string())?;
        let operation_id = *job.operation_id().as_bytes();
        let updates = job
            .poll()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|update| {
                Ok(match update {
                    FsUpdate::Progress {
                        sequence,
                        item,
                        phase,
                        bytes,
                    } => JobUpdate::Progress {
                        sequence,
                        item,
                        phase: phase.into(),
                        bytes,
                    },
                    FsUpdate::Item(item) => JobUpdate::Item {
                        item: item.item,
                        state: item.state,
                        destination: item
                            .destination
                            .as_ref()
                            .map(|entry| self.directory.session.entry_identity(entry))
                            .transpose()
                            .map_err(|e| e.to_string())?,
                        error: item.error.map(|e| e.to_string()),
                        recoveries: item
                            .recoveries
                            .into_iter()
                            .map(|id| *id.as_bytes())
                            .collect(),
                    },
                    FsUpdate::Finished {
                        completed,
                        failed,
                        cancelled,
                    } => JobUpdate::Finished {
                        completed,
                        failed,
                        cancelled,
                    },
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        // Bounded actual-source record of Item+Finished observed while the daemon
        // drains this exact job. No success is synthesized from signed labels.
        self.terminal
            .lock()
            .map_err(|_| "Native terminal lock poisoned")?
            .record(&updates);
        drop(job);
        Ok(JobPoll {
            operation_id,
            updates,
        })
    }
    fn cancel(&self) -> Result<(), String> {
        self.directory.check().map_err(|e| e.to_string())?;
        self.job
            .lock()
            .map_err(|_| "native job lock poisoned".to_string())?
            .request_cancel();
        Ok(())
    }
}

/// Existing workspace bootstrap for independently admitted directory custody.
/// Text-path operations are refused; consumers use the raw-component capability.
pub struct DirectoryWorkspaceBackend(Arc<DirectoryCapability>);
impl DirectoryWorkspaceBackend {
    /// # Errors
    /// Returns the actual session/grant admission error.
    pub fn new(session: FsSession, root: DirectoryGrant) -> Result<Self, FsError> {
        Ok(Self(Arc::new(DirectoryCapability::new(session, root)?)))
    }
}
impl crate::workspace_server::WorkspaceBackend for DirectoryWorkspaceBackend {
    fn directory(&self) -> Result<Option<Arc<dyn WorkspaceDirectoryBackend>>, String> {
        self.0.check().map_err(|e| e.to_string())?;
        Ok(Some(self.0.clone()))
    }
    fn list(&self, _: &str) -> Result<Vec<crate::DirEntry>, String> {
        Err("raw directory capability required".into())
    }
    fn stat(&self, _: &str) -> Result<Metadata, String> {
        Err("raw directory capability required".into())
    }
    fn read(&self, _: &str, _: u64, _: u32) -> Result<Vec<u8>, String> {
        Err("raw directory capability required".into())
    }
}

struct MetadataPin {
    session: FsSession,
    entry: GrantedEntry,
}
impl WorkspaceMetadataBackend for MetadataPin {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(
            self.session
                .check_entry(&self.entry)
                .map_err(|e| e.to_string()),
        )
    }
    fn metadata(&self) -> Result<DirectoryStat, String> {
        self.session
            .entry_stat(&self.entry)
            .map_err(|e| e.to_string())
    }
}

impl crate::directory_backend::WorkspaceDirectoryCursorBackend for crate::FsDirectoryCursor {
    fn check(&self) -> Result<(), String> {
        self.check().map_err(|e| e.to_string())
    }
    fn page(&self, expected: u64) -> Result<crate::directory_backend::DirectoryStreamPage, String> {
        self.page(expected).map_err(|e| e.to_string())
    }
}

struct PendingState {
    prepared: Option<crate::operations::PreparedNativeReplacement>,
    // The actual submitted job, not a job reconstructed from signed metadata.
    submitted: Option<Arc<NativeJob>>,
}
struct PendingReplacement {
    receipt_clients: Arc<AtomicUsize>,
    directory: DirectoryCapability,
    observation: crate::directory_backend::PendingReplacementObservation,
    state: Mutex<PendingState>,
}
impl PendingReplacement {
    fn new(
        directory: DirectoryCapability,
        prepared: crate::operations::PreparedNativeReplacement,
    ) -> Result<Self, String> {
        let observation = Self::observe_prepared(&prepared)?;
        directory.check().map_err(|error| error.to_string())?;
        Ok(Self::with_observation(directory, prepared, observation))
    }
    fn observe_prepared(
        prepared: &crate::operations::PreparedNativeReplacement,
    ) -> Result<crate::directory_backend::PendingReplacementObservation, String> {
        let observation = crate::directory_backend::PendingReplacementObservation {
            purpose: match prepared.purpose().map_err(|e| e.to_string())? {
                crate::operations::NativeReplacementPurpose::ReplacementRename => {
                    crate::directory_backend::ReplacementPurpose::ReplacementRename
                }
                crate::operations::NativeReplacementPurpose::AtomicSave => {
                    crate::directory_backend::ReplacementPurpose::AtomicSave
                }
            },
            operation: *prepared
                .operation_id()
                .map_err(|e| e.to_string())?
                .as_bytes(),
            source_owner: prepared.source_owner(),
            source: prepared.source_identity(),
            expected_victim: prepared.expected_victim_identity(),
            intent_digest: prepared.intent_digest().map_err(|e| e.to_string())?,
            reservation_description: prepared
                .reservation_description()
                .map_err(|e| e.to_string())?,
            archive_plan_digest: prepared.archive_plan_digest().map_err(|e| e.to_string())?,
        };
        observation.validate()?;
        Ok(observation)
    }
    fn with_observation(
        directory: DirectoryCapability,
        prepared: crate::operations::PreparedNativeReplacement,
        observation: crate::directory_backend::PendingReplacementObservation,
    ) -> Self {
        Self {
            receipt_clients: Arc::new(AtomicUsize::new(0)),
            directory,
            observation,
            state: Mutex::new(PendingState {
                prepared: Some(prepared),
                submitted: None,
            }),
        }
    }
    fn current(&self, state: &PendingState) -> Result<(), String> {
        self.directory.check().map_err(|e| e.to_string())?;
        if let Some(job) = &state.submitted {
            // EXCHANGE legitimately changes the old named victim. After submit,
            // only the genuine current session/job authority is checked here.
            job.live_scope().ok_or("Missing native job scope")??;
        } else {
            let prepared = state
                .prepared
                .as_ref()
                .ok_or("Prepared replacement consumed")?;
            if prepared.intent_digest().map_err(|e| e.to_string())?
                != self.observation.intent_digest
                || prepared.archive_plan_digest().map_err(|e| e.to_string())?
                    != self.observation.archive_plan_digest
            {
                return Err("Changed replacement reservation".into());
            }
        }
        self.directory.check().map_err(|e| e.to_string())
    }
}
impl crate::directory_backend::WorkspacePendingReplacementBackend for PendingReplacement {
    fn check(&self) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "Prepared replacement lock poisoned")?;
        self.current(&state)
    }
    fn observe(&self) -> Result<crate::directory_backend::PendingReplacementObservation, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "Prepared replacement lock poisoned")?;
        self.current(&state)?;
        // Cached while actual zero-effect Work was live; descriptive only.
        let value = self.observation.clone();
        self.current(&state)?;
        Ok(value)
    }
    fn start_with_history(&self, bytes: &[u8]) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let receipt = crate::external_history::PreparedReceipt::decode(bytes)
            .map_err(|error| error.to_string())?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Prepared replacement lock poisoned")?;
        self.current(&state)?;
        if state.submitted.is_some() {
            return Err("Prepared replacement already submitted".into());
        }
        // Allocate/revalidate the wrapper's actual directory before submission,
        // so a fallible wrapper constructor cannot lose an already submitted job.
        let directory =
            DirectoryCapability::new(self.directory.session.clone(), self.directory.root.clone())
                .map_err(|e| e.to_string())?;
        let prepared = state
            .prepared
            .take()
            .ok_or("Prepared replacement consumed")?;
        let job = prepared
            .start_with_history(&receipt)
            .map_err(|e| e.to_string())?;
        let job = Arc::new(NativeJob {
            directory,
            job: Mutex::new(job),
            created: Mutex::new(None),
            replacement_pin: Mutex::new(None),
            terminal: Mutex::new(NativeTerminal::default()),
        });
        state.submitted = Some(job.clone());
        self.current(&state)?;
        drop(state);
        Ok(job)
    }
    fn acknowledged_committed_job(&self) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let pin = self.acknowledged_committed_pin()?;
        let state = self
            .state
            .lock()
            .map_err(|_| "Prepared replacement lock poisoned")?;
        self.current(&state)?;
        let native = state
            .submitted
            .as_ref()
            .ok_or("Replacement not submitted")?
            .clone();
        let operation = *native
            .job
            .lock()
            .map_err(|_| "Native job lock poisoned")?
            .operation_id()
            .as_bytes();
        let terminal = native
            .terminal
            .lock()
            .map_err(|_| "Native terminal lock poisoned")?
            .completed(operation)?;
        self.current(&state)?;
        self.receipt_clients
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1).filter(|_| count < 64)
            })
            .map_err(|_| "Native receipt client limit reached")?;
        let job = Arc::new(AcknowledgedJob {
            _pin: pin,
            _client: ReceiptClient(self.receipt_clients.clone()),
            native,
            terminal: Mutex::new(Some(terminal)),
            operation,
        });
        drop(state);
        Ok(job)
    }
    fn acknowledged_committed_pin(
        &self,
    ) -> Result<Arc<dyn crate::directory_backend::WorkspaceReplacementPinBackend>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "Prepared replacement lock poisoned")?;
        self.current(&state)?;
        let job = state
            .submitted
            .as_ref()
            .ok_or("Replacement not submitted")?;
        let pin = job.retained_replacement_pin()?;
        // The signed receipt alone cannot construct this pin. Verify the SAME
        // job's private durable prepare/commit sidecars and actual owned phase.
        pin.pin
            .verify_owned_history(&self.directory.session)
            .map_err(|e| e.to_string())?;
        crate::directory_backend::WorkspaceReplacementPinBackend::check(pin.as_ref())?;
        self.current(&state)?;
        drop(state);
        Ok(pin)
    }
    fn cancel(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Prepared replacement lock poisoned")?;
        if state.submitted.is_some() {
            return Err("Submitted replacement cannot be undone by cancel".into());
        }
        state.prepared.take();
        drop(state);
        Ok(())
    }
}

// Own the genuine reservation even if constructing its descriptive reply or
// checking post-admission scope fails. Unknown cannot drop it or re-arm another.
enum AtomicReservation {
    Empty,
    Reserved(Box<crate::operations::PreparedNativeReplacement>),
    Ready(Arc<PendingReplacement>),
}
struct CreatedTemporary {
    atomic: Mutex<AtomicReservation>,
    directory: DirectoryCapability,
    native: Arc<crate::operations::NativeCreatedTemporary>,
}
impl crate::directory_backend::WorkspaceCreatedTemporaryBackend for CreatedTemporary {
    fn set_permissions(&self, mode: u32) -> Result<(), String> {
        self.check()?;
        self.native
            .set_permissions(mode)
            .map_err(|error| error.to_string())?;
        self.check()
    }

    fn prepare_atomic_save(
        &self,
    ) -> crate::directory_backend::AtomicSavePreparation<
        Arc<dyn crate::directory_backend::WorkspacePendingReplacementBackend>,
    > {
        use crate::directory_backend::AtomicSavePreparation as Outcome;
        if let Err(error) = self.check() {
            return Outcome::Unknown(error);
        }
        let Ok(mut slot) = self.atomic.lock() else {
            return Outcome::Unknown("AtomicSave reservation lock poisoned".into());
        };
        match &*slot {
            AtomicReservation::Empty => {}
            AtomicReservation::Reserved(prepared) => {
                let _ = prepared.operation_id();
                return Outcome::Unknown("Actual native reservation already retained".into());
            }
            AtomicReservation::Ready(pending) => {
                let _ = crate::directory_backend::WorkspacePendingReplacementBackend::check(
                    pending.as_ref(),
                );
                return Outcome::Unknown("Actual source pending already retained".into());
            }
        }
        let directory = match DirectoryCapability::new(
            self.directory.session.clone(),
            self.directory.root.clone(),
        ) {
            Ok(directory) => directory,
            Err(error) => return Outcome::Unknown(error.to_string()),
        };
        let prepared = match self.native.prepare_atomic_save() {
            Ok(prepared) => prepared,
            Err(error) => {
                return match self.check() {
                    Ok(()) => Outcome::Refused(error.to_string()),
                    Err(scope) => Outcome::Unknown(scope),
                };
            }
        };
        *slot = AtomicReservation::Reserved(Box::new(prepared));
        let AtomicReservation::Reserved(prepared) = &*slot else {
            return Outcome::Unknown("Missing actual native reservation".into());
        };
        let observation = match PendingReplacement::observe_prepared(prepared) {
            Ok(value) => value,
            Err(error) => return Outcome::Unknown(error),
        };
        let AtomicReservation::Reserved(prepared) =
            std::mem::replace(&mut *slot, AtomicReservation::Empty)
        else {
            return Outcome::Unknown("Missing actual native reservation".into());
        };
        let pending = Arc::new(PendingReplacement::with_observation(
            directory,
            *prepared,
            observation,
        ));
        // Source ownership precedes every fallible post-admission check.
        *slot = AtomicReservation::Ready(pending.clone());
        if let Err(error) =
            crate::directory_backend::WorkspacePendingReplacementBackend::check(pending.as_ref())
        {
            return Outcome::Unknown(error);
        }
        drop(slot);
        Outcome::Prepared(pending)
    }

    fn prepare_replace(
        &self,
    ) -> Result<Arc<dyn crate::directory_backend::WorkspacePendingReplacementBackend>, String> {
        self.check()?;
        let prepared = self
            .native
            .prepare_replacement()
            .map_err(|error| error.to_string())?;
        let directory =
            DirectoryCapability::new(self.directory.session.clone(), self.directory.root.clone())
                .map_err(|error| error.to_string())?;
        let pending = PendingReplacement::new(directory, prepared)?;
        crate::directory_backend::WorkspacePendingReplacementBackend::check(&pending)?;
        Ok(Arc::new(pending))
    }

    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.check())
    }
    fn check(&self) -> Result<(), String> {
        self.directory.check().map_err(|error| error.to_string())?;
        self.native
            .check_scope()
            .map_err(|error| error.to_string())?;
        self.directory.check().map_err(|error| error.to_string())
    }
    fn retire(&self) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let reservation = self
            .atomic
            .lock()
            .map_err(|_| "AtomicSave reservation lock poisoned")?;
        match &*reservation {
            AtomicReservation::Empty => {}
            AtomicReservation::Reserved(_) => {
                return Err(
                    "Actual native reservation remains unknown; no cancellation ACK".into(),
                );
            }
            AtomicReservation::Ready(pending) => {
                let state = pending
                    .state
                    .lock()
                    .map_err(|_| "Actual pending state lock poisoned")?;
                if state.prepared.is_some() || state.submitted.is_some() {
                    return Err(
                        "Actual pending reservation has not acknowledged no-effects cancellation"
                            .into(),
                    );
                }
            }
        }
        self.check()?;
        let job = self.native.retire().map_err(|error| error.to_string())?;
        // The actual queued effect may already remove the old temp name.
        // Post-admission checking therefore observes source scope, not that name.
        self.directory.check().map_err(|error| error.to_string())?;
        let retained = self.directory.retain_job(job);
        drop(reservation);
        retained
    }
}

struct HistoryReadSubject {
    subject: Arc<crate::NativeHistoryReadSubject>,
}
impl crate::directory_backend::WorkspaceHistoryReadSubjectBackend for HistoryReadSubject {
    fn check_scope(&self) -> Result<(), String> {
        self.subject
            .check_scope()
            .map_err(|error| error.to_string())
    }
    fn check(&self) -> Result<(), String> {
        self.subject.check().map_err(|error| error.to_string())
    }
    fn observe(&self) -> Result<crate::directory_backend::HistoryReadObservation, String> {
        let actual = self.subject.observe().map_err(|error| error.to_string())?;
        let observation = crate::directory_backend::HistoryReadObservation {
            source_owner: actual.source_owner,
            root: actual.root,
            parent: actual.parent,
            parents: actual.parents,
            name: actual.name,
            identity: actual.identity,
        };
        observation.validate()?;
        Ok(observation)
    }
}

#[cfg(test)]
mod terminal_receipt_tests {
    use super::*;
    #[test]
    fn caches_only_actual_durable_native_item_and_finished() {
        // Genuine native grant, owner/journal and effect. No signed ownership or
        // positive root ACK authority is manufactured by this primitive test.
        let fixture = crate::operations::tests::completed_created_temporary_for_transport();
        let directory =
            DirectoryCapability::new(fixture.session.clone(), fixture.parent.clone()).unwrap();
        let job = directory
            .create_file(EntryName::new("terminal-report".into()).unwrap())
            .unwrap();
        let native = NativeJob {
            directory,
            job: Mutex::new(job),
            created: Mutex::new(None),
            replacement_pin: Mutex::new(None),
            terminal: Mutex::new(NativeTerminal::default()),
        };
        let deadline = tilo_test_env::hang_deadline();
        let mut observed = vec![];
        let operation;
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "actual native effect must terminate"
            );
            let report = native.poll().unwrap();
            let done = report
                .updates
                .iter()
                .any(|u| matches!(u, JobUpdate::Finished { .. }));
            for update in report.updates {
                if !matches!(update, JobUpdate::Progress { .. }) {
                    observed.push(update);
                }
            }
            if done {
                operation = report.operation_id;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let cached = native
            .terminal
            .lock()
            .unwrap()
            .completed(operation)
            .unwrap();
        assert_eq!(cached.operation_id, operation);
        assert_eq!(format!("{:?}", cached.updates), format!("{:?}", observed));
        assert!(
            native.poll().unwrap().updates.is_empty(),
            "original job remains draining"
        );
        fixture.revoke().unwrap();
        assert!(
            native.poll().is_err(),
            "cached observations never renew revoked source authority"
        );
    }
}
