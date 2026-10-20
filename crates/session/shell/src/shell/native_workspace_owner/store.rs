//! One state-file writer shared by UI snapshots and committed native owners.
use super::{NativeAuthorityStorage, NativeWorkspaceOwner};
use crate::shell::state::{WeakWorkspaceTreeHandle, WorkspaceTreeHandle};
use parking_lot::{Mutex, RwLock};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tilo_files::native::atomic::lease::{FileLease, LeaseMode};
pub(crate) use tilo_joined_room::{LimitedWriter, MAX_AUTHORITY_BYTES, MAX_STATE_BYTES, preflight};

/// Where a snapshot stands among the writes of one state file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct StateSequence(u64);

/// A file written from a snapshot of the tree, beside the state file. The
/// tree document is not one: it is written as a log of changes, in the
/// order they were made (see [`NativeStateRuntime::document_log`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Companion {
    /// The capability list the filesystem enforces.
    FsCapabilities = 0,
}

/// What [`NativeStateRuntime::publish`] did with a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Publication {
    Written,
    /// A later snapshot is already in the file; this one was dropped.
    Superseded,
}

/// Clones of the desktop share the writer, so stale UI snapshots cannot erase floors.
#[derive(Clone, Default)]
pub struct NativeStateRuntime(pub(super) Arc<Runtime>);
/// Policy observers must not retain the state writer or its exclusive file lease.
#[derive(Clone)]
pub(crate) struct NativeStateWitness(Weak<Runtime>);
/// Static rejection categories contain no peer, capability, path or process data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthorityRejection {
    WriterRetired,
    WriterFenced,
    RoomUncertain,
    InvalidSnapshot,
    StaleAcceptedSnapshot,
    RoomOwnerRetired,
    RoomOwnerNotCurrent,
}
impl AuthorityRejection {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::WriterRetired => "writer_retired",
            Self::WriterFenced => "writer_fenced",
            Self::RoomUncertain => "room_uncertain",
            Self::InvalidSnapshot => "invalid_snapshot",
            Self::StaleAcceptedSnapshot => "stale_accepted_snapshot",
            Self::RoomOwnerRetired => "room_owner_retired",
            Self::RoomOwnerNotCurrent => "room_owner_not_current",
        }
    }
}
impl NativeStateWitness {
    /// Borrow the protected ordinary-room writer for one host operation.
    /// Enrolled rooms must instead borrow their committed native view.
    pub(crate) fn with_current_ordinary<T>(
        &self,
        storage: &NativeAuthorityStorage,
        observed: &NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let runtime = self
            .0
            .upgrade()
            .ok_or("The accepted desktop writer has retired.")?;
        if storage.requires(room)
            || !runtime.healthy.load(Ordering::Acquire)
            || runtime.uncertain_rooms.read().contains(&room)
        {
            return Err("The ordinary desktop policy is not current.".into());
        }
        let expected = digest(storage).map_err(|error| error.to_string())?;
        if digest(observed).map_err(|error| error.to_string())? != expected {
            return Err("The accepted desktop authority has changed.".into());
        }
        // Held across the operation, so no publication completes under it.
        // A publication's file I/O does not hold it, so neither does the
        // operation wait for a disk.
        let accepted = runtime.accepted.lock();
        if *accepted != Some(expected)
            || runtime
                .attached
                .lock()
                .as_ref()
                .is_some_and(|(owner, _)| owner.upgrade().is_none())
        {
            return Err("The accepted desktop writer is unavailable or stale.".into());
        }
        let result = operation();
        drop(accepted);
        drop(runtime);
        result
    }

    #[cfg(test)]
    pub(crate) fn retained_owners(&self) -> usize {
        self.0.strong_count()
    }
    #[cfg(test)]
    pub(crate) fn authority_is_current(
        &self,
        storage: &NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
    ) -> bool {
        self.authority_status(storage, room).is_ok()
    }
    #[cfg(test)]
    pub(crate) fn authority_status(
        &self,
        storage: &NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
    ) -> Result<(), AuthorityRejection> {
        self.authority_status_awaiting(storage, room, |_| false)
    }
    /// [`Self::authority_status`], except that a `storage` this writer has
    /// since moved past is not stale while `exporting` says the record the
    /// writer accepted instead (its digest) is still on its way to the
    /// observer. An owner publication accepts its record before the policy
    /// export carrying it is published; that export judges again when it
    /// lands, so the gap between the two withdraws nothing by itself.
    pub(crate) fn authority_status_awaiting(
        &self,
        storage: &NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
        exporting: impl FnOnce([u8; 32]) -> bool,
    ) -> Result<(), AuthorityRejection> {
        let runtime = self.0.upgrade().ok_or(AuthorityRejection::WriterRetired)?;
        NativeStateRuntime(runtime).authority_status(storage, room, exporting)
    }
    /// Whether both witnesses observe the same writer.
    pub(crate) fn same_writer(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}
impl std::fmt::Debug for NativeStateRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeStateRuntime")
    }
}
pub(super) struct Runtime {
    /// Serializes binding and publication, and is held across their file
    /// I/O (provisioning the directory chain, the state file's data and
    /// directory syncs). Only they take it: what the frame loop asks of the
    /// writer (its owner, its accepted authority, its path) is kept beside
    /// it, so the loop never waits for a disk.
    pub writer: Mutex<Writer>,
    /// Serializes opening this writer's owner ([`super::open`]), which binds
    /// and publishes without holding the tree, so the tree's lock is never
    /// held across a disk sync. Nothing on the frame loop takes it.
    opening: Mutex<()>,
    /// The state file this writer owns, set once its lease is held. It
    /// never changes afterwards.
    path: std::sync::OnceLock<PathBuf>,
    /// Digest of the authority the state file holds, as of the latest
    /// completed bind or publication; `Some` only once the lease is held.
    /// Changed only with `writer` held, and never held across I/O.
    accepted: Mutex<Option<[u8; 32]>>,
    /// The attached owner and the tree whose state file it publishes into.
    /// Never held across I/O.
    attached: Mutex<Option<Attachment>>,
    pub(super) local_filesystem_consents: Mutex<super::local_filesystem_consents::Consents>,
    pub healthy: AtomicBool,
    uncertain_rooms: RwLock<std::collections::BTreeSet<crate::shell::state::RoomId>>,
    /// Hands out [`StateSequence`]s; see [`NativeStateRuntime::next_sequence`].
    sequence: AtomicU64,
    /// Per [`Companion`], the newest snapshot it was written from. Held
    /// across the write, so a companion file never goes backwards.
    companions: Mutex<[u64; 1]>,
    /// The tree document beside the state file.
    document: crate::shell::persist::DocLog,
    /// The action policy this writer's tree has published.
    action_authority: Arc<crate::shell::launcher::action_authority::Authority>,
    /// An owner publication between taking its sequence and installing its
    /// record, and whether a snapshot of the tree was taken meanwhile. Read
    /// and changed only with the tree held, so a snapshot and the install
    /// are ordered by the tree's lock.
    in_flight: Mutex<Option<InFlight>>,
}
/// See [`Runtime::in_flight`].
#[derive(Default)]
struct InFlight {
    snapshotted: bool,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            writer: Mutex::new(Writer::default()),
            opening: Mutex::new(()),
            path: std::sync::OnceLock::new(),
            accepted: Mutex::new(None),
            attached: Mutex::new(None),
            local_filesystem_consents: Mutex::default(),
            healthy: AtomicBool::new(true),
            uncertain_rooms: RwLock::new(std::collections::BTreeSet::default()),
            sequence: AtomicU64::new(0),
            companions: Mutex::new([0; 1]),
            document: crate::shell::persist::DocLog::default(),
            action_authority: Arc::default(),
            in_flight: Mutex::new(None),
        }
    }
}
#[derive(Default)]
pub(super) struct Writer {
    /// The exclusive lease on the state file's writer sentinel, once bound;
    /// released when the runtime is dropped.
    lease: Option<FileLease>,
    /// The newest snapshot published to the state file.
    published: u64,
}

type Attachment = (
    tilo_joined_room::WeakNativeWorkspaceOwner,
    WeakWorkspaceTreeHandle,
);

/// One pretty-printing of an owner record: hashed for its digest and, when
/// the state file is being written, copied out indented one level deeper,
/// as the file embeds it. Wrapped in a [`LimitedWriter`] at
/// [`tilo_joined_room::authority_limit`], the same pass is the record's
/// `preflight_storage`, so a publication encodes its record once rather
/// than once for each check.
struct AuthorityWriter<'a> {
    hash: blake3::Hasher,
    embedded: Option<&'a mut Vec<u8>>,
}
impl Write for AuthorityWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.hash.update(bytes);
        if let Some(embedded) = self.embedded.as_deref_mut() {
            // A pretty-printed newline is always structural: a newline in a
            // string is escaped. So every line after the first is indented.
            let mut lines = bytes.split(|byte| *byte == b'\n');
            if let Some(first) = lines.next() {
                embedded.extend_from_slice(first);
            }
            for line in lines {
                embedded.extend_from_slice(b"\n  ");
                embedded.extend_from_slice(line);
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Pretty-print `storage` once: refuse it as `preflight_storage` would, and
/// return the digest of that JSON; append it to `embedded`, indented to sit
/// one level inside the state file, when given.
pub(crate) fn encode_authority(
    storage: &NativeAuthorityStorage,
    embedded: Option<&mut Vec<u8>>,
) -> io::Result<[u8; 32]> {
    let limit = tilo_joined_room::authority_limit(storage).map_err(io::Error::other)?;
    let mut writer = LimitedWriter::new(
        AuthorityWriter {
            hash: blake3::Hasher::new(),
            embedded,
        },
        limit,
    );
    serde_json::to_writer_pretty(&mut writer, storage).map_err(io::Error::other)?;
    Ok(*writer.into_inner().hash.finalize().as_bytes())
}
/// The digest the writer expects of the record the state file holds.
pub(super) fn digest(storage: &NativeAuthorityStorage) -> io::Result<[u8; 32]> {
    encode_authority(storage, None)
}
impl NativeStateRuntime {
    pub(crate) fn authority_witness(&self) -> NativeStateWitness {
        NativeStateWitness(Arc::downgrade(&self.0))
    }
    /// The action policy this writer's tree has published.
    pub(crate) fn action_authority(
        &self,
    ) -> Arc<crate::shell::launcher::action_authority::Authority> {
        self.0.action_authority.clone()
    }
    pub(crate) fn owner(&self) -> Option<NativeWorkspaceOwner> {
        self.0
            .attached
            .lock()
            .as_ref()
            .and_then(|(owner, _)| owner.upgrade())
    }
    /// The owner already attached to this writer, if it belongs to `tree`.
    pub(super) fn owner_of(
        &self,
        tree: &WorkspaceTreeHandle,
    ) -> Result<Option<NativeWorkspaceOwner>, String> {
        let Some((owner, bound)) = self.0.attached.lock().clone() else {
            return Ok(None);
        };
        let Some(owner) = owner.upgrade() else {
            return Ok(None);
        };
        if !bound.ptr_eq(&tree.downgrade()) {
            return Err("state writer already belongs to a different native tree handle".into());
        }
        Ok(Some(owner))
    }
    /// Held for the whole of opening this writer's owner, so two opens
    /// never both restore one.
    pub(super) fn opening(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.0.opening.lock()
    }
    /// Whether `other` is this same writer, not merely another handle on
    /// the same file.
    pub(super) fn is(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// Whether the record the state file holds, as of the latest completed
    /// bind or publication, is the one whose digest is `authority`.
    pub(super) fn holds(&self, authority: [u8; 32]) -> bool {
        *self.0.accepted.lock() == Some(authority)
    }
    pub(super) fn attach(&self, owner: &NativeWorkspaceOwner, tree: &WorkspaceTreeHandle) {
        *self.0.attached.lock() = Some((owner.downgrade(), tree.downgrade()));
    }
    pub(crate) fn requires_uncertain(&self, room: crate::shell::state::RoomId) -> bool {
        self.0.uncertain_rooms.read().contains(&room)
    }
    /// Read-only witness for host policy exports. A stale/fenced native
    /// authority must not renew an application action lease.
    fn authority_status(
        &self,
        storage: &NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
        exporting: impl FnOnce([u8; 32]) -> bool,
    ) -> Result<(), AuthorityRejection> {
        if !self.healthy() {
            return Err(AuthorityRejection::WriterFenced);
        }
        if self.requires_uncertain(room) {
            return Err(AuthorityRejection::RoomUncertain);
        }
        let expected = digest(storage).map_err(|_| AuthorityRejection::InvalidSnapshot)?;
        let current = *self.0.accepted.lock();
        if let Some(current) = current
            && current != expected
            && !exporting(current)
        {
            return Err(AuthorityRejection::StaleAcceptedSnapshot);
        }
        if storage.requires(room) {
            let owner = self.owner().ok_or(AuthorityRejection::RoomOwnerRetired)?;
            return owner
                .with_current(room, |_| Ok(()))
                .map_err(|_| AuthorityRejection::RoomOwnerNotCurrent);
        }
        Ok(())
    }
    pub(super) fn healthy(&self) -> bool {
        self.0.healthy.load(Ordering::Acquire)
    }
    /// The digest of the record the writer last published or bound.
    #[cfg(test)]
    pub(super) fn expected_digest(&self) -> Option<[u8; 32]> {
        *self.0.accepted.lock()
    }
    /// Called only while the tree snapshot is held. `next` differs only for owner turns.
    /// Order of a snapshot of the tree among every other snapshot of it.
    /// Taken while the caller holds the tree, so a later sequence is never an
    /// older state. A write carrying it is skipped once a later one landed:
    /// background saves and synchronous ones share the files.
    pub(crate) fn next_sequence(&self) -> StateSequence {
        StateSequence(
            self.0
                .sequence
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1),
        )
    }

    /// The tree document beside this state file: every copy of the tree
    /// cuts its changes into it, and every save writes them.
    pub(crate) fn document_log(&self) -> &crate::shell::persist::DocLog {
        &self.0.document
    }

    /// Run `write` for `companion` of snapshot `sequence`, unless that file
    /// was already written from a later snapshot.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock is held across the write so companions are written in order"
    )]
    pub(crate) fn write_companion(
        &self,
        companion: Companion,
        sequence: StateSequence,
        write: impl FnOnce(),
    ) {
        let mut companions = self.0.companions.lock();
        let Some(written) = companions.get_mut(companion as usize) else {
            return;
        };
        if sequence.0 < *written {
            return;
        }
        write();
        *written = sequence.0;
    }

    /// Write `encoded`, the state file holding `next`, in place of the one
    /// holding the record whose digest is `base`, unless a later snapshot
    /// already reached the file. Refused when the file does not hold `base`.
    pub(crate) fn publish(
        &self,
        path: &Path,
        base: [u8; 32],
        next: &NativeAuthorityStorage,
        encoded: &crate::shell::persist::EncodedTree,
        sequence: StateSequence,
    ) -> io::Result<Publication> {
        let mut writer = self.0.writer.lock();
        let written = self.write_held(&mut writer, path, base, next, encoded, sequence);
        match written {
            Ok(Publication::Written) => {
                *self.0.accepted.lock() = Some(encoded.authority);
                writer.published = sequence.0;
                Ok(Publication::Written)
            }
            Ok(Publication::Superseded) => Ok(Publication::Superseded),
            Err(error) => {
                drop(writer);
                Err(self.fence_if_uncertain(error))
            }
        }
    }

    /// The write of [`Self::publish`], with `writer` held, that leaves the
    /// record it wrote for its caller to accept. An error is the atomic
    /// replace's, for [`Self::fence_if_uncertain`] once `writer` is let go.
    fn write_held(
        &self,
        writer: &mut Writer,
        path: &Path,
        base: [u8; 32],
        next: &NativeAuthorityStorage,
        encoded: &crate::shell::persist::EncodedTree,
        sequence: StateSequence,
    ) -> Result<Publication, WriteError> {
        if sequence.0 < writer.published {
            return Ok(Publication::Superseded);
        }
        if !self.healthy() {
            return Err(WriteError::Refused(io::Error::other(
                "native state writer is fenced after uncertain publication",
            )));
        }
        self.bind_held(writer, path, base)
            .map_err(WriteError::Refused)?;
        if *self.0.accepted.lock() != Some(base) {
            return Err(WriteError::Refused(io::Error::other(
                "stale native authority snapshot refused",
            )));
        }
        #[cfg(test)]
        test_seam::before_write();
        match tilo_files::native::atomic::replace(
            path,
            &encoded.bytes,
            tilo_files::native::atomic::Privacy::OwnerOnly,
        ) {
            Ok(()) => Ok(Publication::Written),
            Err(error) => {
                if error.state() != tilo_files::native::atomic::PublicationState::NotPublished {
                    {
                        let mut uncertain = self.0.uncertain_rooms.write();
                        uncertain.extend(next.rooms());
                    }
                    self.0.healthy.store(false, Ordering::Release);
                    return Err(WriteError::Uncertain(error.into_io_error()));
                }
                Err(WriteError::Refused(error.into_io_error()))
            }
        }
    }

    /// The error of a write, after fencing the owner when the write may
    /// have reached the file. Called with the writer let go.
    fn fence_if_uncertain(&self, error: WriteError) -> io::Error {
        match error {
            WriteError::Refused(error) => error,
            WriteError::Uncertain(error) => {
                if let Some(owner) = self.owner() {
                    owner.fence_all();
                }
                error
            }
        }
    }

    /// Start an owner publication: take the writer, which it keeps until
    /// its record is installed. Taken before the tree, so a save holding
    /// the tree never waits on the writer behind it.
    pub(super) fn begin(&self) -> PublicationTurn<'_> {
        PublicationTurn {
            runtime: self,
            writer: Some(self.0.writer.lock()),
            written: None,
            failed: None,
        }
    }

    /// Note a snapshot of the tree being taken, with the tree held: whether
    /// an owner publication is in flight, whose install then saves the tree
    /// again (see [`PublicationTurn::finish`]).
    pub(crate) fn snapshot_during_publication(&self) -> bool {
        let mut in_flight = self.0.in_flight.lock();
        in_flight.as_mut().is_some_and(|in_flight| {
            in_flight.snapshotted = true;
            true
        })
    }

    /// Whether the state file has moved past the record whose digest is
    /// `base`: a snapshot carrying it was overtaken by a publication.
    pub(crate) fn moved_past(&self, base: [u8; 32]) -> bool {
        self.healthy()
            && self
                .0
                .accepted
                .lock()
                .is_some_and(|accepted| accepted != base)
    }

    /// Own the state file at `path`, unless this writer already does. Once
    /// bound, this is a check of [`Runtime::path`] that waits for nothing,
    /// however long a publication holds the writer.
    pub(super) fn bind(&self, path: &Path, storage: &NativeAuthorityStorage) -> io::Result<()> {
        if self.is_bound_to(path)? {
            return Ok(());
        }
        let expected = digest(storage)?;
        let mut writer = self.0.writer.lock();
        self.bind_held(&mut writer, path, expected)
    }

    /// Whether this writer already owns the state file at `path`; an error
    /// once it owns another, since a writer never moves.
    fn is_bound_to(&self, path: &Path) -> io::Result<bool> {
        match self.0.path.get() {
            Some(bound) if bound != path => Err(io::Error::other(
                "native writer cannot change state-file path",
            )),
            bound => Ok(bound.is_some()),
        }
    }

    /// Take the exclusive lease on `path`'s writer sentinel and adopt the
    /// file's authority if it is `expected`, with `writer` held throughout.
    ///
    /// The lease comes first. Another writer holding it (a second session
    /// on the same profile, a process that has not let go yet) is refused
    /// at the cost of one open and one `flock`. Only a writer holding the
    /// lease makes the directory chain durable, once, before anything is
    /// published into it: that syncs every directory up to `/`, which on a
    /// busy disk takes seconds, and a refused writer used to pay it again at
    /// every save and every owner lookup, on whichever thread asked.
    fn bind_held(&self, writer: &mut Writer, path: &Path, expected: [u8; 32]) -> io::Result<()> {
        if self.is_bound_to(path)? {
            debug_assert!(writer.lease.is_some(), "a bound writer holds its lease");
            return Ok(());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let provision = || {
            tilo_files::native::atomic::provision_directory(
                parent,
                tilo_files::native::atomic::Privacy::Inherit,
            )
            .map(drop)
            .map_err(io::Error::other)
        };
        let mut sentinel = path.as_os_str().to_os_string();
        sentinel.push(".writer-lock");
        let sentinel = PathBuf::from(sentinel);
        let open_sentinel = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&sentinel)
        };
        // A missing directory is created and made durable first, only so
        // the sentinel can be created in it.
        let (file, provisioned) = match open_sentinel() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                provision()?;
                (open_sentinel()?, true)
            }
            opened => (opened?, false),
        };
        let lease = FileLease::try_acquire(file, LeaseMode::Exclusive).map_err(io::Error::other)?;
        if !provisioned {
            provision()?;
        }
        if path.exists() {
            #[derive(serde::Deserialize)]
            struct DiskAuthority {
                #[serde(default)]
                schema_version: u32,
                #[serde(default)]
                native_authority: NativeAuthorityStorage,
            }
            use std::io::Read;
            let file = std::fs::File::open(path)?;
            let mut bytes = Vec::new();
            file.take((MAX_STATE_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_STATE_BYTES {
                return Err(io::Error::other(
                    "native encoded state refusal cap exceeded",
                ));
            }
            let disk: DiskAuthority = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if disk.schema_version != crate::shell::persist::NATIVE_STATE_SCHEMA_VERSION {
                return Err(io::Error::other(
                    "unsupported native authority state-file layout",
                ));
            }
            tilo_joined_room::validate_file_layout(disk.schema_version, &disk.native_authority)
                .map_err(io::Error::other)?;
            if digest(&disk.native_authority)? != expected {
                return Err(io::Error::other(
                    "state-file authority changed since this snapshot was loaded",
                ));
            }
        }
        writer.lease = Some(lease);
        *self.0.accepted.lock() = Some(expected);
        // Set only here, with the writer held and unbound, so exactly once.
        let _ = self.0.path.set(path.into());
        Ok(())
    }
}

/// What a write of the state file refused, or failed at after it may have
/// reached the file.
enum WriteError {
    Refused(io::Error),
    Uncertain(io::Error),
}

/// One owner publication's hold of the writer, from before its sequence is
/// taken until its record is installed (see [`NativeStateRuntime::begin`]).
pub(super) struct PublicationTurn<'a> {
    runtime: &'a NativeStateRuntime,
    writer: Option<parking_lot::MutexGuard<'a, Writer>>,
    /// The record written and the sequence it carries, until accepted.
    written: Option<([u8; 32], u64)>,
    /// A failed write, whose fencing waits until the writer is let go.
    failed: Option<WriteError>,
}

impl PublicationTurn<'_> {
    /// Take this publication's sequence and mark it in flight. Called with
    /// the tree held.
    pub(super) fn start(&self) -> StateSequence {
        *self.runtime.0.in_flight.lock() = Some(InFlight::default());
        self.runtime.next_sequence()
    }

    /// Write the state file; the record it holds is accepted by
    /// [`Self::accept`], when it is installed.
    pub(super) fn write(
        &mut self,
        path: &Path,
        base: [u8; 32],
        next: &NativeAuthorityStorage,
        encoded: &crate::shell::persist::EncodedTree,
        sequence: StateSequence,
    ) -> io::Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("native publication already ended"))?;
        match self
            .runtime
            .write_held(writer, path, base, next, encoded, sequence)
        {
            Ok(Publication::Written) => {
                self.written = Some((encoded.authority, sequence.0));
                Ok(())
            }
            // The turn holds the writer from before its sequence was
            // taken, so no later write can have landed first.
            Ok(Publication::Superseded) => {
                Err(io::Error::other("native owner publication was superseded"))
            }
            Err(error) => {
                let shown = match &error {
                    WriteError::Refused(error) | WriteError::Uncertain(error) => {
                        io::Error::new(error.kind(), error.to_string())
                    }
                };
                self.failed = Some(error);
                Err(shown)
            }
        }
    }

    /// Accept the record [`Self::write`] wrote. Called with the tree held,
    /// in the write that installs it.
    pub(super) fn accept(&mut self) {
        if let (Some((authority, sequence)), Some(writer)) = (self.written.take(), &mut self.writer)
        {
            *self.runtime.0.accepted.lock() = Some(authority);
            writer.published = sequence;
        }
    }

    /// End the publication's flight, with the tree held: whether a snapshot
    /// was taken meanwhile, so the caller saves the tree again. That
    /// snapshot still carries the old record; the file refuses it once
    /// this record is in, and its save then stands down for this one.
    pub(super) fn finish(&self) -> bool {
        self.runtime
            .0
            .in_flight
            .lock()
            .take()
            .is_some_and(|in_flight| in_flight.snapshotted)
    }
}

impl Drop for PublicationTurn<'_> {
    fn drop(&mut self) {
        self.runtime.0.in_flight.lock().take();
        drop(self.writer.take());
        if let Some(error) = self.failed.take() {
            drop(self.runtime.fence_if_uncertain(error));
        }
    }
}

#[cfg(test)]
pub(crate) mod test_seam {
    //! A hook run on the publishing thread just before the state file is
    //! replaced, with the writer held, so a test can hold a publication
    //! there as a slow disk would.
    use std::cell::RefCell;

    thread_local! {
        static BEFORE_WRITE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    }

    pub(super) fn before_write() {
        BEFORE_WRITE.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook();
            }
        });
    }

    /// Run `body` with `hook` called before each state-file write this
    /// thread makes.
    pub(crate) fn with_before_write<T>(hook: impl Fn() + 'static, body: impl FnOnce() -> T) -> T {
        let previous = BEFORE_WRITE.with(|slot| slot.borrow_mut().replace(Box::new(hook)));
        let value = body();
        BEFORE_WRITE.with(|slot| *slot.borrow_mut() = previous);
        value
    }
}

#[cfg(test)]
mod action_authority_tests {
    use super::*;
    #[test]
    fn action_authority_witness_rejects_fenced_uncertain_and_stale_native_writer() {
        let runtime = NativeStateRuntime::default();
        let witness = runtime.authority_witness();
        let storage = NativeAuthorityStorage::default();
        let room =
            crate::shell::state::RoomId::parse("00000000-0000-4000-8000-000000000123").unwrap();
        assert!(witness.authority_is_current(&storage, room));
        runtime.0.healthy.store(false, Ordering::Release);
        assert!(!witness.authority_is_current(&storage, room));
        runtime.0.healthy.store(true, Ordering::Release);
        runtime.0.uncertain_rooms.write().insert(room);
        assert!(!witness.authority_is_current(&storage, room));
        runtime.0.uncertain_rooms.write().remove(&room);
        *runtime.0.accepted.lock() = Some([0xff; 32]);
        assert!(!witness.authority_is_current(&storage, room));
        *runtime.0.accepted.lock() = Some(digest(&storage).unwrap());
        assert!(witness.authority_is_current(&storage, room));
        drop(runtime);
        assert!(!witness.authority_is_current(&storage, room));
    }
}
