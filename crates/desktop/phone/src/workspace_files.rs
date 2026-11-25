//! The person's workspace files as a phone's own Files app and every other app's file picker see
//! them: each of the person's workspaces is a root, and every folder and file below it a document
//! whose id is `<workspace>:<path in the workspace>`. The provider behind Android's documents API
//! asks these, and every phone with a file provider asks the same ones of its device daemon.

use tilo_runtime::in_process::{Device, FileEntry, FileKind, Workspace};

/// The type Android gives a folder.
pub const DIRECTORY_MIME: &str = "vnd.android.document/directory";

/// What a file provider shows of one entry of a workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// The entry's path in the workspace, the root being `""`.
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
    pub modified_unix_seconds: i64,
    pub mime: String,
}

/// The person's workspaces, each a root: none until a person's identity has the device.
///
/// # Errors
/// The daemon answered with a refusal.
pub fn roots(device: &Device) -> Result<Vec<Workspace>, String> {
    let devices = device.devices().map_err(|error| format!("{error:#}"))?;
    Ok(devices.workspaces)
}

/// The document id of `path` in `workspace`.
#[must_use]
pub fn document_id(workspace: &str, path: &str) -> String {
    let path = path.trim_matches('/');
    if path.is_empty() {
        return workspace.to_owned();
    }
    format!("{workspace}:{path}")
}

/// The workspace and the path a document id names: the root's document is the workspace alone.
/// A workspace's id is a UUID, which holds no `:`, so the first one is the separator.
///
/// # Errors
/// `id` names no workspace: it is empty.
pub fn split_document_id(id: &str) -> Result<(String, String), String> {
    let (workspace, path) = id.split_once(':').unwrap_or((id, ""));
    if workspace.is_empty() {
        return Err(format!("{id:?} is no document of a workspace"));
    }
    Ok((workspace.to_owned(), path.to_owned()))
}

/// Whether `id` names a workspace rather than one of its documents.
#[must_use]
pub fn is_workspace(id: &str) -> bool {
    !id.is_empty() && !id.contains(':')
}

/// What the folder `folder` of `workspace` holds.
///
/// # Errors
/// A path that leaves the workspace or names no folder there, or the workspace is not one this
/// device holds.
pub fn list(device: &Device, workspace: &str, folder: &str) -> Result<Vec<Entry>, String> {
    let entries = device
        .list_files(workspace, folder)
        .map_err(|error| format!("{error:#}"))?;
    Ok(entries.into_iter().map(described).collect())
}

/// The entry at `path` in `workspace`, and `None` when nothing is there.
///
/// # Errors
/// A path that leaves the workspace, or the workspace is not one this device holds.
pub fn stat(device: &Device, workspace: &str, path: &str) -> Result<Option<Entry>, String> {
    let found = device
        .stat_file(workspace, path)
        .map_err(|error| format!("{error:#}"))?;
    Ok(found.map(described))
}

/// The bytes of the file at `path` in `workspace`.
///
/// # Errors
/// A path that leaves the workspace or names nothing or a folder there, or no device of the
/// workspace hands the bytes over in time.
pub fn read(device: &Device, workspace: &str, path: &str) -> Result<Vec<u8>, String> {
    device
        .read_file(workspace, path)
        .map_err(|error| format!("{error:#}"))
}

/// Write `bytes` at `path` in `workspace`, making the folders on the way that are not there yet.
///
/// # Errors
/// A path that leaves the workspace or names a folder, or the workspace did not take the change
/// in time.
pub fn write(device: &Device, workspace: &str, path: &str, bytes: &[u8]) -> Result<(), String> {
    device
        .write_file(workspace, path, bytes, true)
        .map_err(|error| format!("{error:#}"))
}

/// Make an entry named after `name` in `folder`, a folder when `directory`, under the first free
/// name: `photo.jpg`, then `photo (1).jpg`. Answers what it made.
///
/// # Errors
/// A path that leaves the workspace, a name no file may have, or the workspace did not take the
/// change in time.
pub fn create(
    device: &Device,
    workspace: &str,
    folder: &str,
    name: &str,
    directory: bool,
) -> Result<Entry, String> {
    let name = free_name(device, workspace, folder, name)?;
    let path = if folder.is_empty() {
        name.clone()
    } else {
        format!("{}/{}", folder.trim_end_matches('/'), name)
    };
    if directory {
        device
            .make_folder(workspace, &path)
            .map_err(|error| format!("{error:#}"))?;
    } else {
        device
            .write_file(workspace, &path, &[], false)
            .map_err(|error| format!("{error:#}"))?;
    }
    stat(device, workspace, &path)?.ok_or_else(|| format!("{path:?} was not made in the workspace"))
}

/// Give the entry at `path` in `workspace` the name `name`, beside itself, and answer its new
/// document id.
///
/// # Errors
/// A path that leaves the workspace or names nothing, a name that is taken, or the workspace did
/// not take the change in time.
pub fn rename(device: &Device, workspace: &str, path: &str, name: &str) -> Result<String, String> {
    device
        .rename_file(workspace, path, name)
        .map_err(|error| format!("{error:#}"))?;
    let parent = path.rsplit_once('/').map_or("", |(folder, _)| folder);
    Ok(document_id(workspace, &join(parent, name)))
}

/// Take the entry at `path` in `workspace` out of it, into the workspace's trash.
///
/// # Errors
/// A path that leaves the workspace or names nothing, or the workspace did not take the change
/// in time.
pub fn delete(device: &Device, workspace: &str, path: &str) -> Result<(), String> {
    device
        .delete_file(workspace, path)
        .map_err(|error| format!("{error:#}"))
}

/// A token that changes whenever anything in `workspace`'s files does.
///
/// # Errors
/// The workspace is not one this device holds.
pub fn revision(device: &Device, workspace: &str) -> Result<String, String> {
    device
        .files_revision(workspace)
        .map_err(|error| format!("{error:#}"))
}

/// The first name after `name` that `folder` does not hold: `photo.jpg`, then `photo (1).jpg`.
fn free_name(device: &Device, workspace: &str, folder: &str, name: &str) -> Result<String, String> {
    let taken = |name: &str| -> Result<bool, String> {
        let path = join(folder, name);
        Ok(stat(device, workspace, &path)?.is_some())
    };
    if !taken(name)? {
        return Ok(name.to_owned());
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    for count in 1_u32..1_000 {
        let candidate = format!("{stem} ({count}){extension}");
        if !taken(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(format!("{name:?} has no free name left in {folder:?}"))
}

/// `entry` as the provider shows it.
fn described(entry: FileEntry) -> Entry {
    let mime = match entry.kind {
        FileKind::Directory => DIRECTORY_MIME.to_owned(),
        FileKind::File => tilo_mime::from_path_or_octet_stream(&entry.name).to_owned(),
    };
    Entry {
        name: entry.name,
        path: entry.path,
        kind: entry.kind,
        size: entry.size,
        modified_unix_seconds: entry.modified_unix_seconds,
        mime,
    }
}

fn join(folder: &str, name: &str) -> String {
    let folder = folder.trim_matches('/');
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}
