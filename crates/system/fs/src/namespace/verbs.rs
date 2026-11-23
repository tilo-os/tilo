//! A file manager's verbs on a workspace's tree, each by a path in the workspace: a new folder,
//! a rename, a move, a copy, the trash and the way back out of it.
//!
//! Whoever holds the tree's documents and no folder of its own carries them out with these: a
//! workspace's server while every device of its people is off, or the device that keeps the
//! workspace. A mounted computer does the same through its folder, and the tree reads alike
//! afterwards.
//!
//! A path starts at the workspace's folder (`Tasks/12 Plan/notes.md`) and stays in it: no
//! leading `/`, no `.` and no `..`. The tree holds files and folders and nothing that points
//! elsewhere, so no part of a path leads out of the workspace. A verb is checked whole before
//! it changes anything; whoever calls one that is refused keeps none of what it wrote.

use std::collections::BTreeSet;

use crate::node::FileType;

use super::{Namespace, ROOT_ID, ROOT_INO, ResourceWrite, hash_from_hex, join, plain};

mod read;
mod write;

pub use read::{FileAt, MOST_LISTED, Page, Row, Trashed};
pub use write::Written;

/// The most files and folders one copy makes: a larger folder is copied in parts, each an
/// action of its own.
pub const MOST_COPIED: usize = 4096;

/// What one verb changed in the tree.
pub struct Changed {
    /// What each document of the tree changed.
    pub writes: Vec<ResourceWrite>,
    /// Each entry the verb made, renamed, moved, copied, put in the trash or brought back, in
    /// the order asked.
    pub entries: Vec<Touched>,
    /// The folders it made on the way, outermost first, as paths in the workspace.
    pub made: Vec<String>,
}

/// One entry a verb touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Touched {
    /// The node's key in the tree: what brings it back out of the trash.
    pub id: String,
    /// Where it was, as a path in the workspace; `None` for one the verb made or brought back.
    pub from: Option<String>,
    /// Where it is now; `None` for one now in the trash.
    pub to: Option<String>,
    /// Whether it is a folder.
    pub folder: bool,
}

/// One entry a verb names: the folder it is in, its name there and its node.
struct Named {
    parent: u64,
    name: String,
    ino: u64,
    folder: bool,
    path: String,
}

/// The folder `path` is in: the workspace's own, as the empty path, for a name alone.
fn folder_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// How a refusal names the folder at `path`.
fn shown(path: &str) -> String {
    if path.is_empty() {
        "the workspace's folder".to_owned()
    } else {
        format!("{path:?}")
    }
}

/// The names `path` is made of, a path in the workspace; none for the workspace's own folder.
fn names(path: &str) -> Result<Vec<&str>, String> {
    if path.starts_with('/') {
        return Err(format!(
            "{path:?} is no path in the workspace: one starts at the workspace's folder, with no \
             leading /"
        ));
    }
    let mut names = Vec::new();
    // A doubled or trailing `/` names nothing.
    for name in path.split('/').filter(|name| !name.is_empty()) {
        if !plain(name) {
            return Err(format!("{name:?} in {path:?} is no name in the workspace"));
        }
        names.push(name);
    }
    Ok(names)
}

/// Whether `inner` is `outer` or lies in it.
fn within(inner: &str, outer: &str) -> bool {
    inner == outer
        || inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl Namespace {
    /// The folder at `path` when all of it is there, `None` when a folder on the way is not.
    fn folder_if_there(&mut self, path: &str) -> Result<Option<u64>, String> {
        let mut at = ROOT_INO;
        let mut walked = String::new();
        for name in names(path)? {
            walked = join(&walked, name);
            at = match self.lookup(at, name) {
                Some(attr) if attr.kind == FileType::Directory => attr.ino.into(),
                Some(_) => return Err(format!("{walked:?} is a file, not a folder")),
                None => return Ok(None),
            };
        }
        Ok(Some(at))
    }

    /// The folder at `path`, which is there.
    fn folder_at(&mut self, path: &str) -> Result<u64, String> {
        self.folder_if_there(path)?
            .ok_or_else(|| format!("the workspace has no folder {path:?}"))
    }

    /// The folder at `path`, made with every folder on the way that is not there yet; each one
    /// made is added to `made`.
    fn folder_made(
        &mut self,
        path: &str,
        mtime: u64,
        made: &mut Vec<String>,
    ) -> Result<u64, String> {
        let mut at = ROOT_INO;
        let mut walked = String::new();
        for name in names(path)? {
            walked = join(&walked, name);
            at = match self.lookup(at, name) {
                Some(attr) if attr.kind == FileType::Directory => attr.ino.into(),
                Some(_) => return Err(format!("{walked:?} is a file, not a folder")),
                None => {
                    let ino = self
                        .create_dir(at, name, mtime)
                        .ok_or_else(|| format!("the folder {walked:?} could not be made"))?;
                    made.push(walked.clone());
                    ino
                }
            };
        }
        Ok(at)
    }

    /// The entries `entries` name under `folder`, each by its name there or by a path from it.
    /// Refused: none named, one that is not there, the folder itself, or one named twice.
    fn named_under(&mut self, folder: &str, entries: &[String]) -> Result<Vec<Named>, String> {
        if entries.is_empty() {
            return Err("no entry is named".to_owned());
        }
        let base = names(folder)?.join("/");
        let mut seen = BTreeSet::new();
        let mut named = Vec::new();
        for entry in entries {
            let inner = names(entry)?;
            let Some((name, folders)) = inner.split_last() else {
                return Err(
                    "an entry is named by its name in the folder, or by a path from it".to_owned(),
                );
            };
            let holder = join(&base, &folders.join("/"));
            let parent = self.folder_at(&holder)?;
            let path = join(&holder, name);
            let attr = self
                .lookup(parent, name)
                .ok_or_else(|| format!("the workspace has no {path:?}"))?;
            let ino: u64 = attr.ino.into();
            if !seen.insert(ino) {
                return Err(format!("{path:?} is named twice"));
            }
            named.push(Named {
                parent,
                name: (*name).to_owned(),
                ino,
                folder: attr.kind == FileType::Directory,
                path,
            });
        }
        Ok(named)
    }

    /// What a verb answers of the node `ino` it touched.
    fn touched(
        &self,
        ino: u64,
        from: Option<String>,
        to: Option<String>,
        folder: bool,
    ) -> Result<Touched, String> {
        let id = self
            .id_for_ino(ino)
            .ok_or("the tree lost a node it just wrote")?
            .to_owned();
        Ok(Touched {
            id,
            from,
            to,
            folder,
        })
    }

    /// The name `wanted` takes in `folder`: itself when it is free, else the first free one of
    /// `name (2).ext`, `name (3).ext`…, as a file manager keeps both.
    fn free_name(&mut self, folder: u64, wanted: &str) -> String {
        if self.lookup(folder, wanted).is_none() {
            return wanted.to_owned();
        }
        // A leading dot names a hidden file, not an extension.
        let (stem, extension) = match wanted.rfind('.') {
            Some(dot) if dot > 0 => (
                wanted.get(..dot).unwrap_or(wanted),
                wanted.get(dot..).unwrap_or(""),
            ),
            _ => (wanted, ""),
        };
        (2..=u32::MAX)
            .map(|n| format!("{stem} ({n}){extension}"))
            .find(|name| self.lookup(folder, name).is_none())
            .unwrap_or_else(|| wanted.to_owned())
    }

    /// The files and folders of the folder `ino`, each with its name's bytes.
    fn held_by(&mut self, ino: u64) -> Vec<(u64, Vec<u8>)> {
        self.readdir(ino)
            .unwrap_or_default()
            .into_iter()
            .map(|(child, _, name)| (child, name.as_bytes().to_vec()))
            .filter(|(_, name)| name != b"." && name != b"..")
            .collect()
    }

    /// Count into `count` the nodes a copy of `ino` (at `path`) makes, refusing what a copy
    /// does not carry.
    fn count_copied(&mut self, ino: u64, path: &str, count: &mut usize) -> Result<(), String> {
        *count = count.saturating_add(1);
        if *count > MOST_COPIED {
            return Err(format!(
                "one copy makes at most {MOST_COPIED} files and folders: copy {path:?} in parts"
            ));
        }
        match self.getattr(ino).map(|attr| attr.kind) {
            Some(FileType::Directory) => {
                for (child, name) in self.held_by(ino) {
                    let inner = join(path, &String::from_utf8_lossy(&name));
                    self.count_copied(child, &inner, count)?;
                }
                Ok(())
            }
            Some(_) => Ok(()),
            None => Err(format!("the workspace has no {path:?}")),
        }
    }

    /// Copy the node `ino` into the folder `target` under `name`, with all a folder holds.
    fn copy_node(&mut self, ino: u64, target: u64, name: &[u8], mtime: u64) -> Result<u64, String> {
        let attr = self
            .getattr(ino)
            .ok_or("a file went missing under the copy")?;
        if attr.kind == FileType::Directory {
            let copy = self
                .create_dir(target, name, mtime)
                .ok_or("a folder of the copy could not be made")?;
            for (child, child_name) in self.held_by(ino) {
                self.copy_node(child, copy, &child_name, mtime)?;
            }
            return Ok(copy);
        }
        let hash = self.hash_for(ino);
        self.create_file(target, name, hash.as_ref(), attr.size, mtime)
            .ok_or_else(|| "a file of the copy could not be made".to_owned())
    }

    /// Make the folder `name` in the folder `parent`, with every folder on the way that is not
    /// there yet. A folder that is there already is left as it is, and nothing is answered as
    /// made: asked again by a later step of the same work, it is still the one folder.
    ///
    /// # Errors
    /// A path that is none of the workspace, a name no folder may have, or a file in the way.
    pub fn make_folder(&mut self, parent: &str, name: &str, mtime: u64) -> Result<Changed, String> {
        if !plain(name) {
            return Err(format!("{name:?} is no folder's name"));
        }
        let path = join(&names(parent)?.join("/"), name);
        // A file in the way refuses it before any folder on the way is made.
        self.folder_if_there(&path)?;
        let before = self.before_change();
        let mut made = Vec::new();
        let ino = self.folder_made(&path, mtime, &mut made)?;
        let entries = match made.pop() {
            Some(new) => vec![self.touched(ino, None, Some(new), true)?],
            None => Vec::new(),
        };
        Ok(Changed {
            writes: self.document_writes(&before),
            entries,
            made,
        })
    }

    /// Give the entry `entry` of the folder `folder` the name `name`.
    ///
    /// # Errors
    /// An entry that is not there, a name nothing may have, or one taken in that folder: a
    /// rename never writes over another entry.
    pub fn rename_entry(
        &mut self,
        folder: &str,
        entry: &str,
        name: &str,
        mtime: u64,
    ) -> Result<Changed, String> {
        if !plain(name) {
            return Err(format!("{name:?} is no name for it"));
        }
        let Some(entry) = self.named_under(folder, &[entry.to_owned()])?.pop() else {
            return Err("no entry is named".to_owned());
        };
        if entry.name == name {
            return Err(format!("{name:?} is its name already"));
        }
        if self.lookup(entry.parent, name).is_some() {
            return Err(format!("{name:?} is taken in that folder"));
        }
        let before = self.before_change();
        if !self.rename(entry.parent, &entry.name, entry.parent, name, mtime) {
            return Err(format!("{:?} could not be renamed", entry.path));
        }
        let to = join(folder_of(&entry.path), name);
        let touched = self.touched(entry.ino, Some(entry.path), Some(to), entry.folder)?;
        Ok(Changed {
            writes: self.document_writes(&before),
            entries: vec![touched],
            made: Vec::new(),
        })
    }

    /// Move `entries` of the folder `from` into the folder `to`, keeping their names. `create`
    /// makes `to`, and every folder on the way to it, when it is not there yet.
    ///
    /// # Errors
    /// An entry that is not there, a folder moved into itself, a destination that is not there
    /// and not to be made, or a name taken there: a move never writes over another entry.
    pub fn move_entries(
        &mut self,
        from: &str,
        entries: &[String],
        to: &str,
        create: bool,
        mtime: u64,
    ) -> Result<Changed, String> {
        let named = self.named_under(from, entries)?;
        let target = names(to)?.join("/");
        let there = self.folder_if_there(&target)?;
        if there.is_none() && !create {
            return Err(format!("the workspace has no folder {target:?}"));
        }
        let mut taking = BTreeSet::new();
        for entry in &named {
            if entry.folder && within(&target, &entry.path) {
                return Err(format!("{:?} cannot be moved into itself", entry.path));
            }
            if there == Some(entry.parent) {
                return Err(format!("{:?} is in that folder already", entry.path));
            }
            if !taking.insert(entry.name.as_str()) {
                return Err(format!(
                    "two entries named {:?} cannot go into one folder",
                    entry.name
                ));
            }
            if let Some(there) = there
                && self.lookup(there, &entry.name).is_some()
            {
                return Err(format!("{:?} is taken in {}", entry.name, shown(&target)));
            }
        }
        let before = self.before_change();
        let mut made = Vec::new();
        let folder = self.folder_made(&target, mtime, &mut made)?;
        let mut touched = Vec::new();
        for entry in named {
            if !self.rename(entry.parent, &entry.name, folder, &entry.name, mtime) {
                return Err(format!("{:?} could not be moved", entry.path));
            }
            let to = join(&target, &entry.name);
            touched.push(self.touched(entry.ino, Some(entry.path), Some(to), entry.folder)?);
        }
        Ok(Changed {
            writes: self.document_writes(&before),
            entries: touched,
            made,
        })
    }

    /// Copy `entries` of the folder `from` into the folder `to`, a folder with all it holds;
    /// `to` is made when it is not there yet. A copy whose name is taken there gets a free one
    /// beside it (`plan (2).md`), so a copy never writes over another entry.
    ///
    /// # Errors
    /// An entry that is not there, a folder copied into itself, more than [`MOST_COPIED`]
    /// files and folders, or a text document the tree itself keeps.
    pub fn copy_entries(
        &mut self,
        from: &str,
        entries: &[String],
        to: &str,
        mtime: u64,
    ) -> Result<Changed, String> {
        let named = self.named_under(from, entries)?;
        let target = names(to)?.join("/");
        self.folder_if_there(&target)?;
        let mut count = 0;
        for entry in &named {
            if entry.folder && within(&target, &entry.path) {
                return Err(format!("{:?} cannot be copied into itself", entry.path));
            }
            self.count_copied(entry.ino, &entry.path, &mut count)?;
        }
        let before = self.before_change();
        let mut made = Vec::new();
        let folder = self.folder_made(&target, mtime, &mut made)?;
        let mut touched = Vec::new();
        for entry in named {
            let name = self.free_name(folder, &entry.name);
            let copy = self.copy_node(entry.ino, folder, name.as_bytes(), mtime)?;
            let to = join(&target, &name);
            touched.push(self.touched(copy, Some(entry.path), Some(to), entry.folder)?);
        }
        Ok(Changed {
            writes: self.document_writes(&before),
            entries: touched,
            made,
        })
    }

    /// Put `entries` of the folder `folder` in the workspace's trash, a folder with all it
    /// holds. Each is answered with its key in the tree, which brings it back
    /// ([`Self::restore_entries`]).
    ///
    /// # Errors
    /// An entry that is not there, or one that lies in another one named with it.
    pub fn trash_entries(&mut self, folder: &str, entries: &[String]) -> Result<Changed, String> {
        let named = self.named_under(folder, entries)?;
        for entry in &named {
            if let Some(outer) = named
                .iter()
                .find(|outer| outer.ino != entry.ino && within(&entry.path, &outer.path))
            {
                return Err(format!(
                    "{:?} is in {:?}, which goes to the trash with all it holds",
                    entry.path, outer.path
                ));
            }
        }
        let before = self.before_change();
        let mut touched = Vec::new();
        for entry in named {
            let was = self.touched(entry.ino, Some(entry.path.clone()), None, entry.folder)?;
            if !self.remove(entry.ino) {
                return Err(format!("{:?} could not be put in the trash", entry.path));
            }
            touched.push(was);
        }
        Ok(Changed {
            writes: self.document_writes(&before),
            entries: touched,
            made: Vec::new(),
        })
    }

    /// Bring `entries` back out of the trash, each into the folder it was deleted from, or the
    /// workspace's own when that folder is gone. An entry is named by the key the trash
    /// answered it with, or by the name it had when the trash holds one of that name.
    ///
    /// # Errors
    /// An entry the trash does not hold, a name several in the trash have, or a name taken
    /// again where it comes back to: a restore never writes over another entry.
    pub fn restore_entries(&mut self, entries: &[String]) -> Result<Changed, String> {
        if entries.is_empty() {
            return Err("no entry is named".to_owned());
        }
        let trashed = self.directly_trashed();
        let mut ids: Vec<String> = Vec::new();
        let mut places = BTreeSet::new();
        for entry in entries {
            let id = if trashed.contains(entry) {
                entry.clone()
            } else {
                let mut of_that_name = trashed.iter().filter(|id| {
                    self.node_parent_name(id)
                        .is_some_and(|(_, name)| name.as_bytes() == entry.as_bytes())
                });
                match (of_that_name.next(), of_that_name.next()) {
                    (Some(id), None) => id.clone(),
                    (Some(_), Some(_)) => {
                        return Err(format!(
                            "the trash holds several named {entry:?}: name one by the id the \
                             trash answered"
                        ));
                    }
                    _ => return Err(format!("the trash holds no {entry:?}")),
                }
            };
            if ids.contains(&id) {
                return Err(format!("{entry:?} is named twice"));
            }
            let name = self
                .node_parent_name(&id)
                .map(|(_, name)| name.as_bytes().to_vec())
                .ok_or_else(|| format!("the trash holds no {entry:?}"))?;
            let back = self
                .prev_parent(&id)
                .filter(|parent| parent == ROOT_ID || self.is_visible(parent))
                .unwrap_or_else(|| ROOT_ID.to_owned());
            if self.child_id_named(&back, &name).is_some() || !places.insert((back, name)) {
                return Err(format!(
                    "the name of {entry:?} is taken where it comes back to: rename what is there \
                     first"
                ));
            }
            ids.push(id);
        }
        let before = self.before_change();
        let mut touched = Vec::new();
        for id in ids {
            if !self.restore(&id) {
                return Err("the trash did not give an entry back".to_owned());
            }
            let to = self
                .path_for_id(&id)
                .map(|path| path.trim_start_matches('/').to_owned());
            let folder = self.node_is_dir(&id).unwrap_or(false);
            touched.push(Touched {
                id,
                from: None,
                to,
                folder,
            });
        }
        Ok(Changed {
            writes: self.document_writes(&before),
            entries: touched,
            made: Vec::new(),
        })
    }
}

/// A file [`Namespace::put_files`] puts into the tree.
pub struct FileToPut<'a> {
    /// The folder it goes into, `""` for the tree's root.
    pub to: &'a str,
    /// The name it takes there.
    pub name: &'a str,
    /// The hash of its content, one the tree names files by.
    pub hash: &'a str,
    /// Its length in bytes.
    pub size: u64,
    /// When it was written, as the tree keeps times.
    pub mtime: u64,
}

/// One file [`Namespace::put_files`] put.
pub struct PlacedFile {
    /// Where it is, as a path from the tree's root (`docs/2026/plan.md`).
    pub path: String,
    /// Whether a file of that name was there and took the new content, rather than a node being
    /// made for it.
    pub replaced: bool,
}

fn refused(why: impl Into<String>) -> String {
    why.into()
}

impl Namespace {
    /// Put `files` into the tree as one change: every write a record takes is made once for the
    /// whole set, not once for each file, so a folder of them costs its chunks rather than its
    /// files.
    ///
    /// Every file is checked before any of them is put: a set with one the tree refuses leaves
    /// the tree as it was.
    ///
    /// # Errors
    /// A name no file may have, a file named as a folder on the way, a folder where a file
    /// goes, content that is not a hash, or a file that could not be put.
    pub fn put_files(&mut self, files: &[FileToPut<'_>]) -> Result<Vec<PlacedFile>, String> {
        self.check_put(files)?;
        let mut placed = Vec::with_capacity(files.len());
        for file in files {
            placed.push(self.place(file)?);
        }
        Ok(placed)
    }

    /// Whether every one of `files` can be put, without putting any: the names, the hashes, the
    /// folders on the way (taking the folders and files this same set makes into account), and
    /// the folders where the files go.
    fn check_put(&mut self, files: &[FileToPut<'_>]) -> Result<(), String> {
        let mut made_folders = std::collections::BTreeSet::new();
        let mut made_files = std::collections::BTreeSet::new();
        for file in files {
            let FileToPut { to, name, hash, .. } = *file;
            if !plain(name) {
                return Err(refused(format!("{name:?} is no file's name")));
            }
            if hash_from_hex(hash).is_none() {
                return Err(refused("the content's hash is not one"));
            }
            let mut walked = String::new();
            // The folder's node, while every folder on the way is there already.
            let mut parent = Some(ROOT_INO);
            for folder in to.split('/').filter(|folder| !folder.is_empty()) {
                if !plain(folder) {
                    return Err(refused(format!("{folder:?} is no folder's name")));
                }
                walked = join(&walked, folder);
                if made_files.contains(&walked) {
                    return Err(refused(format!(
                        "{folder} is a file, not a folder to put {name} in"
                    )));
                }
                if made_folders.contains(&walked) {
                    parent = None;
                    continue;
                }
                parent = match parent {
                    Some(at) => match self.lookup(at, folder) {
                        Some(attr) if attr.kind == FileType::Directory => Some(attr.ino.into()),
                        Some(_) => {
                            return Err(refused(format!(
                                "{folder} is a file, not a folder to put {name} in"
                            )));
                        }
                        None => {
                            made_folders.insert(walked.clone());
                            None
                        }
                    },
                    None => None,
                };
            }
            let path = join(&walked, name);
            if made_folders.contains(&path) {
                return Err(refused(format!("a folder named {name} is already there")));
            }
            if let Some(at) = parent
                && let Some(attr) = self.lookup(at, name)
                && attr.kind == FileType::Directory
            {
                return Err(refused(format!("a folder named {name} is already there")));
            }
            made_files.insert(path);
        }
        Ok(())
    }

    /// Put one file into the tree, without the changes: [`Self::put_files`] takes them once for
    /// all of them.
    fn place(&mut self, file: &FileToPut<'_>) -> Result<PlacedFile, String> {
        let FileToPut {
            to,
            name,
            hash,
            size,
            mtime,
        } = *file;
        let hash = hash_from_hex(hash).ok_or_else(|| refused("the content's hash is not one"))?;
        let mut parent = ROOT_INO;
        for folder in to.split('/').filter(|folder| !folder.is_empty()) {
            parent = match self.lookup(parent, folder) {
                Some(attr) if attr.kind == FileType::Directory => attr.ino.into(),
                Some(_) => {
                    return Err(refused(format!(
                        "{folder} is a file, not a folder to put {name} in"
                    )));
                }
                None => self
                    .create_dir(parent, folder, mtime)
                    .ok_or_else(|| refused(format!("the folder {folder} could not be made")))?,
            };
        }
        let replaced = match self.lookup(parent, name) {
            Some(attr) if attr.kind == FileType::Directory => {
                return Err(refused(format!("a folder named {name} is already there")));
            }
            Some(attr) => {
                if !self.set_content(attr.ino.into(), Some(&hash), size, mtime) {
                    return Err(refused(format!("{name} could not take the file")));
                }
                true
            }
            None => {
                self.create_file(parent, name, Some(&hash), size, mtime)
                    .ok_or_else(|| refused(format!("{name} could not be put in the tree")))?;
                false
            }
        };
        Ok(PlacedFile {
            path: join(to, name),
            replaced,
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::namespace::{NodeKind, hash_from_hex};

    fn hash(fill: u8) -> String {
        hex::encode([fill; 32])
    }

    fn listed(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|entry| (*entry).to_owned()).collect()
    }

    /// Put one file into `tree`, as a mount copying it in does.
    fn put(tree: &mut Namespace, to: &str, name: &str, hash: &str, size: u64, mtime: u64) {
        tree.put_files(&[FileToPut {
            to,
            name,
            hash,
            size,
            mtime,
        }])
        .unwrap();
    }

    /// The content the file at `path` names, `None` where no file is.
    fn content_at(tree: &Namespace, path: &str) -> Option<iroh_blobs::Hash> {
        tree.entry_at(path)
            .filter(|entry| entry.kind == NodeKind::File)
            .and_then(|entry| entry.hash)
    }

    fn is_folder(tree: &Namespace, path: &str) -> bool {
        tree.entry_at(path)
            .is_some_and(|entry| entry.kind == NodeKind::Dir)
    }

    #[test]
    fn a_folder_for_a_task_is_made_once_and_files_move_rename_and_copy_by_path() {
        let mut tree = Namespace::new("server").unwrap();
        let made = tree.make_folder("Tasks", "12 Plan the launch", 5).unwrap();
        assert_eq!(made.made, ["Tasks"], "the folder on the way is made too");
        let folder = made.entries.first().unwrap();
        assert_eq!(folder.to.as_deref(), Some("Tasks/12 Plan the launch"));
        assert!(folder.folder && folder.from.is_none());
        assert!(made.writes.iter().any(|write| !write.changes.is_empty()));
        // Asked again, it is the same folder and nothing is written.
        let again = tree.make_folder("Tasks", "12 Plan the launch", 6).unwrap();
        assert!(again.entries.is_empty() && again.made.is_empty());
        assert!(again.writes.iter().all(|write| write.changes.is_empty()));

        put(&mut tree, "", "brief.md", &hash(1), 10, 6);
        put(&mut tree, "", "budget.csv", &hash(2), 20, 6);
        let moved = tree
            .move_entries(
                "",
                &listed(&["brief.md", "budget.csv"]),
                "Tasks/12 Plan the launch",
                true,
                7,
            )
            .unwrap();
        assert_eq!(
            moved
                .entries
                .iter()
                .map(|entry| (entry.from.as_deref(), entry.to.as_deref()))
                .collect::<Vec<_>>(),
            [
                (Some("brief.md"), Some("Tasks/12 Plan the launch/brief.md")),
                (
                    Some("budget.csv"),
                    Some("Tasks/12 Plan the launch/budget.csv")
                ),
            ]
        );
        assert!(content_at(&tree, "brief.md").is_none(), "moved, not copied");
        assert_eq!(
            content_at(&tree, "Tasks/12 Plan the launch/budget.csv"),
            hash_from_hex(&hash(2))
        );

        let renamed = tree
            .rename_entry("Tasks/12 Plan the launch", "brief.md", "Brief v2.md", 8)
            .unwrap();
        assert_eq!(
            renamed.entries.first().unwrap().to.as_deref(),
            Some("Tasks/12 Plan the launch/Brief v2.md")
        );
        assert_eq!(
            renamed.entries.first().unwrap().id,
            moved.entries.first().unwrap().id,
            "a renamed file is the same node"
        );

        // A folder is copied with all it holds, into a folder made on the way.
        let copied = tree
            .copy_entries("Tasks", &listed(&["12 Plan the launch"]), "Archive/2026", 9)
            .unwrap();
        assert_eq!(copied.made, ["Archive", "Archive/2026"]);
        assert!(is_folder(&tree, "Archive/2026/12 Plan the launch"));
        assert_eq!(
            content_at(&tree, "Archive/2026/12 Plan the launch/Brief v2.md"),
            hash_from_hex(&hash(1))
        );
        assert_eq!(
            content_at(&tree, "Tasks/12 Plan the launch/Brief v2.md"),
            hash_from_hex(&hash(1)),
            "what was copied is still where it was"
        );
        // A copy beside what it copies keeps both.
        let beside = tree
            .copy_entries(
                "Tasks/12 Plan the launch",
                &listed(&["budget.csv"]),
                "Tasks/12 Plan the launch",
                9,
            )
            .unwrap();
        assert_eq!(
            beside.entries.first().unwrap().to.as_deref(),
            Some("Tasks/12 Plan the launch/budget (2).csv")
        );
        assert_eq!(
            content_at(&tree, "Tasks/12 Plan the launch/budget (2).csv"),
            hash_from_hex(&hash(2))
        );
    }

    #[test]
    fn the_trash_gives_back_what_it_took_to_where_it_was() {
        let mut tree = Namespace::new("server").unwrap();
        put(&mut tree, "Tasks/7", "notes.md", &hash(3), 4, 1);
        let gone = tree.trash_entries("Tasks", &listed(&["7"])).unwrap();
        let taken = gone.entries.first().unwrap().clone();
        assert_eq!(taken.from.as_deref(), Some("Tasks/7"));
        assert!(taken.to.is_none() && taken.folder);
        assert!(!is_folder(&tree, "Tasks/7"));
        assert!(
            content_at(&tree, "Tasks/7/notes.md").is_none(),
            "what the folder holds goes with it"
        );

        // By the key the trash answered.
        let back = tree.restore_entries(&[taken.id]).unwrap();
        assert_eq!(back.entries.first().unwrap().to.as_deref(), Some("Tasks/7"));
        assert_eq!(
            content_at(&tree, "Tasks/7/notes.md"),
            hash_from_hex(&hash(3))
        );

        // By the name it had.
        tree.trash_entries("Tasks/7", &listed(&["notes.md"]))
            .unwrap();
        tree.restore_entries(&listed(&["notes.md"])).unwrap();
        assert_eq!(
            content_at(&tree, "Tasks/7/notes.md"),
            hash_from_hex(&hash(3))
        );

        // A name taken again meanwhile is not written over.
        tree.trash_entries("Tasks/7", &listed(&["notes.md"]))
            .unwrap();
        put(&mut tree, "Tasks/7", "notes.md", &hash(4), 4, 2);
        assert!(tree.restore_entries(&listed(&["notes.md"])).is_err());
        assert_eq!(
            content_at(&tree, "Tasks/7/notes.md"),
            hash_from_hex(&hash(4))
        );
        assert!(tree.restore_entries(&listed(&["never.md"])).is_err());
    }

    #[test]
    fn a_path_stays_in_the_workspace_and_a_refused_verb_changes_nothing() {
        let mut tree = Namespace::new("server").unwrap();
        put(&mut tree, "a", "one.txt", &hash(5), 1, 1);
        put(&mut tree, "b", "one.txt", &hash(6), 1, 1);
        for outside in ["/a", "a/../b", "./a", "a/..", ".."] {
            assert!(tree.make_folder(outside, "x", 2).is_err(), "{outside:?}");
            assert!(
                tree.trash_entries(outside, &listed(&["one.txt"])).is_err(),
                "{outside:?}"
            );
            assert!(
                tree.move_entries("a", &listed(&["one.txt"]), outside, true, 2)
                    .is_err(),
                "{outside:?}"
            );
            assert!(
                tree.copy_entries("a", &listed(&[outside]), "b", 2).is_err(),
                "{outside:?}"
            );
        }
        assert!(tree.make_folder("a", "..", 2).is_err());
        assert!(
            tree.rename_entry("a", "one.txt", "b/one.txt", 2).is_err(),
            "a name holds no /"
        );
        assert!(
            tree.move_entries("a", &listed(&["one.txt"]), "b", true, 2)
                .is_err(),
            "a move writes over nothing"
        );
        assert!(
            tree.rename_entry("", "a", "b", 2).is_err(),
            "a rename writes over nothing"
        );
        assert!(
            tree.move_entries("", &listed(&["a"]), "a/inner", true, 2)
                .is_err(),
            "a folder does not go into itself"
        );
        assert!(tree.copy_entries("", &listed(&["a"]), "a", 2).is_err());
        assert!(
            tree.make_folder("a/one.txt", "x", 2).is_err(),
            "a file is in the way"
        );
        assert!(
            tree.move_entries("a", &listed(&["one.txt", "missing.txt"]), "c", true, 2)
                .is_err(),
            "one entry that is not there refuses them all"
        );
        assert!(
            tree.move_entries("a", &listed(&["one.txt"]), "c", false, 2)
                .is_err(),
            "a folder that is not there is made only when asked"
        );
        assert!(
            tree.trash_entries("", &listed(&["a", "a/one.txt"]))
                .is_err(),
            "a folder goes with all it holds, once"
        );

        assert!(!is_folder(&tree, "c") && !is_folder(&tree, "a/inner"));
        assert!(!is_folder(&tree, "a/x") && !is_folder(&tree, "x"));
        assert_eq!(content_at(&tree, "a/one.txt"), hash_from_hex(&hash(5)));
        assert_eq!(content_at(&tree, "b/one.txt"), hash_from_hex(&hash(6)));
    }
}
