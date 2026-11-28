//! A workspace's files by a path in it, for a caller that has no folder mounted: a phone's file
//! provider, and the tests of one. It reads and writes the tree this device holds (a member copy
//! as a phone keeps, or the workspace's own where the device serves it), and the bytes its files
//! are made of, which a read fetches from whichever device of the workspace has them.
//!
//! Every name is a path in the workspace's own folder — `Photos/beach.jpg`, the root being `""` —
//! and a path that could leave the workspace (`..`, an absolute path) is refused before anything
//! is asked of the tree. What is written here is a change of the tree like one made in a mounted
//! folder, and reaches the other devices by the same sync.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use iroh_blobs::Hash;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_fs::content::{Manifest, is_manifest, whole_with};
use tilo_fs::hydration::ContentSource as _;
use tilo_fs::namespace::Namespace;
use tilo_fs::node::FileType;
use tilo_fs::tree_sync::FILES_APP;
use tilo_server_protocol::WorkspaceId;

use super::opened::{reads::opened_chunk, tree_route};
use super::{FilesWatch, WorkspaceSource};
use crate::content::{CrdtRoute, RouteContent};
use crate::workspaces::Workspaces;

/// How long a read of a file waits for bytes this device does not hold.
const CONTENT_WAIT: Duration = Duration::from_secs(20);

/// The longest name a file or a folder may have, in bytes.
const NAME_MAX: usize = 255;

/// What a folder of the workspace holds, one entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The entry's own name, never a path.
    pub name: String,
    /// The entry's path in the workspace, the root being `""`.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// When the workspace last changed it, in Unix seconds; `0` when the tree does not say.
    pub modified_unix_seconds: i64,
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Directory,
}

/// A workspace's files by path, read and written where this device holds them.
pub struct Paths {
    workspaces: Workspaces,
    /// Where the bytes of the files are kept and asked for; `None` reads the tree alone, as a
    /// caller that never opens a file.
    content: Option<Arc<RouteContent>>,
    /// The runtime the workspace's records are driven on, as the caller's thread hands it: none
    /// of these calls may be made from inside it.
    runtime: tokio::runtime::Handle,
}

impl Paths {
    #[must_use]
    pub fn new(
        workspaces: Workspaces,
        content: Option<Arc<RouteContent>>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            workspaces,
            content,
            runtime,
        }
    }

    /// What the folder `folder` of `workspace` holds, as the workspace has it now.
    ///
    /// # Errors
    /// A path that leaves the workspace or names no folder here, or the workspace is not held by
    /// this device.
    pub fn list(&self, workspace: &str, folder: &str) -> Result<Vec<Entry>, String> {
        let id = workspace_id(workspace)?;
        let folder = path_of(folder)?;
        self.read_tree(id, |tree| {
            let ino = tree
                .resolve_path_ino(&folder)
                .ok_or_else(|| format!("{folder:?} is no folder of this workspace"))?;
            let children = tree
                .readdir(ino)
                .ok_or_else(|| format!("{folder:?} is no folder of this workspace"))?;
            let mut entries = Vec::new();
            for (ino, kind, name) in children {
                // A name the workspace holds that is not UTF-8 is no name a provider offers,
                // and the tree lists its dot entries as any directory does.
                let Ok(name) = String::from_utf8(name.as_bytes().to_vec()) else {
                    continue;
                };
                if name == "." || name == ".." {
                    continue;
                }
                let Some(attr) = tree.getattr(ino) else {
                    continue;
                };
                let path = join(&folder, &name);
                entries.push(entry(&path, name, kind, &attr));
            }
            serde_json::to_value(entries).map_err(|error| error.to_string())
        })
    }

    /// The entry at `path` in `workspace`, and `None` when nothing is there.
    ///
    /// # Errors
    /// A path that leaves the workspace, or the workspace is not held by this device.
    pub fn stat(&self, workspace: &str, path: &str) -> Result<Option<Entry>, String> {
        let id = workspace_id(workspace)?;
        let path = path_of(path)?;
        let found = self.read_tree(id, |tree| {
            let Some(ino) = tree.resolve_path_ino(&path) else {
                return Ok(Value::Null);
            };
            let Some(attr) = tree.getattr(ino) else {
                return Ok(Value::Null);
            };
            let name = last(&path).to_owned();
            serde_json::to_value(entry(&path, name, attr.kind, &attr))
                .map_err(|error| error.to_string())
        })?;
        match found {
            Value::Null => Ok(None),
            found => serde_json::from_value(found)
                .map(Some)
                .map_err(|error| error.to_string()),
        }
    }

    /// The bytes of the file at `path` in `workspace`, fetched from a device of the workspace
    /// when this one does not hold them: the content its tree names at that path, as
    /// [`Self::read_content`] reads it.
    ///
    /// # Errors
    /// A path that leaves the workspace or names nothing or a folder there, or as
    /// [`Self::read_content`] reads a content address.
    pub fn read(&self, workspace: &str, path: &str) -> Result<Vec<u8>, String> {
        let id = workspace_id(workspace)?;
        let path = path_of(path)?;
        let held = self.read_tree(id, |tree| {
            let ino = tree
                .resolve_path_ino(&path)
                .ok_or_else(|| format!("{path:?} is nothing in this workspace"))?;
            let attr = tree
                .getattr(ino)
                .ok_or_else(|| format!("{path:?} is nothing in this workspace"))?;
            if attr.kind == FileType::Directory {
                return Err(format!("{path:?} is a folder"));
            }
            let hash = tree.hash_for(ino).map(|hash| hex::encode(hash.as_bytes()));
            serde_json::to_value(hash).map_err(|error| error.to_string())
        })?;
        let hash: Option<String> = serde_json::from_value(held).map_err(|e| e.to_string())?;
        let Some(hash) = hash else {
            // A file the tree holds no content for: an empty file, as every device makes one.
            return Ok(Vec::new());
        };
        self.read_content(workspace, &hash)
    }

    /// The whole bytes the content address `hash` names in `workspace`: content kept as it was
    /// written, or the file a manifest describes — how Files names a file it puts there —
    /// assembled from the chunks this device holds and those a device of the workspace hands
    /// over, each checked against its hash and opened with the workspace's Files keys where it is
    /// sealed. A caller that holds an offer's `hash` reads it here, with no path to resolve.
    ///
    /// # Errors
    /// `hash` is no content address, no device hands the content or one of its chunks over in
    /// time, a chunk is not the bytes its hash names, the manifest does not read, or the
    /// workspace's files are sealed and their keys are not here.
    pub fn read_content(&self, workspace: &str, hash: &str) -> Result<Vec<u8>, String> {
        let address: Hash = hash
            .parse()
            .map_err(|_| format!("{hash:?} is no content address"))?;
        let content = self.content()?;
        let route = CrdtRoute::app(FILES_APP, workspace);
        let source = WorkspaceSource::new(Arc::clone(&content), workspace, CONTENT_WAIT);
        let stored = match content.kept(&route, hash) {
            Some(stored) => stored,
            None => self.fetch(&source, address, hash)?.to_vec(),
        };
        if !is_manifest(&stored) {
            return Ok(stored);
        }
        let id = workspace_id(workspace)?;
        let manifest = Manifest::decode(&stored)
            .map_err(|error| format!("{hash} is no file of the workspace: {error}"))?;
        let keys = match manifest.sealed() {
            Some(_) => Some(self.content_keys(id)?.ok_or_else(|| {
                "this workspace's files are sealed, and their keys are not here".to_owned()
            })?),
            None => None,
        };
        let mut failure: Option<String> = None;
        let file = whole_with(
            stored,
            u64::MAX,
            |part| {
                if failure.is_some() {
                    return None;
                }
                let named = hex::encode(part);
                let bytes = match content.kept(&route, &named) {
                    Some(bytes) => bytes,
                    None => match self.fetch(&source, Hash::from(*part), &named) {
                        Ok(bytes) => bytes.to_vec(),
                        Err(error) => {
                            failure = Some(error);
                            return None;
                        }
                    },
                };
                if blake3::hash(&bytes).as_bytes() != part {
                    failure = Some(format!("a chunk of {hash} is not the bytes its hash names"));
                    return None;
                }
                Some(bytes)
            },
            |sealed, bytes| opened_chunk(id, keys.as_ref(), sealed, bytes),
        );
        file.ok_or_else(|| {
            failure.unwrap_or_else(|| format!("{hash} could not be put together from its chunks"))
        })
    }

    /// What a device of `workspace` hands over for `address`, named `named` in an error.
    fn fetch(&self, source: &WorkspaceSource, address: Hash, named: &str) -> Result<Bytes, String> {
        self.runtime.block_on(source.fetch(address)).ok_or_else(|| {
            format!(
                "no device of the workspace hands {named} over within {}s",
                CONTENT_WAIT.as_secs()
            )
        })
    }

    /// The keys `workspace`'s Files seals with, `None` where this device is in no key group.
    fn content_keys(&self, id: WorkspaceId) -> Result<Option<key_group::ContentKeys>, String> {
        match self
            .runtime
            .block_on(self.workspaces.own_route_keys(&tree_route(id), false))
        {
            Some(answer) => answer.map(Some),
            None => Ok(None),
        }
    }

    /// Write `bytes` at `path` in `workspace`, making the folders on the way that are not there
    /// yet. A file of that name is written over when `replace`, and refused otherwise.
    ///
    /// # Errors
    /// A path that leaves the workspace or names a folder, a file in the way of a folder, or the
    /// workspace did not take the change in time.
    pub fn write(
        &self,
        workspace: &str,
        path: &str,
        bytes: &[u8],
        replace: bool,
    ) -> Result<(), String> {
        let id = workspace_id(workspace)?;
        let path = path_of(path)?;
        if path.is_empty() {
            return Err("the workspace's own folder is no file to write".to_owned());
        }
        let (folder, name) = split(&path)?;
        let name = name.to_owned();
        let hash = tilo_content::hash_bytes(bytes);
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let content = self.content()?;
        content.offer(&CrdtRoute::app(FILES_APP, workspace), &hash, bytes)?;
        self.change(id, |tree| {
            tree.write_file(
                &folder,
                &name,
                (hash.as_str(), size),
                replace,
                tilo_clock::unix_now_ms(),
            )
            .map(|_| Value::Null)
        })
    }

    /// Make the folder `path` in `workspace`, making the folders on the way that are not there
    /// yet. A folder or file of that name is left as it is.
    ///
    /// # Errors
    /// A path that leaves the workspace, or the workspace did not take the change in time.
    pub fn make_folder(&self, workspace: &str, path: &str) -> Result<(), String> {
        let id = workspace_id(workspace)?;
        let path = path_of(path)?;
        if path.is_empty() {
            return Ok(());
        }
        self.change(id, |tree| {
            let (folder, name) = split(&path)?;
            let parent = tree
                .resolve_path_ino(&folder)
                .ok_or_else(|| format!("{folder:?} is no folder of this workspace"))?;
            if tree.lookup(parent, name).is_some() {
                return Ok(Value::Null);
            }
            tree.create_dir(parent, name, tilo_clock::unix_now_ms())
                .map(|_| Value::Null)
                .ok_or_else(|| format!("{path:?} could not be made"))
        })
    }

    /// Give the entry at `path` in `workspace` the name `name`, beside itself: its parent is
    /// unchanged, and a name that is taken is refused.
    ///
    /// # Errors
    /// A path that leaves the workspace or names nothing, a name no file may have or one that is
    /// taken, or the workspace did not take the change in time.
    pub fn rename(&self, workspace: &str, path: &str, name: &str) -> Result<(), String> {
        let id = workspace_id(workspace)?;
        let path = path_of(path)?;
        if path.is_empty() {
            return Err("the workspace's own folder is no entry to rename".to_owned());
        }
        plain(name)?;
        let name = name.to_owned();
        self.change(id, |tree| {
            let ino = tree
                .resolve_path_ino(&path)
                .ok_or_else(|| format!("{path:?} is nothing in this workspace"))?;
            let parent = tree
                .parent_ino(ino)
                .ok_or_else(|| format!("{path:?} is nothing in this workspace"))?;
            let from = last(&path).to_owned();
            if tree.lookup(parent, &name).is_some() {
                return Err(format!("{name:?} is taken in the same folder"));
            }
            if tree.rename(parent, &from, parent, &name, tilo_clock::unix_now_ms()) {
                Ok(Value::Null)
            } else {
                Err(format!("{path:?} could not be renamed"))
            }
        })
    }

    /// Take the entry at `path` in `workspace` out of it, a folder with everything it holds: it
    /// goes to the workspace's trash, which the person's Files brings it back from.
    ///
    /// # Errors
    /// A path that leaves the workspace or names nothing, or the workspace did not take the
    /// change in time.
    pub fn trash(&self, workspace: &str, path: &str) -> Result<(), String> {
        let id = workspace_id(workspace)?;
        let path = path_of(path)?;
        if path.is_empty() {
            return Err("the workspace's own folder is no entry to delete".to_owned());
        }
        self.change(id, |tree| {
            let ino = tree
                .resolve_path_ino(&path)
                .ok_or_else(|| format!("{path:?} is nothing in this workspace"))?;
            if tree.remove(ino) {
                Ok(Value::Null)
            } else {
                Err(format!("{path:?} could not be deleted"))
            }
        })
    }

    /// Watch `workspace`'s tree: `changed` is called with the folders whose entries moved, made
    /// here or taken from the workspace, until the returned watch is dropped. A provider that
    /// shows a folder this way reads it again when it moved, instead of asking on a timer.
    ///
    /// # Errors
    /// The workspace is not held by this device, or its tree does not open.
    pub fn watch(
        &self,
        workspace: &str,
        changed: impl Fn(Vec<String>) + Send + 'static,
    ) -> Result<FilesWatch, String> {
        let id = workspace_id(workspace)?;
        let puts = self.workspaces.puts();
        self.runtime.block_on(puts.watch(id, changed))
    }

    /// What `read` answers of `workspace`'s tree, as the workspace has it now.
    fn read_tree<T: DeserializeOwned>(
        &self,
        id: WorkspaceId,
        mut read: impl FnMut(&mut Namespace) -> Result<Value, String>,
    ) -> Result<T, String> {
        let puts = self.workspaces.puts();
        let value = self.runtime.block_on(puts.read(id, &mut read))?;
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    /// Change `workspace`'s tree with `change`, once the workspace has taken it.
    fn change(
        &self,
        id: WorkspaceId,
        mut change: impl FnMut(&mut Namespace) -> Result<Value, String>,
    ) -> Result<(), String> {
        let puts = self.workspaces.puts();
        self.runtime
            .block_on(puts.change(id, None, &mut change))
            .map(drop)
    }

    fn content(&self) -> Result<Arc<RouteContent>, String> {
        self.content.clone().ok_or_else(|| {
            "this device does not route the workspace's content, so it cannot open its files"
                .to_owned()
        })
    }
}

/// The entry `name` at `path` is, from the tree's `attr`.
fn entry(path: &str, name: String, kind: FileType, attr: &tilo_fs::node::FileAttr) -> Entry {
    let directory = kind == FileType::Directory;
    Entry {
        name,
        path: path.to_owned(),
        kind: if directory {
            EntryKind::Directory
        } else {
            EntryKind::File
        },
        size: if directory { 0 } else { attr.size },
        modified_unix_seconds: attr
            .mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
            }),
    }
}

/// `workspace` as the id a tree is held under.
fn workspace_id(workspace: &str) -> Result<WorkspaceId, String> {
    WorkspaceId::parse(workspace).ok_or_else(|| format!("{workspace:?} is no workspace's id"))
}

/// The path a caller names, with no leading or trailing slash: the root is `""`.
///
/// # Errors
/// A path that could leave the workspace: one starting at the root, one with an empty, `.` or
/// `..` part, or a part no name of the workspace's tree may have.
pub(crate) fn path_of(path: &str) -> Result<String, String> {
    if path.starts_with('/') {
        return Err(format!("{path:?} starts outside the workspace"));
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let mut parts = Vec::new();
    for part in trimmed.split('/') {
        if part.is_empty() {
            return Err(format!("{path:?} is no path in the workspace"));
        }
        plain(part)?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

/// Whether `name` is a name a file or folder of the workspace may have.
fn plain(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!("{name:?} is no name in the workspace"));
    }
    if name.len() > NAME_MAX || name.contains('\0') || name.contains('/') {
        return Err(format!("{name:?} is no name in the workspace"));
    }
    Ok(())
}

/// The folder and the name of the path `path`, which is no root.
fn split(path: &str) -> Result<(String, &str), String> {
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    plain(name)?;
    Ok((folder.to_owned(), name))
}

/// What follows the last `/` of `path`.
fn last(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or_default()
}

/// `folder` with `name` in it, the root's entries being just their names.
fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}
