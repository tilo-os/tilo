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
use std::sync::{Arc, Mutex};

/// Opaque native directory custody; cannot be recovered from a selected file.
pub struct DirectoryCapability {
    session: FsSession,
    root: DirectoryGrant,
    cursor: Mutex<(u64, Option<(u64, PageCursor)>)>,
}
impl DirectoryCapability {
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
    pub fn metadata(&self) -> Result<Metadata, FsError> {
        self.session
            .metadata(&GrantedEntry::Directory(self.root.clone()))
    }
    pub fn list(&self, cursor: Option<PageCursor>) -> Result<DirectoryPage, FsError> {
        self.session.list(&self.root, cursor)
    }
    pub fn directory(&self, name: &EntryName) -> Result<Arc<Self>, FsError> {
        let GrantedEntry::Directory(root) = self.session.lookup(&self.root, name)? else {
            return Err(FsError::Unsupported("directory child required"));
        };
        Ok(Arc::new(Self::new(self.session.clone(), root)?))
    }
    /// Open only an existing no-follow regular child. Write truncates; Append
    /// atomically appends; `ReadWriteExisting` and `WriteExisting` preserve length. Missing files are never
    /// silently created: creation must have an actual durable native job.
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
    #[cfg(test)]
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
    directory: DirectoryCapability,
    job: Mutex<FsJob>,
    terminal: Mutex<NativeTerminal>,
}
impl DirectoryCapability {
    fn retain_job(&self, job: FsJob) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let directory =
            Self::new(self.session.clone(), self.root.clone()).map_err(|e| e.to_string())?;
        Ok(Arc::new(NativeJob {
            directory,
            job: Mutex::new(job),
            terminal: Mutex::new(NativeTerminal::default()),
        }))
    }
}
impl WorkspaceDirectoryBackend for DirectoryCapability {
    fn sync_directory(&self) -> Result<(), String> {
        self.session
            .sync_directory(&self.root)
            .map_err(|error| error.to_string())
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
impl WorkspaceJobBackend for NativeJob {
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

pub struct DirectoryWorkspaceBackend(Arc<DirectoryCapability>);
impl DirectoryWorkspaceBackend {
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

#[cfg(test)]
mod terminal_receipt_tests {
    use super::*;
    #[test]
    fn caches_only_actual_durable_native_item_and_finished() {
        let fixture = crate::operations::tests::Fixture::new();
        let directory =
            DirectoryCapability::new(fixture.session.clone(), fixture.src.clone()).unwrap();
        let job = directory
            .create_file(EntryName::new("terminal-report".into()).unwrap())
            .unwrap();
        let native = NativeJob {
            directory,
            job: Mutex::new(job),
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
        fixture.authority.revoke_directory(&fixture.src).unwrap();
        assert!(
            native.poll().is_err(),
            "cached observations never renew revoked source authority"
        );
    }
}
