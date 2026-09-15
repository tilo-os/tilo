//! Descriptor-backed Unix provider shared by macOS and Linux.
//!
//! Calls are blocking and belong on bounded workers, never in paint/layout.
//! The authority gate serializes calls with revocation/close. No ambient path is
//! retained or recovered. Single-component no-follow opens pin row identity;
//! symlink resolution walks a bounded stack of already-authorized descriptors.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsString;
use std::fmt;
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
use cap_std::fs::{Dir, File, MetadataExt, OpenOptions, OpenOptionsExt};
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
    object: Mutex<Option<NativeObject>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeIdentity {
    device: u64,
    inode: u64,
}

impl NativeIdentity {
    fn of(metadata: &cap_std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone)]
enum NativeObject {
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

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, FsError> {
    mutex.lock().map_err(|_| FsError::StoreUnavailable)
}

fn io_error(error: std::io::Error) -> FsError {
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
        let lease = self.inner.insert(&mut table, owner, rights, None, object)?;
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
        let lease = self.inner.insert(&mut table, owner, rights, None, object)?;
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
        if let Ok(mut table) = self.inner.authority.state.lock() {
            self.inner.closed.store(true, Ordering::Release);
            table
                .snapshots
                .retain(|_, snapshot| snapshot.session != self.inner.id);
        } else {
            self.inner.closed.store(true, Ordering::Release);
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
        self.validate(table, parent, Some(FsRight::Enumerate))?;
        let descriptor = Self::directory(parent)?;
        let metadata = descriptor
            .symlink_metadata(name.as_os_str())
            .map_err(io_error)?;
        let identity = NativeIdentity::of(&metadata);
        let object = if metadata.is_symlink() {
            self.validate(table, parent, Some(FsRight::Enumerate))?;
            let target = descriptor
                .read_link_contents(name.as_os_str())
                .map_err(io_error)?;
            self.validate(table, parent, Some(FsRight::Enumerate))?;
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
            self.validate(table, parent, Some(FsRight::Enumerate))?;
            let opened = open_pinned(&descriptor, name.as_os_str(), &metadata, || {
                self.validate(table, parent, Some(FsRight::Enumerate))
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
        self.validate(table, parent, Some(FsRight::Enumerate))?;
        let is_directory = matches!(object, NativeObject::Directory { .. });
        let lease = self.inner.authority.insert(
            table,
            parent.record.owner.clone(),
            parent.record.rights.clone(),
            Some(parent.clone()),
            object,
        )?;
        Ok(if is_directory {
            GrantedEntry::Directory(DirectoryGrant { lease })
        } else {
            GrantedEntry::File(FileGrant { lease })
        })
    }

    fn directory_stack(&self, parent: &GrantLease) -> Result<(Vec<Arc<Dir>>, GrantLease), FsError> {
        let mut stack = Vec::new();
        let mut current = parent;
        loop {
            stack.push(Self::directory(current)?);
            if self
                .inner
                .allowed
                .iter()
                .any(|allowed| Arc::ptr_eq(&allowed.record, &current.record))
            {
                stack.reverse();
                return Ok((stack, current.clone()));
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
            let opened = open_pinned(dir, &component, &metadata, || {
                self.validate(table, parent, Some(FsRight::Read))
            })?;
            if metadata.is_dir() {
                stack.push(Arc::new(Dir::from_std_file(opened.into_std())));
            } else if pending.is_empty() {
                return Ok(Arc::new(opened));
            } else {
                return Err(FsError::NotFound);
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
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(Ok(name.to_os_string())),
            Component::ParentDir => Some(Ok(OsString::from(".."))),
            Component::CurDir => None,
            Component::RootDir | Component::Prefix(_) => Some(Err(FsError::Denied)),
        })
        .collect()
}

fn open_pinned(
    dir: &Dir,
    name: &std::ffi::OsStr,
    metadata: &cap_std::fs::Metadata,
    validate: impl Fn() -> Result<(), FsError>,
) -> Result<File, FsError> {
    let mut flags = OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    if metadata.is_dir() {
        flags |= OFlags::DIRECTORY;
    }
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(
        i32::try_from(flags.bits()).map_err(|_| FsError::Unsupported("native open flags"))?,
    );
    validate()?;
    let opened = dir.open_with(name, &options).map_err(io_error)?;
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
            app_id: "dev.qos.file-explorer".into(),
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
    fn snapshot_capacity_never_returns_truncated_success() {
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

    #[test]
    fn dead_lifetime_releases_retained_snapshot() {
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

    #[test]
    fn evicted_snapshot_cursor_is_stale() {
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
