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

use super::{EIO, EISDIR, ENOENT, ENOTDIR, FileType, Hash, QuickFS, document_codec_for, now_ms};
use crate::namespace::{
    ContentAuthority, LeaseRequest, PosixAttrs, ROOT_INO, VersionFence, WriteLease,
};
use tilo_lock::MutexRecover as _;

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
    pub record: Option<fs_protocol::FileRecordId>,
    /// Replicated permission bits and whether the content is a symbolic
    /// link's target.
    pub posix: PosixAttrs,
    /// The record's write lease, when it is lease-class.
    pub lease: Option<WriteLease>,
    /// The tenancy the current version was written in, and who held the
    /// lease in that epoch.
    pub fence: Option<VersionFence>,
    pub fence_holder: Option<[u8; 32]>,
}

/// How a write lease was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseSource {
    /// Taken or renewed here: no other device arbitrates this room, or this
    /// device does.
    Local,
    Granted,
    /// Taken without an answer from the lease authority, which could not be
    /// reached: versions written under it lose to a granted tenancy.
    Optimistic,
}

/// Splits a root-relative path into its name components. Anything that is
/// not a plain name (`..`, a root, a prefix) is refused.
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

/// The write lease of the record at `ino`, the tenancy its current version
/// was written in, and who held that tenancy.
fn lease_of(
    ns: &crate::namespace::Namespace,
    ino: u64,
) -> (Option<WriteLease>, Option<VersionFence>, Option<[u8; 32]>) {
    let Some(record) = ns.record_id_for(ino) else {
        return (None, None, None);
    };
    let Some(lease) = ns.write_lease(record) else {
        return (None, None, None);
    };
    let fence = ns
        .content_revision_for(ino)
        .and_then(|r| ns.version_fence(record, r.content_hash.map(|h| h.0), r.byte_len));
    let holder = fence.and_then(|f| ns.lease_holder(record, f.epoch));
    (Some(lease), fence, holder)
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
        self.ns().lookup_kind(parent, name)
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
    pub fn apply_local_file(&self, rel: &Path, data: &[u8], mtime_ms: u64) -> Result<(), i32> {
        self.apply_local_file_as(rel, data, mtime_ms, PosixAttrs::default_for(false), None)
    }

    /// [`Self::apply_local_file`] with the file's mode bits, or for a
    /// symbolic link (`attrs.symlink`) whose target bytes are `data`. The
    /// attributes are published before the content, so a peer never shows
    /// the new content with the old type or mode. A version written under a
    /// write lease carries its tenancy (`fence`), published in the same
    /// change as the content.
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
        fence: Option<VersionFence>,
    ) -> Result<(), i32> {
        self.apply_local_version(
            rel,
            super::VersionSource::Bytes(data),
            mtime_ms,
            attrs,
            fence,
        )
    }

    /// Applies a locally captured file version whose content is the file at
    /// `snapshot` (immutable while this runs), described by `manifest` when
    /// the capture already computed it. Large content is cloned into the
    /// content store rather than read into memory. `attrs` and `fence` as
    /// for [`Self::apply_local_file_as`].
    ///
    /// # Errors
    ///
    /// As [`Self::apply_local_file`].
    pub fn apply_local_snapshot(
        &self,
        rel: &Path,
        snapshot: &Path,
        manifest: Option<std::sync::Arc<crate::content::Manifest>>,
        mtime_ms: u64,
        attrs: PosixAttrs,
        fence: Option<VersionFence>,
    ) -> Result<(), i32> {
        self.apply_local_version(
            rel,
            super::VersionSource::File {
                path: snapshot,
                manifest,
            },
            mtime_ms,
            attrs,
            fence,
        )
    }

    fn apply_local_version(
        &self,
        rel: &Path,
        source: super::VersionSource<'_>,
        mtime_ms: u64,
        attrs: PosixAttrs,
        fence: Option<VersionFence>,
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
                return self.create_local_file(parent, name, source, mtime_ms, attrs, fence);
            }
            Some(_) => return Err(libc::EINVAL),
            None => return self.create_local_file(parent, name, source, mtime_ms, attrs, fence),
        };
        self.set_posix(ino, attrs, false)?;
        self.commit_version_at(ino, source, mtime_ms, fence)
            .map_err(|e| to_errno(&e))
    }

    /// A new file is published with its content as its first version and
    /// its attributes in the same changeset. A version written under a
    /// lease starts empty, so that its content carries the tenancy.
    fn create_local_file(
        &self,
        parent: u64,
        name: &[u8],
        source: super::VersionSource<'_>,
        mtime_ms: u64,
        attrs: PosixAttrs,
        fence: Option<VersionFence>,
    ) -> Result<(), i32> {
        if fence.is_some() {
            let (ino, delta) = self.create_empty_file_stamped(parent, name, mtime_ms)?;
            self.publish(delta);
            self.set_posix(ino, attrs, false)?;
            return self
                .commit_version_at(ino, source, mtime_ms, fence)
                .map_err(|e| to_errno(&e));
        }
        let (hash, byte_len) = self
            .store_version_blocking(source, None)
            .map_err(|e| to_errno(&e))?
            .map_or((None, 0), |(hash, len)| (Some(hash), len));
        let (ino, delta) = self.create_file_stamped(parent, name, mtime_ms, hash, byte_len)?;
        self.publish(delta);
        self.set_posix(ino, attrs, false)
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
    /// children. Taken while no changeset is being applied, so it holds
    /// each changeset entirely or not at all.
    pub fn tree_snapshot(&self) -> Vec<TreeEntry> {
        let _gate = self
            .record_publisher
            .as_ref()
            .map(|p| p.repository().publication_gate().blocking_read());
        let mut out = Vec::new();
        let mut stack: Vec<(u64, Vec<Vec<u8>>)> = vec![(ROOT_INO, Vec::new())];
        while let Some((dir, prefix)) = stack.pop() {
            let entries = self.visible_entries(dir).unwrap_or_default();
            for (ino, kind, name) in entries {
                let name = name.as_bytes();
                if !projectable_name(name) {
                    tracing::warn!(
                        target: "fs::native",
                        ino,
                        "not projecting an entry whose name is not one path component",
                    );
                    continue;
                }
                let mut path = prefix.clone();
                path.push(name.to_vec());
                let (hash, size, mtime_ms, known, record, posix, lease) = {
                    let ns = self.ns();
                    let (attr, hash) = ns.attr_and_hash(ino).unzip();
                    let mtime_ms = attr
                        .as_ref()
                        .and_then(|a| a.mtime.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
                    let known = if kind == FileType::Directory {
                        self.directory_known(&ns, ino)
                    } else {
                        ns.content_authority() == ContentAuthority::LegacyNamespace
                            || ns.content_revision_for(ino).is_some()
                    };
                    (
                        hash.flatten(),
                        attr.map_or(0, |a| a.size),
                        mtime_ms,
                        known,
                        ns.record_id_for(ino),
                        posix_of(&ns, ino, kind == FileType::Directory),
                        lease_of(&ns, ino),
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
                    lease: lease.0,
                    fence: lease.1,
                    fence_holder: lease.2,
                });
                if dir_entry {
                    stack.push((ino, path));
                }
            }
        }
        out
    }

    /// Publishes `device`'s changeset mark (see
    /// [`crate::namespace::ChangesetMark`]).
    pub fn put_changeset_mark(&self, device: [u8; 32], mark: &crate::namespace::ChangesetMark) {
        let delta = {
            let mut ns = self.ns();
            if !ns.put_changeset_mark(device, mark) {
                return;
            }
            ns.save_incremental()
        };
        self.publish(delta);
    }

    /// Every device's latest changeset mark.
    pub fn changeset_marks(&self) -> Vec<([u8; 32], crate::namespace::ChangesetMark)> {
        self.ns().changeset_marks()
    }

    /// Whether this device holds the signed record of the directory at
    /// `ino`. Its placement can arrive before the changeset that made it;
    /// until the changeset is here, the directory is not shown either.
    fn directory_known(&self, ns: &crate::namespace::Namespace, ino: u64) -> bool {
        let Some(publisher) = &self.record_publisher else {
            return true;
        };
        if ns.content_authority() == ContentAuthority::LegacyNamespace {
            return true;
        }
        ns.record_id_for(ino).is_some_and(|record_id| {
            publisher
                .repository()
                .contains(crate::record_repository::RecordRepositoryKey {
                    workspace_id: publisher.workspace_id(),
                    record_id,
                })
        })
    }

    /// Runs `apply` (a changeset's worth of the path-level operations
    /// above) as one signed changeset, made durable at once:
    ///
    /// 1. the content of every file is synced once, not once per file;
    /// 2. the history mutations are written in one log write;
    /// 3. every record operation is signed once as a changeset and written
    ///    with it in one log transaction: after a crash either all of the
    ///    changeset is there or none of it;
    /// 4. the durability hook persists the namespace that places the
    ///    changeset's records, so a crash cannot leave durable records
    ///    without placements (which a restart would read as deletions).
    ///
    /// Until then peers see none of it: its records are served only once
    /// durable, namespace exchanges wait on the publication gate, and its
    /// namespace deltas are broadcast after the last step.
    pub fn changeset<T>(&self, apply: impl FnOnce() -> T) -> T {
        let publisher = self.record_publisher.as_ref();
        let gate = publisher.map(|p| p.repository().publication_gate().blocking_write());
        if let Some(publisher) = publisher
            && let Err(error) = publisher.begin_changeset()
        {
            eprintln!("records are published one by one: {error}");
        }
        *self.deferred_deltas.lock_recover() = Some(Vec::new());
        self.ns().begin_incremental_batch();
        let store = std::sync::Arc::clone(&self.hydrator.store);
        store.defer_sync();
        let value = apply();
        if let Err(error) = self.hydrator.block_on(store.end_deferred_sync()) {
            eprintln!("content stays pending: {error:#}");
        }
        let mut durable = true;
        if durable && let Some(publisher) = publisher {
            match publisher.commit_changeset() {
                Ok(Some(changeset)) if std::env::var_os("TILO_FS_NATIVE_TRACE").is_some() => {
                    eprintln!(
                        "  changeset {} of {} record(s) is durable",
                        changeset.header.sequence, changeset.header.leaf_count
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("the changeset stays pending: {error}");
                    durable = false;
                }
            }
        }
        // Taken before the namespace is persisted: a full save moves the
        // incremental cursor past the batch.
        let batch = self.ns().end_incremental_batch();
        if durable
            && let Some(hook) = self.durability_hook.get()
            && let Err(error) = hook()
        {
            eprintln!("the namespace placing the changeset stays pending: {error}");
        }
        drop(gate);
        let mut deltas = self
            .deferred_deltas
            .lock_recover()
            .take()
            .unwrap_or_default();
        deltas.push(batch);
        if durable {
            self.broadcast_deltas(deltas);
        } else {
            // Not durable yet: the next changeset broadcasts these with its
            // own, once both are on disk.
            *self.deferred_deltas.lock_recover() = None;
            self.held_deltas.lock_recover().extend(deltas);
        }
        value
    }

    fn broadcast_deltas(&self, deltas: Vec<Vec<u8>>) {
        let held = std::mem::take(&mut *self.held_deltas.lock_recover());
        let any = !held.is_empty() || !deltas.is_empty();
        if any && self.enforcement.is_some() {
            self.refresh_acl_visible_record_index();
        }
        if let Some(sync) = &self.sync {
            for delta in held.into_iter().chain(deltas) {
                sync.broadcast_delta(delta);
            }
        }
        if any {
            self.wake_pinned_hydration();
        }
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
        let (attr, hash) = ns.attr_and_hash(ino)?;
        let mtime_ms = attr
            .mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let known = if attr.kind == FileType::Directory {
            self.directory_known(&ns, ino)
        } else {
            ns.content_authority() == ContentAuthority::LegacyNamespace
                || ns.content_revision_for(ino).is_some()
        };
        Some(TreeEntry {
            path: components,
            dir: attr.kind == FileType::Directory,
            hash,
            size: attr.size,
            mtime_ms,
            known,
            record: ns.record_id_for(ino),
            posix: posix_of(&ns, ino, attr.kind == FileType::Directory),
            lease: lease_of(&ns, ino).0,
            fence: lease_of(&ns, ino).1,
            fence_holder: lease_of(&ns, ino).2,
        })
    }

    /// Whether the file at `rel` is a document: kept as a CRDT and read and
    /// written in the legacy format its name declares.
    pub fn document_at(&self, rel: &Path) -> bool {
        path_components(rel)
            .ok()
            .and_then(|components| self.ino_at(&components))
            .is_some_and(|ino| self.is_document(ino))
    }

    /// The whole file at `rel` as a reader of the mount sees it: a
    /// document exported through its format's codec, other content hydrated
    /// first when it is not local.
    pub fn read_path(&self, rel: &Path) -> Result<Vec<u8>, i32> {
        let components = path_components(rel)?;
        let ino = self.ino_at(&components).ok_or(ENOENT)?;
        let attr = self.ns().getattr(ino);
        if attr.is_some_and(|attr| attr.kind == FileType::Directory) {
            return Err(EISDIR);
        }
        self.read_full_blocking(ino).map_err(|e| to_errno(&e))
    }

    /// Up to `len` bytes of the file at `rel` from `offset`, as
    /// [`Self::read_path`] reads it; only that range is hydrated.
    ///
    /// # Errors
    ///
    /// As [`Self::read_path`].
    pub fn read_path_range(&self, rel: &Path, offset: u64, len: u64) -> Result<Vec<u8>, i32> {
        let components = path_components(rel)?;
        let ino = self.ino_at(&components).ok_or(ENOENT)?;
        // One namespace lock for the inode's kind, content and document-ness.
        let view = self.inode_view(ino, false).ok_or(ENOENT)?;
        if view.attr.kind == FileType::Directory {
            return Err(EISDIR);
        }
        if let Some(stage) = self.stage(ino) {
            return self
                .read_stage_blocking(&stage, offset, len)
                .map_err(|e| to_errno(&e));
        }
        if view.document {
            let whole = self.read_full_blocking(ino).map_err(|e| to_errno(&e))?;
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(whole.len());
            let end = usize::try_from(len)
                .map_or(whole.len(), |len| start.saturating_add(len))
                .min(whole.len());
            return Ok(whole.get(start..end).unwrap_or_default().to_vec());
        }
        let Some(hash) = view.hash else {
            return Ok(Vec::new());
        };
        self.hydrator
            .block_on(self.hydrator.read(hash, offset, len))
            .map_err(|_| EIO)
    }

    /// The entries of the directory at `rel`, as [`Self::tree_snapshot`]
    /// describes them.
    pub fn list_path(&self, rel: &Path) -> Result<Vec<TreeEntry>, i32> {
        use std::os::unix::ffi::OsStrExt as _;
        let components = path_components(rel)?;
        let ino = self.ino_at(&components).ok_or(ENOENT)?;
        if self.lookup_kind_of(ino) != Some(FileType::Directory) {
            return Err(ENOTDIR);
        }
        let entries = self.visible_entries(ino).unwrap_or_default();
        Ok(entries
            .into_iter()
            .filter(|(_, _, name)| name != "." && name != "..")
            .filter_map(|(_, _, name)| {
                let name = std::ffi::OsStr::from_bytes(name.as_bytes());
                self.entry_at(&rel.join(name))
            })
            .collect())
    }

    fn lookup_kind_of(&self, ino: u64) -> Option<FileType> {
        self.ns().getattr(ino).map(|attr| attr.kind)
    }

    /// Writes the file at `snapshot` (immutable while this runs) as the
    /// whole file at `rel`, as a create or an open, write and close of the
    /// mount would: a document is imported through its format's codec (an
    /// invalid document is refused and the current one kept), a new file
    /// named for a registered format becomes a document where the namespace
    /// keeps documents, and anything else is content, applied as
    /// [`Self::apply_local_snapshot`].
    ///
    /// # Errors
    ///
    /// `EINVAL` for a document its codec refuses, else as
    /// [`Self::apply_local_file`].
    pub fn write_path(&self, rel: &Path, snapshot: &Path, mtime_ms: u64) -> Result<(), i32> {
        let components = path_components(rel)?;
        let Some((name, parents)) = components.split_last() else {
            return Err(EISDIR);
        };
        let parent = self.ensure_dir_components(parents)?;
        let read = || std::fs::read(snapshot).map_err(|e| to_errno(&e));
        match self.lookup_kind(parent, name) {
            Some((ino, FileType::RegularFile)) if self.is_document(ino) => {
                self.commit_bytes(ino, &read()?)
            }
            None => {
                if let Some(codec) = self.new_document_codec(name) {
                    return self.create_local_document(parent, name, &codec, &read()?, mtime_ms);
                }
                self.write_snapshot(rel, snapshot, mtime_ms)?;
                // A signed record's first version is its creation; an
                // unsigned one is created as FUSE `create` creates it.
                if !self.signed()
                    && let Some((ino, _)) = self.lookup_kind(parent, name)
                {
                    self.record_version(ino, fs_protocol::FileProvenance::Create, None);
                }
                Ok(())
            }
            Some(_) => self.write_snapshot(rel, snapshot, mtime_ms),
        }
    }

    fn write_snapshot(&self, rel: &Path, snapshot: &Path, mtime_ms: u64) -> Result<(), i32> {
        let attrs = PosixAttrs::default_for(false);
        self.apply_local_snapshot(rel, snapshot, None, mtime_ms, attrs, None)
    }

    fn signed(&self) -> bool {
        self.record_publisher.is_some() && self.history.is_some()
    }

    /// The codec a new file named `name` is kept in as a document. Signed
    /// records carry opaque content only, so there it is always `None`.
    fn new_document_codec(&self, name: &[u8]) -> Option<String> {
        if self.signed() || self.ns().content_authority() != ContentAuthority::LegacyNamespace {
            return None;
        }
        std::str::from_utf8(name).ok().and_then(document_codec_for)
    }

    /// Creates the document `name` in `parent` with `data` as its content,
    /// as a FUSE create followed by a write and close.
    fn create_local_document(
        &self,
        parent: u64,
        name: &[u8],
        codec: &str,
        data: &[u8],
        mtime_ms: u64,
    ) -> Result<(), i32> {
        let name = std::str::from_utf8(name).map_err(|_| libc::EINVAL)?;
        let (ino, delta) = {
            let mut ns = self.ns();
            let ino = ns
                .create_document(parent, name, codec, mtime_ms)
                .ok_or(ENOENT)?;
            (ino, ns.save_incremental())
        };
        self.publish(delta);
        self.record_version(ino, fs_protocol::FileProvenance::Create, None);
        if data.is_empty() {
            return Ok(());
        }
        let committed = self.commit_bytes(ino, data);
        if committed.is_err() {
            // A document its codec refuses must not stay behind empty.
            let _ = self.delete_named(parent, name.as_bytes(), false);
        }
        committed
    }

    pub fn write_lease_at(&self, rel: &Path) -> Option<WriteLease> {
        let components = path_components(rel).ok()?;
        let ino = self.ino_at(&components)?;
        let ns = self.ns();
        ns.write_lease(ns.record_id_for(ino)?)
    }

    /// Who held `record`'s write lease in `epoch`.
    pub fn lease_holder_of(&self, record: [u8; 16], epoch: u64) -> Option<[u8; 32]> {
        self.ns().lease_holder(
            fs_protocol::FileRecordId(uuid::Uuid::from_bytes(record)),
            epoch,
        )
    }

    /// Makes `me` the room's lease authority, when it is not already.
    pub fn claim_lease_authority(&self, me: [u8; 32]) {
        let delta = {
            let mut ns = self.ns();
            if ns.lease_authority() == Some(me) || !ns.put_lease_authority(me) {
                return;
            }
            ns.save_incremental()
        };
        self.publish(delta);
    }

    /// The room's lease authority, if one claimed it.
    pub fn lease_authority(&self) -> Option<[u8; 32]> {
        self.ns().lease_authority()
    }

    /// Acquires the write lease of `rel` for `me`. A live lease `me` holds
    /// is renewed; another device's live lease is `Err(Some(lease))`. A
    /// free lease comes from the room's lease authority when that is
    /// another device: `me` asks and waits up to `wait` for its answer,
    /// and takes the lease optimistically when none comes.
    pub fn acquire_write_lease(
        &self,
        rel: &Path,
        me: [u8; 32],
        ttl: std::time::Duration,
        wait: std::time::Duration,
    ) -> Result<(WriteLease, LeaseSource), Option<WriteLease>> {
        let components = path_components(rel).map_err(|_| None)?;
        let ino = self.ino_at(&components).ok_or(None)?;
        let (record, current, authority) = {
            let ns = self.ns();
            let record = ns.record_id_for(ino).ok_or(None)?;
            (record, ns.write_lease(record), ns.lease_authority())
        };
        let now = now_ms();
        let held = current.filter(|l| l.is_live(now));
        let asks = match (held, authority) {
            (Some(l), _) if l.holder != me => return Err(Some(l)),
            // Renewal, or nobody else arbitrates.
            (Some(_), _) | (None, None) => None,
            (None, Some(a)) if a == me => None,
            (None, Some(a)) => Some(a),
        };
        let Some(_authority) = asks else {
            let granted = authority == Some(me) && held.is_none();
            return self
                .take_lease(record, me, ttl, granted)
                .map(|l| (l, LeaseSource::Local));
        };
        let nonce = tilo_clock::unix_now_ns();
        let delta = {
            let mut ns = self.ns();
            if !ns.put_lease_request(record, me, LeaseRequest { nonce }) {
                return Err(None);
            }
            ns.save_incremental()
        };
        self.publish(delta);
        let before = current.map_or(0, |l| l.epoch);
        let started = std::time::Instant::now();
        let answer = loop {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let lease = self.ns().write_lease(record);
            let now = now_ms();
            match lease {
                Some(l) if l.holder == me && l.granted && l.epoch > before && l.is_live(now) => {
                    break Some(Ok(l));
                }
                Some(l) if l.holder != me && l.is_live(now) => break Some(Err(l)),
                _ => {}
            }
            if started.elapsed() >= wait {
                break None;
            }
        };
        let delta = {
            let mut ns = self.ns();
            ns.remove_lease_request(record, me);
            ns.save_incremental()
        };
        self.publish(delta);
        match answer {
            Some(Ok(lease)) => Ok((lease, LeaseSource::Granted)),
            Some(Err(lease)) => Err(Some(lease)),
            None => self
                .take_lease(record, me, ttl, false)
                .map(|l| (l, LeaseSource::Optimistic)),
        }
    }

    /// Answers pending lease requests as the room's lease authority: each
    /// request for a lease nobody holds is granted with a new epoch, one at
    /// a time, so of two devices asking at once exactly one gets it.
    /// `answered` remembers requests already decided. Returns the grants.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "requests are answered one at a time under one namespace lock, so two grants cannot interleave"
    )]
    pub fn arbitrate_lease_requests(
        &self,
        ttl: std::time::Duration,
        answered: &mut std::collections::HashSet<(fs_protocol::FileRecordId, [u8; 32], u64)>,
    ) -> Vec<(std::path::PathBuf, WriteLease)> {
        let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        let mut granted = Vec::new();
        let delta = {
            let mut ns = self.ns();
            for (record, device, request) in ns.lease_requests() {
                if !answered.insert((record, device, request.nonce)) {
                    continue;
                }
                let current = ns.write_lease(record);
                let Ok(lease) = WriteLease::take(current, device, now_ms(), ttl_ms) else {
                    continue;
                };
                let lease = WriteLease {
                    granted: true,
                    ..lease
                };
                if ns.put_write_lease(record, &lease) {
                    let path = ns
                        .ino_for_record_id(record)
                        .and_then(|ino| ns.path_for(ino))
                        .map(|p| std::path::PathBuf::from(p.trim_start_matches('/')))
                        .unwrap_or_default();
                    granted.push((path, lease));
                }
            }
            (!granted.is_empty()).then(|| ns.save_incremental())
        };
        if let Some(delta) = delta {
            self.publish(delta);
        }
        granted
    }

    fn take_lease(
        &self,
        record: fs_protocol::FileRecordId,
        me: [u8; 32],
        ttl: std::time::Duration,
        granted: bool,
    ) -> Result<WriteLease, Option<WriteLease>> {
        let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        let delta = {
            let mut ns = self.ns();
            let current = ns.write_lease(record);
            let mut lease = WriteLease::take(current, me, now_ms(), ttl_ms).map_err(Some)?;
            if granted && current.is_none_or(|c| c.epoch != lease.epoch) {
                lease.granted = true;
            }
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

    /// Takes or renews the write lease of the file at `rel` for device `me`
    /// and broadcasts the change. `Err(Some(lease))` is another device's
    /// live lease; `Err(None)` means there is no such file.
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

/// Whether a namespace entry's name is exactly one path component.
///
/// The native data plane turns these names into real paths under the live
/// tree, as root. Local creates validate names, but a name merged from a
/// peer's document is whatever bytes the peer wrote, so a separator, a NUL
/// or a dot-link in it must never reach `Path::join`: `../../etc/cron.d/x`
/// would leave the tree, and `/etc/cron.d/x` would replace it.
pub fn projectable_name(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/') && !name.contains(&0)
}

#[cfg(test)]
mod projectable_name_tests {
    use super::projectable_name;

    #[test]
    fn only_single_components_are_projected() {
        for name in [&b"notes.md"[..], b"..hidden", b"a b", "é.txt".as_bytes()] {
            assert!(projectable_name(name), "{name:?} is one component");
        }
        for name in [
            &b""[..],
            b".",
            b"..",
            b"../etc",
            b"../../../../etc/cron.d/x",
            b"/etc/cron.d/x",
            b"a/b",
            b"a\0b",
        ] {
            assert!(!projectable_name(name), "{name:?} must not be projected");
        }
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
