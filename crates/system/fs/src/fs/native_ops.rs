//! Path-level record operations for the native data plane.
//!
//! On Tier 1 the workspace is a real tree on the store volume and nothing
//! passes through FUSE. Changes are observed afterwards, as ordered
//! changesets of paths, and applied here through the same signed
//! operations a FUSE `create`, `write`+`release`, `mkdir`, `unlink` and
//! `rmdir` use: record bindings and envelopes, versions, history and the
//! namespace delta broadcast to peers. The FUSE callbacks and these entry
//! points share one implementation, so a record written through either
//! path is indistinguishable to a peer.

use std::path::{Component, Path};

use super::{EIO, EISDIR, ENOENT, ENOTDIR, FileType, Hash, QuickFS, now_ms};
use crate::namespace::{ContentAuthority, PosixAttrs, ROOT_INO, WriteLease};

/// One file or directory of the workspace tree as the record plane sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// Root-relative path components, raw bytes.
    pub path: Vec<Vec<u8>>,
    pub dir: bool,
    /// Content id of a file's current version (`None` for empty files and
    /// directories).
    pub hash: Option<Hash>,
    pub size: u64,
    /// Time of the version that set the current content, Unix ms. The
    /// author assigns it when the change is committed, so it orders one
    /// author's changes the way that author made them.
    pub mtime_ms: u64,
    /// Whether this device holds the verified content revision. A signed
    /// record whose revision has not been verified yet reads as empty; it is
    /// not empty, just not known.
    pub known: bool,
    /// The stable record this path is. A rename keeps it.
    pub record: Option<collab_types::FileRecordId>,
    /// Replicated permission bits and whether the content is a symbolic
    /// link's target.
    pub posix: PosixAttrs,
}

/// Splits a root-relative path into its name components. Anything that is
/// not a plain name (`..`, a root, a prefix) is refused.
///
/// # Errors
///
/// `EINVAL` for a component that is not a plain name.
pub fn path_components(rel: &Path) -> Result<Vec<Vec<u8>>, i32> {
    use std::os::unix::ffi::OsStrExt as _;
    rel.components()
        .map(|c| match c {
            Component::Normal(name) => Ok(name.as_bytes().to_vec()),
            Component::CurDir => Ok(Vec::new()),
            _ => Err(libc::EINVAL),
        })
        .filter(|c| !matches!(c, Ok(v) if v.is_empty()))
        .collect()
}

/// The published POSIX attributes of the record at `ino`, else the default.
fn posix_of(ns: &crate::namespace::Namespace, ino: u64, dir: bool) -> PosixAttrs {
    ns.record_id_for(ino)
        .and_then(|r| ns.posix_attrs(r))
        .unwrap_or_else(|| PosixAttrs::default_for(dir))
}

fn to_errno(error: &std::io::Error) -> i32 {
    error.raw_os_error().unwrap_or(EIO)
}

impl QuickFS {
    fn lookup_kind(&self, parent: u64, name: &[u8]) -> Option<(u64, FileType)> {
        self.ns()
            .lookup(parent, name)
            .map(|attr| (attr.ino.0, attr.kind))
    }

    /// The directory at `components`, creating missing directories. A file
    /// standing where a directory must be is removed first: the changeset
    /// that made the path a directory removed it on the author's side too.
    fn ensure_dir_components(&self, components: &[Vec<u8>]) -> Result<u64, i32> {
        let mut cur = ROOT_INO;
        for name in components {
            cur = match self.lookup_kind(cur, name) {
                Some((ino, FileType::Directory)) => ino,
                Some(_) => {
                    self.delete_named(cur, name, false)?;
                    let (ino, delta) = self.create_empty_dir(cur, name)?;
                    self.publish(delta);
                    ino
                }
                None => {
                    let (ino, delta) = self.create_empty_dir(cur, name)?;
                    self.publish(delta);
                    ino
                }
            };
        }
        Ok(cur)
    }

    /// Applies a locally captured directory with its default mode.
    ///
    /// # Errors
    ///
    /// The errno of the directory creation that failed.
    pub fn apply_local_dir(&self, rel: &Path) -> Result<(), i32> {
        self.apply_local_dir_as(rel, PosixAttrs::default_for(true))
    }

    /// Applies a locally captured directory with mode bits `attrs`.
    ///
    /// # Errors
    ///
    /// The errno of the directory creation that failed.
    pub fn apply_local_dir_as(&self, rel: &Path, attrs: PosixAttrs) -> Result<(), i32> {
        let components = path_components(rel)?;
        if components.is_empty() {
            return Ok(());
        }
        let ino = self.ensure_dir_components(&components)?;
        self.set_posix(ino, attrs, true)
    }

    /// Publishes `attrs` for the record at `ino` when they differ from what
    /// it has.
    fn set_posix(&self, ino: u64, attrs: PosixAttrs, dir: bool) -> Result<(), i32> {
        let delta = {
            let mut ns = self.ns();
            let record = ns.record_id_for(ino).ok_or(EIO)?;
            let current = ns
                .posix_attrs(record)
                .unwrap_or_else(|| PosixAttrs::default_for(dir));
            if current == attrs {
                return Ok(());
            }
            if !ns.put_posix_attrs(record, &attrs) {
                return Err(EIO);
            }
            ns.save_incremental()
        };
        self.publish(delta);
        Ok(())
    }

    /// Applies a locally captured file version with content `data`, written
    /// at `mtime_ms` (the file's own modification time, which peers
    /// materialise). Nothing
    /// is published when the record already has exactly this content.
    ///
    /// # Errors
    ///
    /// The errno of the record operation that failed (`EDQUOT` when the
    /// record's history is full, `EIO` when publication failed).
    pub fn apply_local_file(&self, rel: &Path, data: &[u8], mtime_ms: u64) -> Result<(), i32> {
        self.apply_local_file_as(rel, data, mtime_ms, PosixAttrs::default_for(false))
    }

    /// [`Self::apply_local_file`] with the file's mode bits, or for a
    /// symbolic link (`attrs.symlink`) whose target bytes are `data`. The
    /// attributes are published before the content, so a peer never shows
    /// the new content with the old type or mode.
    ///
    /// # Errors
    ///
    /// As [`Self::apply_local_file`].
    pub fn apply_local_file_as(
        &self,
        rel: &Path,
        data: &[u8],
        mtime_ms: u64,
        attrs: PosixAttrs,
    ) -> Result<(), i32> {
        let components = path_components(rel)?;
        let Some((name, parents)) = components.split_last() else {
            return Err(EISDIR);
        };
        let parent = self.ensure_dir_components(parents)?;
        let ino = match self.lookup_kind(parent, name) {
            Some((ino, FileType::RegularFile)) => ino,
            Some((_, FileType::Directory)) => {
                self.remove_subtree(parent, name)?;
                let (ino, delta) = self.create_empty_file_stamped(parent, name, mtime_ms)?;
                self.publish(delta);
                ino
            }
            Some(_) => return Err(libc::EINVAL),
            None => {
                let (ino, delta) = self.create_empty_file_stamped(parent, name, mtime_ms)?;
                self.publish(delta);
                ino
            }
        };
        self.set_posix(ino, attrs, false)?;
        self.commit_content_at(ino, data, mtime_ms)
            .map_err(|e| to_errno(&e))
    }

    /// Applies a locally captured removal. Removing a path that is already
    /// gone succeeds: the change it describes holds.
    ///
    /// # Errors
    ///
    /// The errno of the signed delete that failed.
    pub fn apply_local_remove(&self, rel: &Path) -> Result<(), i32> {
        let components = path_components(rel)?;
        let Some((name, parents)) = components.split_last() else {
            return Err(libc::EBUSY);
        };
        let mut parent = ROOT_INO;
        for segment in parents {
            match self.lookup_kind(parent, segment) {
                Some((ino, FileType::Directory)) => parent = ino,
                _ => return Ok(()),
            }
        }
        match self.lookup_kind(parent, name) {
            None => Ok(()),
            Some((_, FileType::Directory)) => self.remove_subtree(parent, name),
            Some(_) => match self.delete_named(parent, name, false) {
                Err(ENOENT) => Ok(()),
                other => other,
            },
        }
    }

    /// Applies a locally captured rename of `from` to `to`: the record keeps
    /// its identity, so its history, access and every reference to it follow
    /// it, and a directory brings its subtree. A record already at `to` is
    /// replaced as `rename(2)` replaces it (one signed transaction).
    ///
    /// # Errors
    ///
    /// `ENOENT` when `from` is not a record here, else the errno of the
    /// signed rename that failed. The caller falls back to a removal and a
    /// creation, which loses identity but never content.
    pub fn apply_local_move(&self, from: &Path, to: &Path) -> Result<(), i32> {
        let source = path_components(from)?;
        let target = path_components(to)?;
        let (Some((name, parents)), Some((new_name, new_parents))) =
            (source.split_last(), target.split_last())
        else {
            return Err(libc::EBUSY);
        };
        if target.starts_with(&source) {
            return Err(libc::EINVAL);
        }
        let parent = self.ino_at(parents).ok_or(ENOENT)?;
        if self.lookup_kind(parent, name).is_none() {
            return Err(ENOENT);
        }
        let new_parent = self.ensure_dir_components(new_parents)?;
        let delta = self.rename_named(parent, name, new_parent, new_name)?;
        self.publish(delta);
        Ok(())
    }

    /// Removes a directory and everything below it, deepest first, each as
    /// its own signed delete.
    fn remove_subtree(&self, parent: u64, name: &[u8]) -> Result<(), i32> {
        let Some((ino, kind)) = self.lookup_kind(parent, name) else {
            return Ok(());
        };
        if kind != FileType::Directory {
            return Err(ENOTDIR);
        }
        let children = self.ns().readdir(ino).unwrap_or_default();
        for (_, child_kind, child) in children {
            let child = child.as_bytes();
            if child == b"." || child == b".." {
                continue;
            }
            if child_kind == FileType::Directory {
                self.remove_subtree(ino, child)?;
            } else {
                match self.delete_named(ino, child, false) {
                    Ok(()) | Err(ENOENT) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        match self.delete_named(parent, name, true) {
            Ok(()) | Err(ENOENT) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Every file and directory this mount may read, parents before
    /// children.
    pub fn tree_snapshot(&self) -> Vec<TreeEntry> {
        let mut out = Vec::new();
        let mut stack: Vec<(u64, Vec<Vec<u8>>)> = vec![(ROOT_INO, Vec::new())];
        while let Some((dir, prefix)) = stack.pop() {
            let entries = self.visible_entries(dir).unwrap_or_default();
            for (ino, kind, name) in entries {
                let name = name.as_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                let mut path = prefix.clone();
                path.push(name.to_vec());
                let (hash, size, mtime_ms, known, record, posix) = {
                    let ns = self.ns();
                    let attr = ns.getattr(ino);
                    let mtime_ms = attr
                        .as_ref()
                        .and_then(|a| a.mtime.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
                    let known = kind == FileType::Directory
                        || ns.content_authority() == ContentAuthority::LegacyNamespace
                        || ns.content_revision_for(ino).is_some();
                    (
                        ns.hash_for(ino),
                        attr.map_or(0, |a| a.size),
                        mtime_ms,
                        known,
                        ns.record_id_for(ino),
                        posix_of(&ns, ino, kind == FileType::Directory),
                    )
                };
                let dir_entry = kind == FileType::Directory;
                out.push(TreeEntry {
                    path: path.clone(),
                    dir: dir_entry,
                    hash,
                    size,
                    mtime_ms,
                    known,
                    record,
                    posix,
                });
                if dir_entry {
                    stack.push((ino, path));
                }
            }
        }
        out
    }

    /// The current time as the record plane stamps it.
    pub fn record_clock_ms() -> u64 {
        now_ms()
    }

    fn ino_at(&self, components: &[Vec<u8>]) -> Option<u64> {
        let mut cur = ROOT_INO;
        for name in components {
            cur = self.lookup_kind(cur, name)?.0;
        }
        Some(cur)
    }

    /// The record plane's current state of one path.
    pub fn entry_at(&self, rel: &Path) -> Option<TreeEntry> {
        let components = path_components(rel).ok()?;
        let ino = self.ino_at(&components)?;
        let ns = self.ns();
        let attr = ns.getattr(ino)?;
        let mtime_ms = attr
            .mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let known = attr.kind == FileType::Directory
            || ns.content_authority() == ContentAuthority::LegacyNamespace
            || ns.content_revision_for(ino).is_some();
        Some(TreeEntry {
            path: components,
            dir: attr.kind == FileType::Directory,
            hash: ns.hash_for(ino),
            size: attr.size,
            mtime_ms,
            known,
            record: ns.record_id_for(ino),
            posix: posix_of(&ns, ino, attr.kind == FileType::Directory),
        })
    }

    /// Takes or renews the write lease of the file at `rel` for device `me`
    /// and broadcasts the change. `Err(Some(lease))` is another device's
    /// live lease; `Err(None)` means there is no such file.
    ///
    /// # Errors
    ///
    /// Another holder's live lease, or `None` when there is no such file.
    pub fn take_write_lease(
        &self,
        rel: &Path,
        me: [u8; 32],
        ttl: std::time::Duration,
    ) -> Result<WriteLease, Option<WriteLease>> {
        let components = path_components(rel).map_err(|_| None)?;
        let ino = self.ino_at(&components).ok_or(None)?;
        let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        let delta = {
            let mut ns = self.ns();
            let record = ns.record_id_for(ino).ok_or(None)?;
            let current = ns.write_lease(record);
            let lease = WriteLease::take(current, me, now_ms(), ttl_ms).map_err(Some)?;
            if current == Some(lease) {
                return Ok(lease);
            }
            if !ns.put_write_lease(record, &lease) {
                return Err(None);
            }
            (lease, ns.save_incremental())
        };
        self.publish(delta.1);
        Ok(delta.0)
    }

    /// Releases `me`'s lease of `rel`: it keeps its holder and epoch and
    /// lapses now, so the next writer takes it with a new epoch.
    pub fn release_write_lease(&self, rel: &Path, me: [u8; 32], now: u64) {
        let Ok(components) = path_components(rel) else {
            return;
        };
        let Some(ino) = self.ino_at(&components) else {
            return;
        };
        let delta = {
            let mut ns = self.ns();
            let Some(record) = ns.record_id_for(ino) else {
                return;
            };
            match ns.write_lease(record) {
                Some(lease) if lease.holder == me && lease.is_live(now) => {
                    let released = WriteLease {
                        expires_ms: now,
                        ..lease
                    };
                    if !ns.put_write_lease(record, &released) {
                        return;
                    }
                    ns.save_incremental()
                }
                _ => return,
            }
        };
        self.publish(delta);
    }

    /// Every write lease on a file this mount can see, by path.
    pub fn write_leases_by_path(&self) -> Vec<(std::path::PathBuf, WriteLease)> {
        let leases = self.ns().write_leases();
        leases
            .into_iter()
            .filter_map(|(record, lease)| {
                let path = {
                    let ns = self.ns();
                    let ino = ns.ino_for_record_id(record)?;
                    ns.path_for(ino)?
                };
                let rel = std::path::PathBuf::from(path.trim_start_matches('/'));
                Some((rel, lease))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_are_accepted() {
        assert_eq!(
            path_components(Path::new("a/./b")).unwrap(),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
        assert!(path_components(Path::new("../x")).is_err());
        assert!(path_components(Path::new("/abs")).is_err());
        assert!(path_components(Path::new("")).unwrap().is_empty());
    }
}
