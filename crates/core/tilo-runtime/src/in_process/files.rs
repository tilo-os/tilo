//! The person's workspace files as a phone's file provider reads and writes them: the trees the
//! device daemon in this process holds, and the bytes their files are made of, through the same
//! service the daemon's own folder and the workspace's agents reach ([`Paths`]).

use std::time::Duration;

use crate::in_process::Device;
use tilo_workspace_service::files::{Entry, EntryKind, Paths};

/// The watch a file provider holds while it shows a folder: dropping it ends the watch.
pub use tilo_workspace_service::files::FilesWatch as FileWatch;

/// How long a file ask waits for the person's services to start in the daemon before their
/// workspaces answer for nothing yet: a daemon starting a person signs in reads its workspaces
/// after, and a provider asks again.
const FILES_STARTING: Duration = Duration::from_secs(5);

/// What a folder of the workspace holds, one entry, as a phone's file provider shows it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileEntry {
    pub name: String,
    /// The entry's path in the workspace, the root being `""`.
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
    /// When the workspace last changed it, in Unix seconds; `0` when the tree does not say.
    pub modified_unix_seconds: i64,
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    File,
    Directory,
}

impl From<Entry> for FileEntry {
    fn from(entry: Entry) -> Self {
        Self {
            name: entry.name,
            path: entry.path,
            kind: match entry.kind {
                EntryKind::File => FileKind::File,
                EntryKind::Directory => FileKind::Directory,
            },
            size: entry.size,
            modified_unix_seconds: entry.modified_unix_seconds,
        }
    }
}

impl Device {
    /// The person's workspace files, read and written where the daemon in this process holds
    /// them.
    fn paths(&self) -> Result<Paths<'_>, String> {
        let Some(running) = self.running.as_ref() else {
            return Err("the device daemon has stopped".to_owned());
        };
        let services = running
            .services(FILES_STARTING)
            .map_err(|error| format!("the person's services did not start: {error}"))?;
        // The content route opens with the device's store, after its workspaces: a listing reads
        // the tree without it, and a file opened before it is refused until it comes.
        Ok(Paths::new(
            services.workspaces().clone(),
            services.record_content(),
            &self.runtime,
        ))
    }

    /// What the folder `folder` of `workspace` holds, the root being `""`.
    ///
    /// # Errors
    /// A path that leaves the workspace or names no folder there, or the workspace is not one
    /// this device holds.
    pub fn list_files(&self, workspace: &str, folder: &str) -> Result<Vec<FileEntry>, String> {
        let entries = self.paths()?.list(workspace, folder)?;
        Ok(entries.into_iter().map(FileEntry::from).collect())
    }

    /// The entry at `path` in `workspace`, and `None` when nothing is there.
    ///
    /// # Errors
    /// A path that leaves the workspace, or the workspace is not one this device holds.
    pub fn stat_file(&self, workspace: &str, path: &str) -> Result<Option<FileEntry>, String> {
        let found = self.paths()?.stat(workspace, path)?;
        Ok(found.map(FileEntry::from))
    }

    /// The bytes of the file at `path` in `workspace`, fetched from a device of the workspace
    /// when this one does not hold them.
    ///
    /// # Errors
    /// A path that leaves the workspace or names nothing or a folder there, no device hands the
    /// bytes over in time, or the workspace is not one this device holds.
    pub fn read_file(&self, workspace: &str, path: &str) -> Result<Vec<u8>, String> {
        self.paths()?.read(workspace, path)
    }

    /// Write `bytes` at `path` in `workspace`, making the folders on the way that are not there
    /// yet, and writing over a file of that name when `replace`.
    ///
    /// # Errors
    /// A path that leaves the workspace or names a folder, or the workspace did not take the
    /// change in time.
    pub fn write_file(
        &self,
        workspace: &str,
        path: &str,
        bytes: &[u8],
        replace: bool,
    ) -> Result<(), String> {
        self.paths()?.write(workspace, path, bytes, replace)
    }

    /// Make the folder `path` in `workspace`.
    ///
    /// # Errors
    /// A path that leaves the workspace, or the workspace did not take the change in time.
    pub fn make_folder(&self, workspace: &str, path: &str) -> Result<(), String> {
        self.paths()?.make_folder(workspace, path)
    }

    /// Give the entry at `path` in `workspace` the name `name`, beside itself.
    ///
    /// # Errors
    /// A path that leaves the workspace or names nothing, a name that is taken, or the workspace
    /// did not take the change in time.
    pub fn rename_file(&self, workspace: &str, path: &str, name: &str) -> Result<(), String> {
        self.paths()?.rename(workspace, path, name)
    }

    /// Take the entry at `path` in `workspace` out of it, into the workspace's trash.
    ///
    /// # Errors
    /// A path that leaves the workspace or names nothing, or the workspace did not take the
    /// change in time.
    pub fn delete_file(&self, workspace: &str, path: &str) -> Result<(), String> {
        self.paths()?.trash(workspace, path)
    }

    /// Watch `workspace`'s files: `changed` is called with the folders whose entries moved —
    /// made on this device or taken from the workspace — until the returned watch is dropped.
    ///
    /// # Errors
    /// The workspace is not one this device holds, or its tree does not open.
    pub fn watch_files(
        &self,
        workspace: &str,
        changed: impl Fn(Vec<String>) + Send + 'static,
    ) -> Result<FileWatch, String> {
        self.paths()?.watch(workspace, changed)
    }
}
