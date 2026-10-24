//! A single already-granted native file on the existing filesystem protocol.
//! The source worker owns credentials; callers receive bytes, never its fd.

use std::sync::Arc;

use crate::workspace_server::{WorkspaceBackend, WorkspaceFileBackend};
use crate::{DirEntry, EntryKind, FileGrant, FsSession, GrantedEntry, Metadata, OpenMode};

/// Presentation below this private service. This label conveys no authority.
pub use tilo_fs_client::SELECTED_FILE;

/// Consumes an existing opaque session/grant rather than opening a caller path.
/// The authenticating host must retain its source and target-person lifetime in
/// `session.attach_lifetime`; this backend adds no credential or source grant.
pub struct SelectedFileBackend {
    session: FsSession,
    file: FileGrant,
}

impl SelectedFileBackend {
    /// # Errors
    /// Refuses a stale, foreign, nonregular or unreadable selected file.
    pub fn new(session: FsSession, file: FileGrant) -> Result<Self, crate::FsError> {
        let metadata = session.metadata(&GrantedEntry::File(file.clone()))?;
        if metadata.kind != EntryKind::File {
            return Err(crate::FsError::Unsupported(
                "regular selected file required",
            ));
        }
        session.read_at(&file, 0, 0)?;
        Ok(Self { session, file })
    }

    fn selected(&self, path: &str) -> Result<Metadata, String> {
        if path != SELECTED_FILE {
            return Err("selected file not found".into());
        }
        self.session
            .metadata(&GrantedEntry::File(self.file.clone()))
            .map_err(|error| error.to_string())
    }
}

impl WorkspaceBackend for SelectedFileBackend {
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        if !path.is_empty() {
            return Err("selected projection is not a directory".into());
        }
        Ok(vec![DirEntry {
            name: SELECTED_FILE.into(),
            metadata: self.selected(SELECTED_FILE)?,
        }])
    }

    fn stat(&self, path: &str) -> Result<Metadata, String> {
        let selected = self.selected(SELECTED_FILE)?;
        if path.is_empty() {
            return Ok(Metadata {
                kind: EntryKind::Directory,
                size: 0,
                modified_unix_seconds: selected.modified_unix_seconds,
                needs_hydration: false,
            });
        }
        self.selected(path)
    }

    fn read(&self, path: &str, offset: u64, max_len: u32) -> Result<Vec<u8>, String> {
        self.selected(path)?;
        self.session
            .read_at(&self.file, offset, max_len)
            .map_err(|error| error.to_string())
    }

    fn open(
        &self,
        path: &str,
        mode: OpenMode,
    ) -> Result<Option<Arc<dyn WorkspaceFileBackend>>, String> {
        self.selected(path)?;
        open_granted(self.session.clone(), self.file.clone(), mode).map(Some)
    }
}

pub(crate) fn open_granted(
    session: FsSession,
    file: FileGrant,
    mode: OpenMode,
) -> Result<Arc<dyn WorkspaceFileBackend>, String> {
    let append = match mode {
        OpenMode::Read => {
            session.read_at(&file, 0, 0).map_err(|e| e.to_string())?;
            None
        }
        OpenMode::Write => {
            session
                .truncate(&file, 0)
                .map_err(|error| error.to_string())?;
            None
        }
        OpenMode::WriteExisting => {
            session.validate_write(&file).map_err(|e| e.to_string())?;
            None
        }
        OpenMode::ReadWriteExisting => {
            session
                .validate_read_write(&file)
                .map_err(|e| e.to_string())?;
            None
        }
        OpenMode::Append => Some(session.open_append(&file).map_err(|e| e.to_string())?),
    };
    Ok(Arc::new(SelectedHandle {
        session,
        file,
        mode,
        append,
    }))
}

struct SelectedHandle {
    session: FsSession,
    file: FileGrant,
    mode: OpenMode,
    append: Option<crate::FsAppendHandle>,
}

impl WorkspaceFileBackend for SelectedHandle {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(
            self.session
                .check_entry(&GrantedEntry::File(self.file.clone()))
                .map_err(|e| e.to_string()),
        )
    }
    fn native_identity(&self) -> Result<Option<[u64; 2]>, String> {
        self.session
            .entry_identity(&GrantedEntry::File(self.file.clone()))
            .map(Some)
            .map_err(|e| e.to_string())
    }
    fn read(&self, offset: u64, max_len: u32) -> Result<Vec<u8>, String> {
        if !matches!(self.mode, OpenMode::Read | OpenMode::ReadWriteExisting) {
            return Err("selected handle is write only".into());
        }
        self.session
            .read_at(&self.file, offset, max_len)
            .map_err(|error| error.to_string())
    }

    fn write(&self, offset: u64, bytes: &[u8]) -> Result<u32, String> {
        if !matches!(
            self.mode,
            OpenMode::Write | OpenMode::WriteExisting | OpenMode::ReadWriteExisting
        ) {
            return Err("selected handle is read only".into());
        }
        self.session
            .write_at(&self.file, offset, bytes)
            .map_err(|error| error.to_string())
    }

    fn append(&self, bytes: &[u8]) -> Result<u32, String> {
        self.append
            .as_ref()
            .ok_or("selected handle is not append")?
            .append(bytes)
            .map_err(|error| error.to_string())
    }

    fn flush(&self) -> Result<(), String> {
        if self.mode == OpenMode::Read {
            self.session
                .metadata(&GrantedEntry::File(self.file.clone()))
                .map(|_| ())
        } else {
            self.session.sync_file(&self.file)
        }
        .map_err(|error| error.to_string())
    }

    fn metadata(&self) -> Result<Metadata, String> {
        self.session
            .metadata(&GrantedEntry::File(self.file.clone()))
            .map_err(|error| error.to_string())
    }
}
