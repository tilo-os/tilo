//! Descriptor-backed Unix provider shared by macOS and Linux.
//!
//! Calls are blocking and belong on bounded workers, never in paint/layout.
//! The authority gate serializes calls with revocation/close. No ambient path is
//! retained or recovered. Single-component no-follow opens pin row identity;
//! symlink resolution walks a bounded stack of already-authorized descriptors.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsString;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::sync::{
    Arc, Mutex, MutexGuard, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use crate::grants::{
    DirectoryGrant, DirectoryPage, EntryName, FileGrant, FsEntry, FsError, FsOwner, FsRight,
    GrantedEntry, PageCursor,
};
use crate::{EntryKind, MAX_READ_CHUNK, Metadata};
use cap_std::fs::{Dir, File, MetadataExt};
use rustix::fs::OFlags;

// Opaque cursors cannot alias a snapshot in a different authority.
static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
struct SnapshotLimits {
    page_rows: usize,
    retained_snapshots: usize,
    snapshot_entries: usize,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            page_rows: 256,
            retained_snapshots: 4,
            snapshot_entries: 1024,
        }
    }
}

struct Authority {
    state: Mutex<Table>,
    limits: SnapshotLimits,
}

#[derive(Default)]
struct Table {
    sequence: u64,
    grants: HashMap<u64, Weak<GrantRecord>>,
    snapshots: BTreeMap<u64, Snapshot>,
    operation_owners: Vec<Arc<crate::recovery::OperationOwner>>,
}

impl Table {
    fn next_id(&mut self) -> Result<u64, FsError> {
        self.sequence = self.sequence.checked_add(1).ok_or(FsError::LimitExceeded)?;
        Ok(self.sequence)
    }
}

struct Snapshot {
    session: u64,
    root: GrantLease,
    entries: Vec<FsEntry>,
}

/// Trusted-host admission boundary. Construction does no filesystem or environment I/O.
pub struct NativeGrantAuthority {
    inner: Arc<Authority>,
    workers: Arc<crate::operations::WorkerQueue>,
}

/// A mount-scoped view of an explicit set of grants. Clones share terminal closure.
#[derive(Clone)]
pub struct FsSession {
    inner: Arc<Session>,
}

type Lifetime = Arc<dyn Fn() -> bool + Send + Sync>;

struct Session {
    authority: Arc<Authority>,
    owner: FsOwner,
    mount: uuid::Uuid,
    id: u64,
    allowed: Vec<GrantLease>,
    closed: AtomicBool,
    lifetimes: Mutex<Vec<Lifetime>>,
    workers: Weak<crate::operations::WorkerQueue>,
    pub(super) mutation_active: AtomicBool,
}

// A terminal lifetime releases snapshots even when an operation exits early.
struct SessionCall<'a> {
    table: MutexGuard<'a, Table>,
    session: &'a Session,
}

impl std::ops::Deref for SessionCall<'_> {
    type Target = Table;
    fn deref(&self) -> &Table {
        &self.table
    }
}

impl std::ops::DerefMut for SessionCall<'_> {
    fn deref_mut(&mut self) -> &mut Table {
        &mut self.table
    }
}

impl Drop for SessionCall<'_> {
    fn drop(&mut self) {
        if self.session.closed.load(Ordering::Acquire) {
            self.table
                .snapshots
                .retain(|_, snapshot| snapshot.session != self.session.id);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(mut table) = self.authority.state.lock() {
            table
                .snapshots
                .retain(|_, snapshot| snapshot.session != self.id);
        }
    }
}

#[derive(Clone)]
pub(super) struct GrantLease {
    record: Arc<GrantRecord>,
    generation: u64,
}

impl fmt::Debug for GrantLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrantLease")
            .field("slot", &self.record.slot)
            .field("generation", &self.generation)
            .field("owner", &self.record.owner)
            .field("rights", &self.record.rights)
            .finish_non_exhaustive()
    }
}

struct GrantRecord {
    authority: Weak<Authority>,
    slot: u64,
    generation: AtomicU64,
    owner: FsOwner,
    rights: Vec<FsRight>,
    parent: Option<GrantLease>,
    name: Option<EntryName>,
    object: Mutex<Option<NativeObject>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct NativeIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
}

impl NativeIdentity {
    pub(super) fn of(metadata: &cap_std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone)]
pub(super) enum NativeObject {
    Directory {
        descriptor: Arc<Dir>,
        identity: NativeIdentity,
    },
    File {
        descriptor: Arc<File>,
        identity: NativeIdentity,
    },
    Link {
        metadata: Metadata,
        _identity: NativeIdentity,
        target: Arc<Result<Arc<File>, FsError>>,
        scope: GrantLease,
    },
    Other {
        metadata: Metadata,
        _identity: NativeIdentity,
    },
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, FsError> {
    mutex.lock().map_err(|_| FsError::StoreUnavailable)
}

pub(super) fn io_error(error: std::io::Error) -> FsError {
    match error.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound,
        std::io::ErrorKind::PermissionDenied => FsError::Denied,
        std::io::ErrorKind::AlreadyExists => FsError::Conflict,
        std::io::ErrorKind::WouldBlock => FsError::Busy,
        _ => FsError::Io(error),
    }
}

fn repeat_error(error: &FsError) -> FsError {
    match error {
        FsError::Denied => FsError::Denied,
        FsError::NotFound => FsError::NotFound,
        FsError::Busy => FsError::Busy,
        FsError::Closed => FsError::Closed,
        FsError::Revoked => FsError::Revoked,
        FsError::Unsupported(reason) => FsError::Unsupported(reason),
        FsError::LimitExceeded => FsError::LimitExceeded,
        FsError::Io(error) => FsError::Io(error.raw_os_error().map_or_else(
            || std::io::Error::new(error.kind(), error.to_string()),
            std::io::Error::from_raw_os_error,
        )),
        _ => FsError::StoreUnavailable,
    }
}

fn describe(metadata: &cap_std::fs::Metadata) -> Metadata {
    let kind = if metadata.is_symlink() {
        EntryKind::Symlink
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    Metadata {
        kind,
        size: metadata.len(),
        modified_unix_seconds: Some(metadata.mtime()),
        needs_hydration: false,
    }
}

impl GrantLease {
    fn object(&self) -> Result<NativeObject, FsError> {
        lock(&self.record.object)?.clone().ok_or(FsError::Revoked)
    }

    fn descends_from(&self, ancestor: &Self) -> bool {
        let mut current = Some(self);
        while let Some(lease) = current {
            if Arc::ptr_eq(&lease.record, &ancestor.record)
                && lease.generation == ancestor.generation
            {
                return true;
            }
            current = lease.record.parent.as_ref();
        }
        false
    }
}

impl Default for NativeGrantAuthority {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeGrantAuthority {
    pub fn new() -> Self {
        Self::with_limits(SnapshotLimits::default())
    }

    fn with_limits(limits: SnapshotLimits) -> Self {
        Self {
            inner: Arc::new(Authority {
                state: Mutex::new(Table::default()),
                limits,
            }),
            workers: crate::operations::WorkerQueue::new(),
        }
    }

    #[cfg(test)]
    fn with_snapshot_limits(limits: SnapshotLimits) -> Self {
        Self::with_limits(limits)
    }

    /// Consume a directory descriptor supplied by the authenticated host.
    /// # Errors
    /// Returns `Unsupported` for a non-directory and typed metadata I/O errors.
    pub fn admit_directory(
        &self,
        opened: std::fs::File,
        owner: FsOwner,
        rights: Vec<FsRight>,
    ) -> Result<DirectoryGrant, FsError> {
        let metadata = cap_std::fs::Metadata::from_file(&opened).map_err(io_error)?;
        if !metadata.is_dir() {
            return Err(FsError::Unsupported("directory descriptor required"));
        }
        let object = NativeObject::Directory {
            descriptor: Arc::new(Dir::from_std_file(opened)),
            identity: NativeIdentity::of(&metadata),
        };
        let mut table = lock(&self.inner.state)?;
        let lease = self
            .inner
            .insert(&mut table, owner, rights, None, None, object)?;
        drop(table);
        Ok(DirectoryGrant { lease })
    }

    /// Consume exactly the supplied file descriptor, without acquiring a parent.
    /// # Errors
    /// Returns `Unsupported` for a directory and typed metadata I/O errors.
    pub fn admit_file(
        &self,
        opened: std::fs::File,
        owner: FsOwner,
        rights: Vec<FsRight>,
    ) -> Result<FileGrant, FsError> {
        let metadata = cap_std::fs::Metadata::from_file(&opened).map_err(io_error)?;
        if metadata.is_dir() {
            return Err(FsError::Unsupported("file descriptor required"));
        }
        let identity = NativeIdentity::of(&metadata);
        let object = if metadata.is_file() {
            NativeObject::File {
                descriptor: Arc::new(File::from_std(opened)),
                identity,
            }
        } else {
            NativeObject::Other {
                metadata: describe(&metadata),
                _identity: identity,
            }
        };
        let mut table = lock(&self.inner.state)?;
        let lease = self
            .inner
            .insert(&mut table, owner, rights, None, None, object)?;
        drop(table);
        Ok(FileGrant { lease })
    }

    /// Bind only the listed live grants to the caller-supplied mount identity.
    /// # Errors
    /// Returns `Denied` for owner/authority mismatch and `Revoked` for expired grants.
    pub fn bind(
        &self,
        owner: FsOwner,
        mount: uuid::Uuid,
        directories: &[DirectoryGrant],
        files: &[FileGrant],
    ) -> Result<FsSession, FsError> {
        let mut table = lock(&self.inner.state)?;
        for lease in directories
            .iter()
            .map(|grant| &grant.lease)
            .chain(files.iter().map(|grant| &grant.lease))
        {
            self.inner.validate(&table, lease, &owner)?;
        }
        let id = table.next_id()?;
        drop(table);
        let allowed = directories
            .iter()
            .map(|grant| grant.lease.clone())
            .chain(files.iter().map(|grant| grant.lease.clone()))
            .collect();
        Ok(FsSession {
            inner: Arc::new(Session {
                authority: Arc::clone(&self.inner),
                owner,
                mount,
                id,
                allowed,
                closed: AtomicBool::new(false),
                lifetimes: Mutex::new(Vec::new()),
                workers: Arc::downgrade(&self.workers),
                mutation_active: AtomicBool::new(false),
            }),
        })
    }

    /// Revoke this directory and every existing descendant without changing disk.
    /// # Errors
    /// Returns `Denied` for a foreign grant and `Revoked` for an expired grant.
    pub fn revoke_directory(&self, grant: &DirectoryGrant) -> Result<(), FsError> {
        self.revoke(&grant.lease)
    }

    /// Revoke this file and all of its clones without changing disk.
    /// # Errors
    /// Returns `Denied` for a foreign grant and `Revoked` for an expired grant.
    pub fn revoke_file(&self, grant: &FileGrant) -> Result<(), FsError> {
        self.revoke(&grant.lease)
    }

    fn revoke(&self, lease: &GrantLease) -> Result<(), FsError> {
        let mut table = lock(&self.inner.state)?;
        self.inner.validate(&table, lease, &lease.record.owner)?;
        for record in table.grants.values().filter_map(Weak::upgrade) {
            let candidate = GrantLease {
                generation: record.generation.load(Ordering::Acquire),
                record,
            };
            if candidate.descends_from(lease) {
                candidate.record.generation.store(1, Ordering::Release);
                *lock(&candidate.record.object)? = None;
            }
        }
        table.snapshots.retain(|_, snapshot| {
            !snapshot.root.descends_from(lease)
                && !snapshot
                    .entries
                    .iter()
                    .any(|entry| entry.grant.lease().descends_from(lease))
        });
        drop(table);
        Ok(())
    }
}

impl Authority {
    fn insert(
        self: &Arc<Self>,
        table: &mut Table,
        owner: FsOwner,
        rights: Vec<FsRight>,
        parent: Option<GrantLease>,
        name: Option<EntryName>,
        object: NativeObject,
    ) -> Result<GrantLease, FsError> {
        table.grants.retain(|_, record| record.strong_count() != 0);
        let slot = table.next_id()?;
        let record = Arc::new(GrantRecord {
            authority: Arc::downgrade(self),
            slot,
            generation: AtomicU64::new(0),
            owner,
            rights,
            parent,
            name,
            object: Mutex::new(Some(object)),
        });
        table.grants.insert(slot, Arc::downgrade(&record));
        Ok(GrantLease {
            record,
            generation: 0,
        })
    }

    fn validate(
        self: &Arc<Self>,
        table: &Table,
        lease: &GrantLease,
        owner: &FsOwner,
    ) -> Result<(), FsError> {
        let mut current = Some(lease);
        while let Some(lease) = current {
            if !Weak::ptr_eq(&lease.record.authority, &Arc::downgrade(self))
                || &lease.record.owner != owner
            {
                return Err(FsError::Denied);
            }
            if lease.record.generation.load(Ordering::Acquire) != lease.generation {
                return Err(FsError::Revoked);
            }
            if !table
                .grants
                .get(&lease.record.slot)
                .is_some_and(|record| Weak::ptr_eq(record, &Arc::downgrade(&lease.record)))
            {
                return Err(FsError::Revoked);
            }
            current = lease.record.parent.as_ref();
        }
        Ok(())
    }
}

impl FsSession {
    fn call(&self) -> Result<SessionCall<'_>, FsError> {
        Ok(SessionCall {
            table: lock(&self.inner.authority.state)?,
            session: &self.inner,
        })
    }

    pub fn owner(&self) -> &FsOwner {
        &self.inner.owner
    }
    pub fn mount_id(&self) -> uuid::Uuid {
        self.inner.mount
    }

    /// Terminal for this session and its clones. A fresh bind is independent.
    pub fn close(&self) {
        // Serialize closure with the next syscall boundary and release snapshots.
        let table = self.inner.authority.state.lock();
        self.inner.closed.store(true, Ordering::Release);
        if let Ok(mut table) = table {
            table
                .snapshots
                .retain(|_, snapshot| snapshot.session != self.inner.id);
        }
    }

    /// Attach a fast, in-memory lifetime predicate. Predicates must not call this provider.
    /// # Errors
    /// Returns `Closed` once any attached lifetime ends; it cannot be reopened.
    pub fn attach_lifetime(&self, is_alive: Lifetime) -> Result<(), FsError> {
        let _call = self.call()?;
        self.check_alive()?;
        lock(&self.inner.lifetimes)?.push(is_alive);
        self.check_alive()
    }

    fn check_alive(&self) -> Result<(), FsError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(FsError::Closed);
        }
        let alive = lock(&self.inner.lifetimes)?
            .iter()
            .all(|predicate| predicate());
        if !alive {
            self.inner.closed.store(true, Ordering::Release);
            return Err(FsError::Closed);
        }
        Ok(())
    }

    fn validate(
        &self,
        table: &Table,
        lease: &GrantLease,
        right: Option<FsRight>,
    ) -> Result<(), FsError> {
        self.check_alive()?;
        self.inner
            .authority
            .validate(table, lease, &self.inner.owner)?;
        if !self
            .inner
            .allowed
            .iter()
            .any(|allowed| lease.descends_from(allowed))
        {
            return Err(FsError::Denied);
        }
        if right.is_some_and(|right| !lease.record.rights.contains(&right)) {
            return Err(FsError::Denied);
        }
        Ok(())
    }

    /// Check live directory scope without accessing the filesystem.
    /// # Errors
    /// Returns `Closed`, `Revoked`, or `Denied` for invalid lifetime, lineage or rights.
    pub fn check_directory(&self, grant: &DirectoryGrant) -> Result<(), FsError> {
        let table = self.call()?;
        self.validate(&table, &grant.lease, Some(FsRight::Enumerate))
    }

    /// Resolve one native child into a retained descriptor or link object.
    /// # Errors
    /// Returns live-scope errors, `NotFound`, or typed filesystem errors.
    pub fn lookup(&self, dir: &DirectoryGrant, name: &EntryName) -> Result<GrantedEntry, FsError> {
        let mut table = self.call()?;
        self.lookup_locked(&mut table, &dir.lease, name)
    }

    fn directory(lease: &GrantLease) -> Result<Arc<Dir>, FsError> {
        match lease.object()? {
            NativeObject::Directory { descriptor, .. } => Ok(descriptor),
            _ => Err(FsError::Unsupported("directory required")),
        }
    }

    fn lookup_locked(
        &self,
        table: &mut Table,
        parent: &GrantLease,
        name: &EntryName,
    ) -> Result<GrantedEntry, FsError> {
        self.lookup_right(table, parent, name, FsRight::Enumerate)
    }

    fn lookup_right(
        &self,
        table: &mut Table,
        parent: &GrantLease,
        name: &EntryName,
        right: FsRight,
    ) -> Result<GrantedEntry, FsError> {
        self.validate(table, parent, Some(right))?;
        let descriptor = Self::directory(parent)?;
        let metadata = descriptor
            .symlink_metadata(name.as_os_str())
            .map_err(io_error)?;
        let identity = NativeIdentity::of(&metadata);
        let object = if metadata.is_symlink() {
            self.validate(table, parent, Some(right))?;
            let target = descriptor
                .read_link_contents(name.as_os_str())
                .map_err(io_error)?;
            self.validate(table, parent, Some(right))?;
            if NativeIdentity::of(
                &descriptor
                    .symlink_metadata(name.as_os_str())
                    .map_err(io_error)?,
            ) != identity
            {
                return Err(FsError::Busy);
            }
            let (stack, scope) = self.directory_stack(parent)?;
            let resolved = if parent.record.rights.contains(&FsRight::Read) {
                self.resolve_link(table, parent, stack, &target)
            } else {
                Err(FsError::Denied)
            };
            if let Err(error @ (FsError::Closed | FsError::Revoked)) = resolved {
                return Err(error);
            }
            NativeObject::Link {
                metadata: describe(&metadata),
                _identity: identity,
                target: Arc::new(resolved),
                scope,
            }
        } else if metadata.is_file() || metadata.is_dir() {
            self.validate(table, parent, Some(right))?;
            let opened = open_pinned(&descriptor, name, &metadata, || {
                self.validate(table, parent, Some(right))
            })?;
            if metadata.is_dir() {
                NativeObject::Directory {
                    descriptor: Arc::new(Dir::from_std_file(opened.into_std())),
                    identity,
                }
            } else {
                NativeObject::File {
                    descriptor: Arc::new(opened),
                    identity,
                }
            }
        } else {
            NativeObject::Other {
                metadata: describe(&metadata),
                _identity: identity,
            }
        };
        self.validate(table, parent, Some(right))?;
        let is_directory = matches!(object, NativeObject::Directory { .. });
        let lease = self.inner.authority.insert(
            table,
            parent.record.owner.clone(),
            parent.record.rights.clone(),
            Some(parent.clone()),
            Some(name.clone()),
            object,
        )?;
        Ok(if is_directory {
            GrantedEntry::Directory(DirectoryGrant { lease })
        } else {
            GrantedEntry::File(FileGrant { lease })
        })
    }

    fn directory_stack(&self, parent: &GrantLease) -> Result<(Vec<Arc<Dir>>, GrantLease), FsError> {
        // Allowed roots authorize their union. A redundant child must not narrow
        // an already-allowed ancestor, while a child-only session stays bounded.
        let mut scope = None;
        let mut ancestor = Some(parent);
        while let Some(lease) = ancestor {
            if self
                .inner
                .allowed
                .iter()
                .any(|allowed| Arc::ptr_eq(&allowed.record, &lease.record))
            {
                scope = Some(lease);
            }
            ancestor = lease.record.parent.as_ref();
        }
        let scope = scope.ok_or(FsError::Denied)?;
        let mut stack = Vec::new();
        let mut current = parent;
        loop {
            stack.push(Self::directory(current)?);
            if Arc::ptr_eq(&current.record, &scope.record) {
                stack.reverse();
                return Ok((stack, scope.clone()));
            }
            current = current.record.parent.as_ref().ok_or(FsError::Denied)?;
        }
    }

    fn resolve_link(
        &self,
        table: &Table,
        parent: &GrantLease,
        mut stack: Vec<Arc<Dir>>,
        target: &Path,
    ) -> Result<Arc<File>, FsError> {
        let mut pending = components(target)?;
        let mut links = 0_u32;
        while let Some(component) = pending.pop_front() {
            self.validate(table, parent, Some(FsRight::Read))?;
            if component == "." {
                // A terminal dot preserves the target's directory requirement.
                continue;
            }
            if component == ".." {
                if stack.len() <= 1 {
                    return Err(FsError::Denied);
                }
                stack.pop();
                continue;
            }
            let dir = stack.last().ok_or(FsError::Denied)?;
            let metadata = dir.symlink_metadata(&component).map_err(io_error)?;
            if metadata.is_symlink() {
                links = links.checked_add(1).ok_or(FsError::LimitExceeded)?;
                if links > 40 {
                    return Err(FsError::LimitExceeded);
                }
                self.validate(table, parent, Some(FsRight::Read))?;
                let target = dir.read_link_contents(&component).map_err(io_error)?;
                self.validate(table, parent, Some(FsRight::Read))?;
                if NativeIdentity::of(&dir.symlink_metadata(&component).map_err(io_error)?)
                    != NativeIdentity::of(&metadata)
                {
                    return Err(FsError::Busy);
                }
                let mut expanded = components(&target)?;
                expanded.append(&mut pending);
                pending = expanded;
                continue;
            }
            if !metadata.is_file() && !metadata.is_dir() {
                return Err(FsError::Unsupported("special file bytes"));
            }
            self.validate(table, parent, Some(FsRight::Read))?;
            let name = EntryName::new(component)?;
            let opened = open_pinned(dir, &name, &metadata, || {
                self.validate(table, parent, Some(FsRight::Read))
            })?;
            if metadata.is_dir() {
                stack.push(Arc::new(Dir::from_std_file(opened.into_std())));
            } else if pending.is_empty() {
                return Ok(Arc::new(opened));
            } else {
                return Err(FsError::Io(rustix::io::Errno::NOTDIR.into()));
            }
        }
        Err(FsError::Unsupported("directory bytes"))
    }

    /// Return metadata for the retained native object, without retargeting names.
    /// # Errors
    /// Returns live-scope, missing-right, or typed metadata I/O errors.
    pub fn metadata(&self, entry: &GrantedEntry) -> Result<Metadata, FsError> {
        let table = self.call()?;
        self.metadata_locked(&table, entry.lease())
    }

    fn metadata_locked(&self, table: &Table, lease: &GrantLease) -> Result<Metadata, FsError> {
        self.validate(table, lease, None)?;
        if !lease
            .record
            .rights
            .iter()
            .any(|right| matches!(right, FsRight::Enumerate | FsRight::Read))
        {
            return Err(FsError::Denied);
        }
        match lease.object()? {
            NativeObject::Directory {
                descriptor,
                identity,
            } => checked_metadata(&descriptor.dir_metadata().map_err(io_error)?, identity),
            NativeObject::File {
                descriptor,
                identity,
            } => checked_metadata(&descriptor.metadata().map_err(io_error)?, identity),
            NativeObject::Link { metadata, .. } | NativeObject::Other { metadata, .. } => {
                Ok(metadata)
            }
        }
    }

    /// Read at most `MAX_READ_CHUNK` bytes, after live-scope and rights validation.
    /// # Errors
    /// Returns live-scope errors, typed link errors, `Unsupported` for special files,
    /// and host I/O errors. EOF is an empty successful chunk.
    pub fn read_at(&self, file: &FileGrant, offset: u64, max_len: u32) -> Result<Vec<u8>, FsError> {
        let table = self.call()?;
        self.validate(&table, &file.lease, Some(FsRight::Read))?;
        let descriptor = match file.lease.object()? {
            NativeObject::File { descriptor, .. } => descriptor,
            NativeObject::Link { target, scope, .. } => {
                if !self
                    .inner
                    .allowed
                    .iter()
                    .any(|allowed| Arc::ptr_eq(&allowed.record, &file.lease.record))
                {
                    self.validate(&table, &scope, Some(FsRight::Read))?;
                }
                match target.as_ref() {
                    Ok(descriptor) => Arc::clone(descriptor),
                    Err(error) => return Err(repeat_error(error)),
                }
            }
            _ => return Err(FsError::Unsupported("special file bytes")),
        };
        let len =
            usize::try_from(max_len.min(MAX_READ_CHUNK)).map_err(|_| FsError::LimitExceeded)?;
        self.validate(&table, &file.lease, Some(FsRight::Read))?;
        if len == 0 {
            return Ok(Vec::new());
        }
        let mut bytes = vec![0; len];
        self.validate(&table, &file.lease, Some(FsRight::Read))?;
        let read = rustix::io::pread(&*descriptor, &mut bytes, offset)
            .map_err(|error| io_error(error.into()))?;
        bytes.truncate(read);
        Ok(bytes)
    }

    /// Retain a bounded snapshot and return at most 256 rows per page.
    /// # Errors
    /// Returns `Busy` when the fixed snapshot entry budget is exceeded, never a
    /// complete-looking truncation. Expired/foreign cursors return `StalePage`.
    pub fn list(
        &self,
        dir: &DirectoryGrant,
        cursor: Option<PageCursor>,
    ) -> Result<DirectoryPage, FsError> {
        let mut table = self.call()?;
        self.validate(&table, &dir.lease, Some(FsRight::Enumerate))?;
        let limits = self.inner.authority.limits;
        let (generation, offset) = if let Some(cursor) = cursor {
            (cursor.generation, cursor.offset)
        } else {
            let descriptor = Self::directory(&dir.lease)?;
            let mut iterator = descriptor.entries().map_err(io_error)?;
            let mut entries = Vec::new();
            loop {
                self.validate(&table, &dir.lease, Some(FsRight::Enumerate))?;
                let Some(row) = iterator.next() else { break };
                let row = row.map_err(io_error)?;
                if entries.len() == limits.snapshot_entries {
                    return Err(FsError::Busy);
                }
                let name = EntryName::new(row.file_name())?;
                let grant = self.lookup_locked(&mut table, &dir.lease, &name)?;
                let metadata = self.metadata_locked(&table, grant.lease())?;
                entries.push(FsEntry {
                    name,
                    grant,
                    metadata,
                });
            }
            entries.sort_by(|left, right| left.name.as_os_str().cmp(right.name.as_os_str()));
            self.validate(&table, &dir.lease, Some(FsRight::Enumerate))?;
            if limits.retained_snapshots == 0 {
                return Err(FsError::Busy);
            }
            while table.snapshots.len() >= limits.retained_snapshots {
                table.snapshots.pop_first();
            }
            let generation = NEXT_SNAPSHOT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| FsError::LimitExceeded)?;
            table.snapshots.insert(
                generation,
                Snapshot {
                    session: self.inner.id,
                    root: dir.lease.clone(),
                    entries,
                },
            );
            (generation, 0)
        };
        self.validate(&table, &dir.lease, Some(FsRight::Enumerate))?;
        let snapshot = table.snapshots.get(&generation).ok_or(FsError::StalePage)?;
        if snapshot.session != self.inner.id
            || !Arc::ptr_eq(&snapshot.root.record, &dir.lease.record)
            || offset > snapshot.entries.len()
        {
            return Err(FsError::StalePage);
        }
        let end = offset
            .saturating_add(limits.page_rows.min(256))
            .min(snapshot.entries.len());
        let entries = snapshot
            .entries
            .get(offset..end)
            .ok_or(FsError::StalePage)?
            .to_vec();
        let next = (end < snapshot.entries.len()).then_some(PageCursor {
            generation,
            offset: end,
        });
        if next.is_none() {
            table.snapshots.remove(&generation);
        }
        drop(table);
        Ok(DirectoryPage {
            entries,
            next,
            generation,
        })
    }
}

fn checked_metadata(
    metadata: &cap_std::fs::Metadata,
    identity: NativeIdentity,
) -> Result<Metadata, FsError> {
    if NativeIdentity::of(metadata) != identity {
        return Err(FsError::Busy);
    }
    Ok(describe(metadata))
}

fn components(path: &Path) -> Result<VecDeque<OsString>, FsError> {
    let mut result = path
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            Component::ParentDir => Ok(OsString::from("..")),
            Component::CurDir => Ok(OsString::from(".")),
            Component::RootDir | Component::Prefix(_) => Err(FsError::Denied),
        })
        .collect::<Result<VecDeque<_>, _>>()?;
    // Path::components normalizes these suffixes away. Keep a directory step
    // in the queue so nested expansion cannot turn `file/` into file bytes.
    let bytes = path.as_os_str().as_bytes();
    if bytes.ends_with(b"/") || bytes.ends_with(b"/.") {
        result.push_back(OsString::from("."));
    }
    Ok(result)
}

fn open_pinned(
    dir: &Dir,
    name: &EntryName,
    metadata: &cap_std::fs::Metadata,
    validate: impl Fn() -> Result<(), FsError>,
) -> Result<File, FsError> {
    let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    if metadata.is_dir() {
        flags |= OFlags::DIRECTORY;
    }
    validate()?;
    // EntryName proves this is one normal component. Use the kernel's actual
    // no-follow operation; cap-std's manual resolver can follow raw O_NOFOLLOW.
    let opened = rustix::fs::openat(dir, name.as_os_str(), flags, rustix::fs::Mode::empty())
        .map_err(|error| io_error(error.into()))?;
    let opened = File::from_std(std::fs::File::from(opened));
    validate()?;
    let observed = opened.metadata().map_err(io_error)?;
    if NativeIdentity::of(&observed) != NativeIdentity::of(metadata)
        || describe(&observed).kind != describe(metadata).kind
    {
        return Err(FsError::Busy);
    }
    Ok(opened)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
        tempfile::TempDir,
        NativeGrantAuthority,
        FsSession,
        DirectoryGrant,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let authority = NativeGrantAuthority::with_snapshot_limits(SnapshotLimits {
            page_rows: 2,
            retained_snapshots: 1,
            snapshot_entries: 3,
        });
        let owner = FsOwner {
            workspace: "test-room".into(),
            app_id: "so.tilo.file-explorer".into(),
        };
        let root = authority
            .admit_directory(
                std::fs::File::open(temp.path()).unwrap(),
                owner.clone(),
                vec![FsRight::Enumerate, FsRight::Read],
            )
            .unwrap();
        let session = authority
            .bind(
                owner,
                uuid::Uuid::from_u128(0xa11),
                std::slice::from_ref(&root),
                &[],
            )
            .unwrap();
        (temp, authority, session, root)
    }

    #[test]
    fn open_pinned_rejects_substituted_symlink_to_original() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("entry"), b"original").unwrap();
        let dir = Dir::from_std_file(std::fs::File::open(temp.path()).unwrap());
        let name = EntryName::new("entry".into()).unwrap();
        let metadata = dir.symlink_metadata(name.as_os_str()).unwrap();
        std::fs::rename(temp.path().join("entry"), temp.path().join("held")).unwrap();
        std::os::unix::fs::symlink("held", temp.path().join("entry")).unwrap();
        assert!(open_pinned(&dir, &name, &metadata, || Ok(())).is_err());
        assert_eq!(
            std::fs::read(temp.path().join("held")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn snapshots_never_truncate_and_die_with_their_lifetime() {
        // phase: snapshot_capacity_never_returns_truncated_success
        {
            let (temp, _authority, session, root) = fixture();
            for name in ["a", "b", "c", "d"] {
                std::fs::write(temp.path().join(name), b"data").unwrap();
            }
            assert!(matches!(session.list(&root, None), Err(FsError::Busy)));
            std::fs::remove_file(temp.path().join("d")).unwrap();
            let page = session.list(&root, None).unwrap();
            assert_eq!(page.entries.len(), 2);
            let final_page = session.list(&root, page.next).unwrap();
            assert_eq!(final_page.entries.len(), 1);
            assert!(final_page.next.is_none());
        }
        // phase: dead_lifetime_releases_retained_snapshot
        {
            let (temp, authority, session, root) = fixture();
            for name in ["a", "b", "c"] {
                std::fs::write(temp.path().join(name), b"").unwrap();
            }
            let alive = Arc::new(AtomicBool::new(true));
            let predicate = Arc::clone(&alive);
            session
                .attach_lifetime(Arc::new(move || predicate.load(Ordering::Acquire)))
                .unwrap();
            let page = session.list(&root, None).unwrap();
            assert!(page.next.is_some());
            assert_eq!(lock(&authority.inner.state).unwrap().snapshots.len(), 1);
            alive.store(false, Ordering::Release);
            assert!(matches!(
                session.check_directory(&root),
                Err(FsError::Closed)
            ));
            assert!(lock(&authority.inner.state).unwrap().snapshots.is_empty());
        }
        // phase: evicted_snapshot_cursor_is_stale
        {
            let (temp, _authority, session, root) = fixture();
            for name in ["a", "b", "c"] {
                std::fs::write(temp.path().join(name), b"").unwrap();
            }
            let first = session.list(&root, None).unwrap();
            let second = session.list(&root, None).unwrap();
            assert_ne!(first.generation, second.generation);
            assert!(matches!(
                session.list(&root, first.next),
                Err(FsError::StalePage)
            ));
            assert_eq!(session.list(&root, second.next).unwrap().entries.len(), 1);
        }
    }
}

/// Private receipt locator. Components are names beneath a freshly allowed root,
/// never paths to reopen from ambient authority.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct NativeLocator {
    pub(super) root: NativeIdentity,
    pub(super) parents: Vec<Vec<u8>>,
    pub(super) name: Vec<u8>,
    pub(super) identity: Option<NativeIdentity>,
}

pub(super) struct MutationSource {
    pub(super) parent: Arc<Dir>,
    pub(super) name: EntryName,
    pub(super) object: NativeObject,
    pub(super) locator: NativeLocator,
}
impl NativeObject {
    pub(super) fn identity(&self) -> NativeIdentity {
        match self {
            Self::Directory { identity, .. }
            | Self::File { identity, .. }
            | Self::Link {
                _identity: identity,
                ..
            }
            | Self::Other {
                _identity: identity,
                ..
            } => *identity,
        }
    }
}
impl NativeGrantAuthority {
    /// Bind the host's durable operation namespace once for its authenticated owner.
    /// # Errors
    /// Returns `Denied` if a different store is already bound for this owner.
    pub fn attach_operations(
        &self,
        owner: Arc<crate::recovery::OperationOwner>,
    ) -> Result<(), FsError> {
        let mut table = lock(&self.inner.state)?;
        if let Some(existing) = table
            .operation_owners
            .iter()
            .find(|existing| existing.owner() == owner.owner())
        {
            if !Arc::ptr_eq(existing, &owner) {
                return Err(FsError::Denied);
            }
        } else {
            table.operation_owners.push(Arc::clone(&owner));
        }
        let roots = table
            .grants
            .values()
            .filter_map(Weak::upgrade)
            .filter_map(|record| {
                if record.parent.is_some()
                    || &record.owner != owner.owner()
                    || !record.rights.contains(&FsRight::Read)
                {
                    return None;
                }
                let lease = GrantLease {
                    generation: record.generation.load(Ordering::Acquire),
                    record,
                };
                if self.inner.validate(&table, &lease, owner.owner()).is_err()
                    || !matches!(lease.object(), Ok(NativeObject::Directory { .. }))
                {
                    return None;
                }
                Some(DirectoryGrant { lease })
            })
            .collect::<Vec<_>>();
        drop(table);
        let session = self.bind(owner.owner().clone(), uuid::Uuid::nil(), &roots, &[])?;
        let result = owner.reconcile(&session);
        drop(owner);
        result
    }
}
impl FsSession {
    pub(super) fn operation_store(&self) -> Result<Arc<crate::recovery::OperationOwner>, FsError> {
        let table = self.call()?;
        self.check_alive()?;
        table
            .operation_owners
            .iter()
            .find(|owner| owner.owner() == self.owner())
            .cloned()
            .ok_or(FsError::StoreUnavailable)
    }
    pub(super) fn operation_queue(&self) -> Result<Arc<crate::operations::WorkerQueue>, FsError> {
        self.inner.workers.upgrade().ok_or(FsError::Closed)
    }
    pub(super) fn claim_mutation(&self) -> Result<(), FsError> {
        self.check_alive()?;
        self.inner
            .mutation_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| FsError::Busy)
    }
    pub(super) fn release_mutation(&self) {
        self.inner.mutation_active.store(false, Ordering::Release);
    }
    pub(super) fn operation_effect<T>(
        &self,
        entries: &[(&GrantedEntry, FsRight)],
        destination: Option<&DirectoryGrant>,
        effect: impl FnOnce() -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        let table = self.call()?;
        self.check_alive()?;
        for (entry, right) in entries {
            let lease = entry.lease();
            self.validate(&table, lease, Some(*right))?;
            if matches!(right, FsRight::Copy | FsRight::Move) {
                self.validate(&table, lease, Some(FsRight::Read))?;
            }
            if let (Some(parent), Some(name)) = (&lease.record.parent, &lease.record.name) {
                self.validate(&table, parent, Some(*right))?;
                let descriptor = Self::directory(parent)?;
                if NativeIdentity::of(
                    &descriptor
                        .symlink_metadata(name.as_os_str())
                        .map_err(io_error)?,
                ) != lease.object()?.identity()
                {
                    return Err(FsError::StaleEntry);
                }
            }
        }
        if let Some(destination) = destination {
            self.validate(&table, &destination.lease, Some(FsRight::Create))?;
        }
        effect()
    }
    pub(super) fn mutation_directory(&self, grant: &DirectoryGrant) -> Result<Arc<Dir>, FsError> {
        let table = self.call()?;
        self.validate(&table, &grant.lease, Some(FsRight::Create))?;
        Self::directory(&grant.lease)
    }
    pub(super) fn mutation_source(
        &self,
        entry: &GrantedEntry,
        right: FsRight,
    ) -> Result<MutationSource, FsError> {
        let table = self.call()?;
        let lease = entry.lease();
        self.validate(&table, lease, Some(right))?;
        if matches!(right, FsRight::Copy | FsRight::Move) {
            self.validate(&table, lease, Some(FsRight::Read))?;
        }
        let parent = lease.record.parent.as_ref().ok_or(FsError::Denied)?;
        self.validate(&table, parent, Some(right))?;
        let name = lease.record.name.clone().ok_or(FsError::Denied)?;
        let descriptor = Self::directory(parent)?;
        let object = lease.object()?;
        let observed = descriptor
            .symlink_metadata(name.as_os_str())
            .map_err(io_error)?;
        if NativeIdentity::of(&observed) != object.identity() {
            return Err(FsError::StaleEntry);
        }
        let locator = self.locator_locked(parent, &name, Some(object.identity()))?;
        Ok(MutationSource {
            parent: descriptor,
            name,
            object,
            locator,
        })
    }
    fn locator_locked(
        &self,
        parent: &GrantLease,
        name: &EntryName,
        identity: Option<NativeIdentity>,
    ) -> Result<NativeLocator, FsError> {
        let mut current = parent;
        let mut components = Vec::new();
        while !self
            .inner
            .allowed
            .iter()
            .any(|allowed| Arc::ptr_eq(&allowed.record, &current.record))
        {
            let name = current.record.name.as_ref().ok_or(FsError::Denied)?;
            components.push(name.as_os_str().as_bytes().to_vec());
            current = current.record.parent.as_ref().ok_or(FsError::Denied)?;
        }
        components.reverse();
        Ok(NativeLocator {
            root: current.object()?.identity(),
            parents: components,
            name: name.as_os_str().as_bytes().to_vec(),
            identity,
        })
    }
    pub(super) fn mutation_locator(
        &self,
        parent: &DirectoryGrant,
        name: &EntryName,
        identity: Option<NativeIdentity>,
    ) -> Result<NativeLocator, FsError> {
        let table = self.call()?;
        self.validate(&table, &parent.lease, Some(FsRight::Create))?;
        self.locator_locked(&parent.lease, name, identity)
    }
    pub(super) fn mutation_result(
        &self,
        parent: &DirectoryGrant,
        name: &EntryName,
    ) -> Result<GrantedEntry, FsError> {
        let mut table = self.call()?;
        self.lookup_right(&mut table, &parent.lease, name, FsRight::Create)
    }
    pub(super) fn rename_result(
        &self,
        parent: &DirectoryGrant,
        name: &EntryName,
    ) -> Result<GrantedEntry, FsError> {
        let mut table = self.call()?;
        self.lookup_right(&mut table, &parent.lease, name, FsRight::Rename)
    }
    pub(super) fn mutation_parent(&self, entry: &GrantedEntry) -> Result<DirectoryGrant, FsError> {
        let table = self.call()?;
        self.validate(&table, entry.lease(), None)?;
        Ok(DirectoryGrant {
            lease: entry.lease().record.parent.clone().ok_or(FsError::Denied)?,
        })
    }
    pub(super) fn recovery_source(
        &self,
        locator: &NativeLocator,
    ) -> Result<(GrantedEntry, Arc<Dir>, EntryName), FsError> {
        use std::os::unix::ffi::OsStringExt;
        let table = self.call()?;
        self.check_alive()?;
        for root in &self.inner.allowed {
            if self.validate(&table, root, None).is_err() {
                continue;
            }
            if root.object()?.identity() != locator.root {
                continue;
            }
            if self.validate(&table, root, Some(FsRight::Read)).is_err() {
                continue;
            }
            let mut dir = Self::directory(root)?;
            for component in &locator.parents {
                let component = EntryName::new(OsString::from_vec(component.clone()))?;
                self.validate(&table, root, Some(FsRight::Read))?;
                let metadata = dir
                    .symlink_metadata(component.as_os_str())
                    .map_err(io_error)?;
                if !metadata.is_dir() {
                    return Err(FsError::StaleEntry);
                }
                let file = open_pinned(&dir, &component, &metadata, || {
                    self.validate(&table, root, Some(FsRight::Read))
                })?;
                dir = Arc::new(Dir::from_std_file(file.into_std()));
            }
            let name = EntryName::new(OsString::from_vec(locator.name.clone()))?;
            self.validate(&table, root, Some(FsRight::Read))?;
            if Some(NativeIdentity::of(
                &dir.symlink_metadata(name.as_os_str()).map_err(io_error)?,
            )) != locator.identity
            {
                return Err(FsError::StaleEntry);
            }
            return Ok((
                GrantedEntry::Directory(DirectoryGrant {
                    lease: root.clone(),
                }),
                dir,
                name,
            ));
        }
        Err(FsError::Denied)
    }
}

impl FsSession {
    pub(super) fn observe_receipt(
        &self,
        locator: &NativeLocator,
    ) -> Result<crate::recovery::Fingerprint, FsError> {
        let (grant, parent, name) = self.recovery_source(locator)?;
        let mut hash = blake3::Hasher::new();
        let mut length = 0;
        let mut count = 0;
        self.hash_receipt_object(
            &grant,
            &parent,
            &name,
            &mut hash,
            &mut length,
            0,
            &mut count,
        )?;
        self.operation_effect(&[(&grant, FsRight::Read)], None, || {
            let identity = NativeIdentity::of(
                &parent
                    .symlink_metadata(name.as_os_str())
                    .map_err(io_error)?,
            );
            if Some(identity) != locator.identity {
                return Err(FsError::StaleEntry);
            }
            Ok(crate::recovery::Fingerprint {
                identity,
                length,
                digest: *hash.finalize().as_bytes(),
            })
        })
    }
    #[allow(
        clippy::too_many_lines,
        reason = "bounded read-only directory, file and link verification share one confinement gate"
    )]
    fn hash_receipt_object(
        &self,
        grant: &GrantedEntry,
        parent: &Dir,
        name: &EntryName,
        hash: &mut blake3::Hasher,
        length: &mut u64,
        depth: usize,
        count: &mut usize,
    ) -> Result<(), FsError> {
        if depth > 128 || *count == 1_000_000 {
            return Err(FsError::LimitExceeded);
        }
        *count = count.checked_add(1).ok_or(FsError::LimitExceeded)?;
        let effect = |action: &mut dyn FnMut() -> Result<(), FsError>| {
            self.operation_effect(&[(grant, FsRight::Read)], None, action)
        };
        let metadata = self.operation_effect(&[(grant, FsRight::Read)], None, || {
            parent.symlink_metadata(name.as_os_str()).map_err(io_error)
        })?;
        if metadata.is_symlink() {
            effect(&mut || {
                let target = parent
                    .read_link_contents(name.as_os_str())
                    .map_err(io_error)?;
                if NativeIdentity::of(
                    &parent
                        .symlink_metadata(name.as_os_str())
                        .map_err(io_error)?,
                ) != NativeIdentity::of(&metadata)
                {
                    return Err(FsError::StaleEntry);
                }
                hash.update(b"link");
                hash.update(&(target.as_os_str().as_bytes().len() as u64).to_le_bytes());
                hash.update(target.as_os_str().as_bytes());
                *length = length
                    .checked_add(target.as_os_str().as_bytes().len() as u64)
                    .ok_or(FsError::LimitExceeded)?;
                Ok(())
            })?;
        } else if metadata.is_file() {
            hash.update(b"file");
            hash.update(&metadata.len().to_le_bytes());
            let file = self.operation_effect(&[(grant, FsRight::Read)], None, || {
                open_pinned(parent, name, &metadata, || Ok(()))
            })?;
            let mut buffer = vec![0; 1024 * 1024];
            let mut offset = 0;
            while offset < metadata.len() {
                let len = usize::try_from(
                    (metadata.len().saturating_sub(offset)).min(buffer.len() as u64),
                )
                .map_err(|_| FsError::LimitExceeded)?;
                let read = self.operation_effect(&[(grant, FsRight::Read)], None, || {
                    rustix::io::pread(
                        &file,
                        buffer.get_mut(..len).ok_or(FsError::LimitExceeded)?,
                        offset,
                    )
                    .map_err(|e| io_error(e.into()))
                })?;
                if read == 0 {
                    return Err(FsError::StaleEntry);
                }
                hash.update(buffer.get(..read).ok_or(FsError::LimitExceeded)?);
                offset = offset
                    .checked_add(read as u64)
                    .ok_or(FsError::LimitExceeded)?;
                *length = length
                    .checked_add(read as u64)
                    .ok_or(FsError::LimitExceeded)?;
            }
            self.operation_effect(&[(grant, FsRight::Read)], None, || {
                let observed = file.metadata().map_err(io_error)?;
                if observed.len() != metadata.len()
                    || observed.mtime() != metadata.mtime()
                    || observed.mtime_nsec() != metadata.mtime_nsec()
                {
                    return Err(FsError::StaleEntry);
                }
                Ok(())
            })?;
        } else if metadata.is_dir() {
            hash.update(b"dir");
            let dir = self.operation_effect(&[(grant, FsRight::Read)], None, || {
                Ok(Dir::from_std_file(
                    open_pinned(parent, name, &metadata, || Ok(()))?.into_std(),
                ))
            })?;
            let mut entries = self.operation_effect(&[(grant, FsRight::Read)], None, || {
                dir.entries().map_err(io_error)
            })?;
            let mut names = Vec::new();
            loop {
                let next = self.operation_effect(&[(grant, FsRight::Read)], None, || {
                    entries.next().transpose().map_err(io_error)
                })?;
                let Some(next) = next else { break };
                if names.len() >= 1_000_000_usize.saturating_sub(*count) {
                    return Err(FsError::LimitExceeded);
                }
                names.push(next.file_name());
            }
            names.sort();
            for native in names {
                hash.update(&(native.as_bytes().len() as u64).to_le_bytes());
                hash.update(native.as_bytes());
                self.hash_receipt_object(
                    grant,
                    &dir,
                    &EntryName::new(native)?,
                    hash,
                    length,
                    depth.saturating_add(1),
                    count,
                )?;
            }
            hash.update(b"end");
        } else {
            return Err(FsError::Unsupported("special staged object"));
        }
        Ok(())
    }
}
