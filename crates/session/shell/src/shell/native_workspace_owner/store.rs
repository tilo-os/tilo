//! The state file's one writer, shared by every snapshot of the tree, and
//! the native owner attached to it.
use super::NativeWorkspaceOwner;
use crate::shell::state::{WeakWorkspaceTreeHandle, WorkspaceTreeHandle};
use parking_lot::Mutex;
use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tilo_files::native::atomic::lease::{FileLease, LeaseMode};
pub(crate) use tilo_peer_state::owner::{LimitedWriter, MAX_STATE_BYTES};

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

/// Clones of the desktop share the writer, so a stale snapshot never
/// overwrites a later one.
#[derive(Clone, Default)]
pub struct NativeStateRuntime(pub(super) Arc<Runtime>);
/// Policy observers must not retain the state writer or its exclusive file lease.
#[derive(Clone)]
pub(crate) struct NativeStateWitness(Weak<Runtime>);
/// Static rejection categories contain no peer, capability, path or process data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthorityRejection {
    WriterRetired,
    OwnerFenced,
    StaleAcceptedSnapshot,
    RoomOwnerRetired,
    RoomOwnerNotCurrent,
}
impl AuthorityRejection {
    /// Whether the authority is definitely gone. A stale snapshot is not:
    /// the owner accepted a record after it, and the export of that record
    /// is what tells whether anything was taken away.
    pub(crate) const fn withdraws(self) -> bool {
        !matches!(self, Self::StaleAcceptedSnapshot)
    }
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::WriterRetired => "writer_retired",
            Self::OwnerFenced => "owner_fenced",
            Self::StaleAcceptedSnapshot => "stale_accepted_snapshot",
            Self::RoomOwnerRetired => "room_owner_retired",
            Self::RoomOwnerNotCurrent => "room_owner_not_current",
        }
    }
}
impl NativeStateWitness {
    #[cfg(test)]
    pub(crate) fn retained_owners(&self) -> usize {
        self.0.strong_count()
    }
    #[cfg(test)]
    pub(crate) fn authority_is_current(
        &self,
        storage: &super::NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
    ) -> bool {
        self.authority_status(storage, room).is_ok()
    }
    #[cfg(test)]
    pub(crate) fn authority_status(
        &self,
        storage: &super::NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
    ) -> Result<(), AuthorityRejection> {
        self.authority_status_awaiting(storage, room, |_| false)
    }
    /// Whether `storage`, the record a policy was exported with, is still
    /// the one the attached owner holds, and `room` current in it. A record
    /// the owner published since is not stale while `exporting` says the
    /// policy its publication exported (its revision) is still on its way
    /// to the observer: that export judges again when it lands, so the gap
    /// between the two withdraws nothing by itself. Every other rejection
    /// is reported ahead of staleness, which withdraws nothing on its own
    /// ([`AuthorityRejection::withdraws`]).
    pub(crate) fn authority_status_awaiting(
        &self,
        storage: &super::NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
        exporting: impl FnOnce(u64) -> bool,
    ) -> Result<(), AuthorityRejection> {
        let runtime = self.0.upgrade().ok_or(AuthorityRejection::WriterRetired)?;
        let Some(owner) = NativeStateRuntime(runtime).owner() else {
            if storage.requires(room) {
                return Err(AuthorityRejection::RoomOwnerRetired);
            }
            return Ok(());
        };
        owner
            .health()
            .map_err(|_| AuthorityRejection::OwnerFenced)?;
        let current = owner.revision();
        let stale = current != storage.revision() && !exporting(current);
        // Judged against the owner as it is now, so a room it no longer
        // holds is a withdrawal whether or not the record is stale.
        if storage.requires(room) {
            owner
                .with_current(room, |_| Ok(()))
                .map_err(|_| AuthorityRejection::RoomOwnerNotCurrent)?;
        }
        if stale {
            return Err(AuthorityRejection::StaleAcceptedSnapshot);
        }
        Ok(())
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
    /// Serializes binding and writing the state file, and is held across
    /// their file I/O. What the frame loop asks of the writer (its owner,
    /// its path) is kept beside it, so the loop never waits for a disk.
    writer: Mutex<Writer>,
    /// Serializes opening this writer's owner ([`super::open`]).
    opening: Mutex<()>,
    /// The state file this writer owns, set once its lease is held. It
    /// never changes afterwards.
    path: std::sync::OnceLock<PathBuf>,
    /// The attached owner and the tree whose state file it belongs to.
    /// Never held across I/O.
    attached: Mutex<Option<Attachment>>,
    pub(super) local_filesystem_consents: Mutex<super::local_filesystem_consents::Consents>,
    /// Set once the writer writes nothing more.
    closed: AtomicBool,
    /// Hands out [`StateSequence`]s; see [`NativeStateRuntime::next_sequence`].
    sequence: AtomicU64,
    /// Per [`Companion`], the newest snapshot it was written from. Held
    /// across the write, so a companion file never goes backwards.
    companions: Mutex<[u64; 1]>,
    /// The tree document beside the state file.
    document: crate::shell::persist::DocLog,
    /// Each room's layout record, beside the state file: every device of
    /// the room writes it, so its history has to outlive a restart, or a
    /// copy rebuilt from the tree would merge with the others' as a
    /// stranger's.
    layout_documents: Mutex<
        std::collections::HashMap<crate::shell::state::RoomId, Arc<crate::shell::persist::DocLog>>,
    >,
    /// The action policy this writer's tree has published.
    action_authority: Arc<crate::shell::action_authority::Authority>,
    /// Runs each state-file write of this writer, on whichever thread makes
    /// it; see [`test_seam::wrap_writes`].
    #[cfg(test)]
    around_write: Mutex<Option<test_seam::AroundWrite>>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            writer: Mutex::new(Writer::default()),
            opening: Mutex::new(()),
            path: std::sync::OnceLock::new(),
            attached: Mutex::new(None),
            local_filesystem_consents: Mutex::default(),
            closed: AtomicBool::new(false),
            sequence: AtomicU64::new(0),
            companions: Mutex::new([0; 1]),
            document: crate::shell::persist::DocLog::default(),
            layout_documents: Mutex::default(),
            action_authority: Arc::default(),
            #[cfg(test)]
            around_write: Mutex::new(None),
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // The policy this writer's tree published is no longer accepted by
        // a writer that holds it: what was judged against it is judged again.
        self.action_authority.judge_again();
    }
}
#[derive(Default)]
struct Writer {
    /// The exclusive lease on the state file's writer sentinel, once bound;
    /// released when the runtime is dropped.
    lease: Option<FileLease>,
    /// The newest snapshot published to the state file.
    published: u64,
}

type Attachment = (
    tilo_peer_state::owner::WeakNativeWorkspaceOwner,
    WeakWorkspaceTreeHandle,
);

impl NativeStateRuntime {
    pub(crate) fn authority_witness(&self) -> NativeStateWitness {
        NativeStateWitness(Arc::downgrade(&self.0))
    }
    /// The action policy this writer's tree has published.
    pub(crate) fn action_authority(&self) -> Arc<crate::shell::action_authority::Authority> {
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
    pub(super) fn attach(&self, owner: &NativeWorkspaceOwner, tree: &WorkspaceTreeHandle) {
        *self.0.attached.lock() = Some((owner.downgrade(), tree.downgrade()));
    }
    /// Write nothing more, once any write in progress has finished, and let
    /// go of the state file's lease: the file stays as this writer last left
    /// it, as at the end of the process that held it. The session that owned
    /// the writer ends with it, so the profile's next session takes the lease
    /// however long work of the old one (a network task, a queued snapshot)
    /// still holds the tree; that work's writes are refused.
    pub(crate) fn close(&self) {
        let mut writer = self.0.writer.lock();
        self.0.closed.store(true, Ordering::Release);
        writer.lease = None;
    }
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

    /// The log of `room`'s layout record beside the state file.
    pub(crate) fn layout_document_log(
        &self,
        room: crate::shell::state::RoomId,
    ) -> Arc<crate::shell::persist::DocLog> {
        self.0
            .layout_documents
            .lock()
            .entry(room)
            .or_default()
            .clone()
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

    /// Write `bytes`, the state file of snapshot `sequence`, to `path`,
    /// unless a later snapshot already reached the file.
    pub(crate) fn publish(
        &self,
        path: &Path,
        bytes: &[u8],
        sequence: StateSequence,
    ) -> io::Result<Publication> {
        let mut writer = self.0.writer.lock();
        if sequence.0 < writer.published {
            return Ok(Publication::Superseded);
        }
        if self.0.closed.load(Ordering::Acquire) {
            return Err(io::Error::other("the state file's writer is closed"));
        }
        self.bind_held(&mut writer, path)?;
        let replace = || {
            tilo_files::native::atomic::replace(
                path,
                bytes,
                tilo_files::native::atomic::Privacy::OwnerOnly,
            )
        };
        #[cfg(test)]
        let replaced = test_seam::run(self, replace);
        #[cfg(not(test))]
        let replaced = replace();
        replaced.map_err(tilo_files::native::atomic::PublicationError::into_io_error)?;
        writer.published = sequence.0;
        drop(writer);
        Ok(Publication::Written)
    }

    /// Own the state file at `path`, unless this writer already does. Once
    /// bound, this is a check of [`Runtime::path`] that waits for nothing,
    /// however long a write holds the writer.
    pub(super) fn bind(&self, path: &Path) -> io::Result<()> {
        if self.is_bound_to(path)? {
            return Ok(());
        }
        let mut writer = self.0.writer.lock();
        self.bind_held(&mut writer, path)
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

    /// Take the exclusive lease on `path`'s writer sentinel, with `writer`
    /// held throughout.
    ///
    /// The lease comes first. Another writer holding it (a second session
    /// on the same profile, a process that has not let go yet) is refused
    /// at the cost of one open and one `flock`. Only a writer holding the
    /// lease makes the directory chain durable, once, before anything is
    /// written into it: that syncs every directory up to `/`, which on a
    /// busy disk takes seconds.
    fn bind_held(&self, writer: &mut Writer, path: &Path) -> io::Result<()> {
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
        writer.lease = Some(lease);
        // Set only here, with the writer held and unbound, so exactly once.
        let _ = self.0.path.set(path.into());
        Ok(())
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

    fn before_write() {
        BEFORE_WRITE.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook();
            }
        });
    }

    /// Run `write`, one write of `runtime`'s state file or of its owner's
    /// record, through the hooks a test installed.
    pub(in crate::shell::native_workspace_owner) fn run<T>(
        runtime: &super::NativeStateRuntime,
        write: impl FnOnce() -> T,
    ) -> T {
        before_write();
        let around = runtime.0.around_write.lock().clone();
        match around {
            Some(around) => {
                let mut write = Some(write);
                let mut written = None;
                around(&mut || written = write.take().map(|write| write()));
                written.expect("a test's write wrapper runs the write")
            }
            None => write(),
        }
    }

    /// Runs one state-file write, given as the closure it calls.
    pub(crate) type AroundWrite = std::sync::Arc<dyn Fn(&mut dyn FnMut()) + Send + Sync>;

    /// Run every state-file write `runtime` makes through `around`, on
    /// whichever thread makes it, until the returned guard is dropped.
    /// `around` must call the write it is given exactly once; a scope it
    /// opens around that call (a storage failure) covers the write.
    pub(crate) fn wrap_writes(
        runtime: &super::NativeStateRuntime,
        around: impl Fn(&mut dyn FnMut()) + Send + Sync + 'static,
    ) -> HeldWrites {
        *runtime.0.around_write.lock() = Some(std::sync::Arc::new(around));
        HeldWrites(runtime.clone())
    }

    /// Call `hook` before every state-file write `runtime` makes, on any
    /// thread, until the returned guard is dropped: a disk whose syncs take
    /// as long as `hook` does.
    pub(crate) fn hold_writes(
        runtime: &super::NativeStateRuntime,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> HeldWrites {
        wrap_writes(runtime, move |write| {
            hook();
            write();
        })
    }

    /// Lets the writer's state-file writes go at full speed again when dropped.
    pub(crate) struct HeldWrites(super::NativeStateRuntime);
    impl Drop for HeldWrites {
        fn drop(&mut self) {
            self.0.0.around_write.lock().take();
        }
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
