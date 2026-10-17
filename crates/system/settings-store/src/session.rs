//! The session's settings: the person's record over the device's record.
//!
//! The shell installs a [`SessionSettings`] as its [`SettingsSource`]. Every
//! leaf of the settings document lives in one of two records, chosen by the
//! continuity registry ([`crate::continuity::classify`]):
//!
//! - the **person** record holds the leaves classified
//!   [`SettingScope::Personal`]. The device fabric carries them to the
//!   person's other devices and writes the winners back through
//!   [`crate::update_section`];
//! - the **device** record holds every other leaf and never leaves this
//!   device.
//!
//! [`SettingsSource::load`] answers the device record with the person record
//! laid over it. `settings.json` is no longer the stored truth but a copy of
//! that projection, rewritten after every change, for the processes that
//! read the document without this source: the compositor at boot, apps and
//! anything that runs before the session. Those processes still write
//! sections into the copy with [`crate::update_section_at`]; their writes
//! take the copy's lock, and the session takes in every leaf that differs
//! from the copy it last wrote as an edit, so a write in any process reaches
//! the records, the fabric and every other reader. A copy found when the
//! session opens is taken in the same way, which is how settings written
//! before the first session (first boot, distribution defaults) become the
//! records' first values.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

use crate::continuity::{SettingScope, classify};
use crate::{
    ChangeOrigin, ConditionalUpdate, SectionChange, SettingsSource, load_at, merge_section,
    write_atomic,
};

mod change_log;
mod checked_copy;
mod view;

use change_log::LeafChange;
use tilo_lock::MutexRecover as _;

#[derive(Clone)]
struct Committed {
    document: Map<String, Value>,
    revision: u64,
}

/// Where the session keeps its records and its copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPaths {
    /// The person's record: personal leaves, the ones that roam.
    pub person: PathBuf,
    /// This device's record: every other leaf.
    pub device: PathBuf,
    /// The copy other processes read: `settings.json`.
    pub copy: PathBuf,
}

impl SessionPaths {
    /// Records in a `records` directory beside `copy`.
    #[must_use]
    pub fn beside(copy: PathBuf) -> Self {
        let dir = copy.parent().map(Path::to_path_buf).unwrap_or_default();
        Self::in_root(&dir, copy)
    }

    /// The configured settings file ([`crate::settings_path`]) with the
    /// records in `records` under the config root, where they have always
    /// been: the copy moved into its own directory
    /// ([`crate::SESSION_DIR`]), the records did not, so a process given
    /// that directory cannot rewrite the person's records. A copy named by
    /// [`crate::SESSION_SETTINGS_ENV`] keeps its records beside it.
    #[must_use]
    pub fn configured() -> Option<Self> {
        let copy = crate::settings_path()?;
        let named = std::env::var_os(crate::SESSION_SETTINGS_ENV).is_some_and(|v| !v.is_empty());
        match crate::config_root() {
            Some(root) if !named => Some(Self::in_root(&root, copy)),
            _ => Some(Self::beside(copy)),
        }
    }

    /// Records in `root/records`, with the copy at `copy`.
    #[must_use]
    pub fn in_root(root: &Path, copy: PathBuf) -> Self {
        let records = root.join("records");
        Self {
            person: records.join("person.json"),
            device: records.join("device.json"),
            copy,
        }
    }

    /// Resolve only a trusted root spelling (for example the login home),
    /// leaving all components below it for checked traversal. Never pass an
    /// app-writable copy directory as `root`.
    ///
    /// # Errors
    /// The trusted root cannot be resolved.
    pub fn resolve_trusted_root(mut self, root: &Path) -> std::io::Result<Self> {
        let resolved = root.canonicalize()?;
        for path in [&mut self.person, &mut self.device, &mut self.copy] {
            if let Ok(relative) = path.strip_prefix(root) {
                *path = resolved.join(relative);
            }
        }
        Ok(self)
    }

    /// The bounded log of who changed which leaf, beside the records.
    #[must_use]
    pub fn change_log(&self) -> PathBuf {
        change_log::path_beside(&self.person)
    }
}

/// What tells two versions of the copy apart without reading it: its size
/// and modification time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CopyIdentity {
    len: u64,
    modified: tilo_clock::SystemTime,
    checked: Option<checked_copy::Identity>,
}

/// How long after its modification a file's time may still be shared by a
/// later write of the same size: the file system's clock only ticks so often.
const SAME_TICK: tilo_clock::Duration = tilo_clock::Duration::from_secs(2);

fn copy_identity(path: &Path) -> Option<CopyIdentity> {
    let (dir, name) = tilo_files::file_at(path).ok()?;
    let meta = tilo_files::blocking::metadata(&dir, &name).ok()?;
    Some(CopyIdentity {
        len: meta.len,
        modified: meta.modified,
        checked: None,
    })
}

/// Whether the copy is not what `state` last wrote or read. Size and time
/// answer unless the copy is recent enough that a second write could share
/// both; then its contents do.
fn copy_changed(path: &Path, state: &State) -> bool {
    identity_says_changed(copy_identity(path), state).unwrap_or_else(|| load_at(path) != state.copy)
}

/// What the copy's size and time alone say about whether it is not what
/// `state` last wrote or read: `None` when it is recent enough that only its
/// contents can tell.
fn identity_says_changed(identity: Option<CopyIdentity>, state: &State) -> Option<bool> {
    // A failed copy rewrite forgets its identity. If the obstruction is
    // removed, both identities can be absent; refresh must still repair it.
    if identity != state.copy_identity || identity.is_none() {
        return Some(true);
    }
    let recent = identity.is_some_and(|identity| {
        identity
            .modified
            .duration_since(tilo_clock::UNIX_EPOCH)
            .map_or(true, |modified| {
                u128::from(tilo_clock::unix_now_ms())
                    < modified.as_millis().saturating_add(SAME_TICK.as_millis())
            })
    });
    (!recent).then_some(false)
}

/// Access policy retained for the lifetime of the session. Checked access never
/// resolves the copy's pathname again after opening its parent directory.
enum CopyAccess {
    Legacy(PathBuf),
    Checked(checked_copy::CheckedCopy),
}

enum CopyLock {
    Legacy {
        _guard: tilo_files::Lock,
    },
    Checked {
        _guard: tilo_files::native::lock::FileLock,
    },
}

impl From<checked_copy::Identity> for CopyIdentity {
    fn from(identity: checked_copy::Identity) -> Self {
        Self {
            len: identity.len,
            modified: identity.modified,
            checked: Some(identity),
        }
    }
}

impl CopyAccess {
    fn lock(&self) -> std::io::Result<CopyLock> {
        match self {
            Self::Legacy(path) => Ok(CopyLock::Legacy {
                _guard: lock_copy(path)?,
            }),
            Self::Checked(copy) => Ok(CopyLock::Checked {
                _guard: copy.lock()?,
            }),
        }
    }

    fn identity(&self) -> std::io::Result<Option<CopyIdentity>> {
        match self {
            Self::Legacy(path) => Ok(copy_identity(path)),
            Self::Checked(copy) => Ok(copy.identity()?.map(Into::into)),
        }
    }

    /// Checked reads return the identity of the very descriptor they read.
    /// Callers hold the copy lock before reading its contents.
    fn read(&self) -> std::io::Result<(Map<String, Value>, Option<CopyIdentity>)> {
        match self {
            Self::Legacy(path) => Ok((crate::load_for_update(path)?, copy_identity(path))),
            Self::Checked(copy) => {
                let (document, identity) = copy.read()?;
                Ok((document, identity.map(Into::into)))
            }
        }
    }

    /// An unlocked hint only: recent checked copies are read after locking.
    fn may_have_changed(&self, state: &State) -> bool {
        match self {
            Self::Legacy(path) => copy_changed(path, state),
            Self::Checked(_) => self.identity().map_or(true, |identity| {
                identity_says_changed(identity, state).unwrap_or(true)
            }),
        }
    }

    fn write_copy(&self, paths: &SessionPaths, state: &mut State) -> std::io::Result<()> {
        match self {
            Self::Legacy(_) => write_copy(paths, state),
            Self::Checked(copy) => {
                let projection = state.projection();
                if projection == state.copy && state.copy_identity.is_some() {
                    return Ok(());
                }
                match copy.write(&projection) {
                    Ok(identity) => {
                        state.copy = projection;
                        state.copy_identity = Some(identity.into());
                        Ok(())
                    }
                    Err(error) => {
                        state.copy_identity = None;
                        Err(error)
                    }
                }
            }
        }
    }
}

struct State {
    person: Map<String, Value>,
    device: Map<String, Value>,
    /// The copy as this source last wrote or read it.
    copy: Map<String, Value>,
    copy_identity: Option<CopyIdentity>,
}

impl State {
    fn projection(&self) -> Map<String, Value> {
        let mut doc = self.device.clone();
        for (section, leaves) in &self.person {
            if let Value::Object(leaves) = leaves {
                merge_section(&mut doc, section, leaves.clone());
            }
        }
        doc
    }

    /// Store `value` at `section.leaf` in the record its scope names, and
    /// note the change in `log` when there is one.
    fn put(
        &mut self,
        section: &str,
        leaf: &str,
        value: Value,
        log: &mut Vec<LeafChange>,
    ) -> Touched {
        let personal = classify(&format!("tilo.{section}.{leaf}")) == Some(SettingScope::Personal);
        let record = if personal {
            &mut self.person
        } else {
            &mut self.device
        };
        let entry = record
            .entry(section.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        let Some(leaves) = entry.as_object_mut() else {
            return Touched::default();
        };
        if leaves.get(leaf) == Some(&value) {
            return Touched::default();
        }
        let before = leaves.insert(leaf.to_string(), value.clone());
        log.push(LeafChange {
            section: section.to_string(),
            leaf: Some(leaf.to_string()),
            before,
            after: value,
        });
        Touched {
            person: personal,
            device: !personal,
        }
    }

    /// A top-level value that is not a section (`version`): device-local.
    fn put_top_level(&mut self, name: &str, value: Value, log: &mut Vec<LeafChange>) -> Touched {
        if self.device.get(name) == Some(&value) {
            return Touched::default();
        }
        let before = self.device.insert(name.to_string(), value.clone());
        log.push(LeafChange {
            section: name.to_string(),
            leaf: None,
            before,
            after: value,
        });
        Touched {
            person: false,
            device: true,
        }
    }

    /// Apply every leaf of `doc` that differs from `previous`, the copy it
    /// replaced. Leaves `doc` no longer has are left alone: writers merge
    /// sections, they never remove keys.
    fn take_in(
        &mut self,
        doc: &Map<String, Value>,
        previous: &Map<String, Value>,
        log: &mut Vec<LeafChange>,
    ) -> Touched {
        let mut touched = Touched::default();
        for (name, value) in doc {
            match (value, previous.get(name)) {
                (Value::Object(leaves), before) => {
                    let before = before.and_then(Value::as_object);
                    for (leaf, value) in leaves {
                        if before.and_then(|b| b.get(leaf)) != Some(value) {
                            touched = touched.or(self.put(name, leaf, value.clone(), log));
                        }
                    }
                }
                (value, before) if before != Some(value) => {
                    touched = touched.or(self.put_top_level(name, value.clone(), log));
                }
                _ => {}
            }
        }
        touched
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Touched {
    person: bool,
    device: bool,
}

impl Touched {
    const fn or(self, other: Self) -> Self {
        Self {
            person: self.person || other.person,
            device: self.device || other.device,
        }
    }
}

/// The session's settings document: see the [module docs](self).
pub struct SessionSettings {
    paths: SessionPaths,
    copy_access: CopyAccess,
    state: Mutex<State>,
    revision: AtomicU64,
    changes: crate::ChangeSignal,
    committed: Mutex<std::sync::Arc<Committed>>,
    /// Writes accepted without waiting for the disk, oldest first. Readers
    /// see them already ([`Self::publish_committed`] lays them over the
    /// records); [`Self::write_pending`] makes them durable.
    pending: Mutex<Vec<Edit>>,
    /// The deferred writes that failed, until taken.
    deferred_failures: Mutex<Vec<crate::DeferredFailure>>,
}

/// A write accepted before it was durable.
struct Edit {
    section: String,
    values: Map<String, Value>,
    origin: ChangeOrigin,
}

impl SessionSettings {
    /// Open the records at `paths`, take in whatever the copy holds that they
    /// do not (writes made while no session ran), and bring the copy up to
    /// date.
    ///
    /// # Errors
    ///
    /// The copy cannot be locked, an existing record or copy cannot be read,
    /// or a record or the copy cannot be written.
    pub fn open(paths: SessionPaths) -> std::io::Result<Self> {
        let copy_access = CopyAccess::Legacy(paths.copy.clone());
        Self::open_with(paths, copy_access)
    }

    /// Open with strict, descriptor-relative access to the settings copy.
    /// Missing copies are empty; malformed documents, links and nonregular
    /// files are errors rather than empty settings. The parent directory is
    /// retained so later operations cannot be redirected by renaming it.
    ///
    /// This protects only copy access. Record and history paths remain trusted
    /// caller inputs and are not anchored to the retained directory.
    ///
    /// # Errors
    /// Returns an error for unsafe or unreadable copies, lock or persistence
    /// failures, or unsupported platforms. There is no unchecked fallback.
    pub fn open_checked(paths: SessionPaths) -> std::io::Result<Self> {
        let copy_access = CopyAccess::Checked(checked_copy::CheckedCopy::open(&paths.copy)?);
        Self::open_with(paths, copy_access)
    }

    fn open_with(paths: SessionPaths, copy_access: CopyAccess) -> std::io::Result<Self> {
        let _lock = copy_access.lock()?;
        let mut state = State {
            person: crate::load_for_update(&paths.person)?,
            device: crate::load_for_update(&paths.device)?,
            copy: Map::new(),
            copy_identity: None,
        };
        let (copy, identity) = copy_access.read()?;
        let previous = state.projection();
        let mut log = Vec::new();
        let touched = state.take_in(&copy, &previous, &mut log);
        persist(&paths, &state, touched)?;
        change_log::append(&paths.change_log(), &log, None);
        state.copy = copy;
        state.copy_identity = identity;
        copy_access.write_copy(&paths, &mut state)?;
        let committed = Committed {
            document: state.projection(),
            revision: 1,
        };
        Ok(Self {
            paths,
            copy_access,
            state: Mutex::new(state),
            revision: AtomicU64::new(1),
            changes: crate::ChangeSignal::default(),
            committed: Mutex::new(std::sync::Arc::new(committed)),
            pending: Mutex::new(Vec::new()),
            deferred_failures: Mutex::new(Vec::new()),
        })
    }

    /// Where the records and the copy are.
    #[must_use]
    pub fn paths(&self) -> &SessionPaths {
        &self.paths
    }

    /// The source installed for ordinary host event-loop readers. Its worker
    /// wakes only when read; another process's write to the copy reaches it
    /// through the host's watch of the copy calling [`Self::refresh`].
    ///
    /// # Errors
    /// Returns an error if its refresh worker cannot start.
    pub fn host_view(
        self: &std::sync::Arc<Self>,
    ) -> std::io::Result<std::sync::Arc<dyn SettingsSource>> {
        view::start(self, None)
    }

    /// [`Self::host_view`] for a host that could not watch the copy: the
    /// view takes in another process's write to it every `interval` as well
    /// as on each read, so readers that only wait for
    /// [`SettingsSource::changes`] still hear of it.
    ///
    /// # Errors
    /// Returns an error if its refresh worker cannot start.
    pub fn host_view_polling_copy(
        self: &std::sync::Arc<Self>,
        interval: std::time::Duration,
    ) -> std::io::Result<std::sync::Arc<dyn SettingsSource>> {
        view::start(self, Some(interval))
    }

    /// The document readers see: the records, with the writes not yet made
    /// durable laid over them.
    fn visible_document(state: &State, pending: &[Edit]) -> Map<String, Value> {
        let mut document = state.projection();
        for edit in pending {
            merge_section(&mut document, &edit.section, edit.values.clone());
        }
        document
    }

    fn publish_committed(&self, state: &State) {
        let pending = self.pending.lock_recover();
        self.revision.fetch_add(1, Ordering::AcqRel);
        let committed = std::sync::Arc::new(Committed {
            document: Self::visible_document(state, &pending),
            revision: self.revision.load(Ordering::Acquire),
        });
        *self.committed.lock_recover() = committed;
        drop(pending);
        self.changes.ring();
    }

    /// Accept a write without waiting for the disk: readers see it from the
    /// moment this returns, in order after every earlier write, and
    /// [`Self::write_pending`] makes it durable. `change` sees section
    /// `name` as readers see it now, so two toggles in a row flip twice.
    pub(crate) fn accept_deferred(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        change: &mut SectionChange<'_>,
    ) {
        let mut pending = self.pending.lock_recover();
        let current = std::sync::Arc::clone(&self.committed.lock_recover());
        let empty = Map::new();
        let section = current
            .document
            .get(name)
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let values: Map<String, Value> = change(section)
            .into_iter()
            .filter(|(leaf, value)| section.get(leaf) != Some(value))
            .collect();
        if values.is_empty() {
            return;
        }
        let mut document = current.document.clone();
        merge_section(&mut document, name, values.clone());
        pending.push(Edit {
            section: name.to_string(),
            values,
            origin: origin.clone(),
        });
        let revision = self
            .revision
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        *self.committed.lock_recover() = std::sync::Arc::new(Committed { document, revision });
        drop(pending);
        self.changes.ring();
    }

    /// Whether a write is accepted and not yet durable.
    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.lock_recover().is_empty()
    }

    /// Make every deferred write durable. Waits for the copy's lock and the
    /// disk, so never from a thread that must keep answering input.
    pub fn write_pending(&self) {
        // A worker may already have taken the pending edits and still be
        // persisting them. Join that write before deciding there is no work,
        // so flush_pending returning proves the accepted edits are durable.
        let mut state = self.state.lock_recover();
        if !self.has_pending() {
            return;
        }
        let Ok(_lock) = self.copy_access.lock() else {
            // Still pending: the next call tries again.
            return;
        };
        // Read successfully before removing any accepted edits from pending.
        if self.take_in_copy(&mut state).is_err() {
            return;
        }
        self.apply_pending(&mut state);
    }

    /// With the state and copy locked and the copy already taken in: apply
    /// what the loop accepted and write it. A failure reverts the writes.
    fn apply_pending(&self, state: &mut State) {
        if !self.has_pending() {
            return;
        }
        let edits = std::mem::take(&mut *self.pending.lock_recover());
        if edits.is_empty() {
            return;
        }
        let (person, device) = (state.person.clone(), state.device.clone());
        let mut touched = Touched::default();
        let mut logs = Vec::new();
        for edit in &edits {
            let mut log = Vec::new();
            for (leaf, value) in edit.values.clone() {
                touched = touched.or(state.put(&edit.section, &leaf, value, &mut log));
            }
            logs.push((log, &edit.origin));
        }
        if let Err(error) = persist(&self.paths, state, touched) {
            state.person = person;
            state.device = device;
            let mut sections: Vec<String> = Vec::new();
            for edit in &edits {
                if !sections.contains(&edit.section) {
                    sections.push(edit.section.clone());
                }
            }
            eprintln!(
                "settings-store: writes to {} were not saved and are reverted: {error}",
                sections.join(", ")
            );
            self.deferred_failures
                .lock_recover()
                .push(crate::DeferredFailure {
                    sections,
                    error: error.to_string(),
                });
            crate::source::note_deferred_failure();
            self.publish_committed(state);
            return;
        }
        for (log, origin) in &logs {
            change_log::append(&self.paths.change_log(), log, Some(origin));
        }
        // The records keep the change even when the copy cannot be written;
        // the next change or refresh writes it again.
        let _ = self.copy_access.write_copy(&self.paths, state);
        self.resync_committed(state);
    }

    /// Make the visible document the records plus what is still pending,
    /// telling readers only when that differs from what they were last shown
    /// (another process's write was taken in meanwhile).
    fn resync_committed(&self, state: &State) {
        let pending = self.pending.lock_recover();
        let document = Self::visible_document(state, &pending);
        let mut committed = self.committed.lock_recover();
        if committed.document == document {
            return;
        }
        let revision = self
            .revision
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        *committed = std::sync::Arc::new(Committed { document, revision });
        drop(committed);
        drop(pending);
        self.changes.ring();
    }

    /// The deferred writes that failed, each once, oldest first.
    pub fn take_deferred_failures(&self) -> Vec<crate::DeferredFailure> {
        std::mem::take(&mut *self.deferred_failures.lock_recover())
    }

    /// Take in a write another process made to the copy since this source
    /// last looked. Readers call it through [`SettingsSource::load`] and
    /// [`SettingsSource::revision`], so a reader woken by the copy's file
    /// watch sees the write.
    /// Take in what another process wrote to the copy. Readers do so
    /// before they read; a host that watches the copy calls this when it
    /// changes, so those waiting for a change hear of it at once.
    pub fn refresh(&self) {
        let mut state = self.state.lock_recover();
        if !self.copy_access.may_have_changed(&state) {
            return;
        }
        let Ok(_lock) = self.copy_access.lock() else {
            return;
        };
        if self.take_in_copy(&mut state).is_ok() {
            // A failed write forgets the copy's identity, so the next reader
            // tries again.
            let _ = self.copy_access.write_copy(&self.paths, &mut state);
        }
    }

    /// Whether [`Self::refresh`] may find another process's write: the
    /// copy's size or time is not what this source last wrote or read, or it
    /// is too recent for them to tell. One identity check, no storage lock: a state
    /// held by a writer answers yes and leaves the deciding to `refresh`. A
    /// reader asks this before waking a refresh worker, so reads of an
    /// unchanged copy wake nobody.
    pub(super) fn copy_may_have_changed(&self) -> bool {
        let Ok(identity) = self.copy_access.identity() else {
            return true;
        };
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return true,
        };
        identity_says_changed(identity, &state).unwrap_or(true)
    }

    /// With the copy locked: apply what another process wrote to it.
    fn take_in_copy(&self, state: &mut State) -> std::io::Result<()> {
        // Keep the legacy cache and read-error handling unchanged. Checked
        // reads must succeed before any records, history or revision change,
        // including when a same-size write shares the cached timestamp.
        match &self.copy_access {
            CopyAccess::Legacy(path) => {
                if !copy_changed(path, state) {
                    return Ok(());
                }
            }
            CopyAccess::Checked(_) => {}
        }
        let (copy, identity) = self.copy_access.read()?;
        let (person, device) = (state.person.clone(), state.device.clone());
        let mut log = Vec::new();
        let touched = state.take_in(&copy, &state.copy.clone(), &mut log);
        if let Err(error) = persist(&self.paths, state, touched) {
            state.person = person;
            state.device = device;
            return Err(error);
        }
        change_log::append(&self.paths.change_log(), &log, None);
        state.copy = copy;
        state.copy_identity = identity;
        if touched.person || touched.device {
            self.publish_committed(state);
        }
        Ok(())
    }
}

/// What a session write makes of the section it found (`None`: absent): the
/// leaves to merge, or `None` when the caller's intent was superseded.
type Decide<'a> = dyn FnMut(Option<&Value>) -> Option<Map<String, Value>> + 'a;

impl SessionSettings {
    /// The log of who changed which leaf, oldest first: one JSON object per
    /// change with `section`, `leaf`, `before`, `after`, `action`,
    /// `principal` and `via` (`session` for a write through this source,
    /// `copy` for one another process made to the copy).
    #[must_use]
    pub fn change_log(&self) -> Vec<Value> {
        change_log::read(&self.paths.change_log())
    }

    fn update_section_if(
        &self,
        name: &str,
        expected: Option<&Map<String, Value>>,
        values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate> {
        let mut values = Some(values);
        self.update_section_where(name, &ChangeOrigin::default(), &mut |current| {
            if expected.is_some_and(|expected| current != Some(&Value::Object(expected.clone()))) {
                return None;
            }
            Some(values.take().unwrap_or_default())
        })
    }

    /// Under the state and copy locks: take in the copy, let `decide` look
    /// at section `name` as it now stands, and merge what it returns,
    /// recording `origin` with each changed leaf.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "canonical state and external-copy locks cover comparison, durable merge and publication"
    )]
    fn update_section_where(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        decide: &mut Decide<'_>,
    ) -> std::io::Result<ConditionalUpdate> {
        let mut state = self.state.lock_recover();
        let _lock = self.copy_access.lock()?;
        self.take_in_copy(&mut state)?;
        // Writes the loop accepted come first, so this one is decided and
        // ordered after them.
        self.apply_pending(&mut state);
        let current = state.projection().get(name).cloned();
        let Some(values) = decide(current.as_ref()) else {
            return Ok(ConditionalUpdate::Superseded);
        };
        let (person, device) = (state.person.clone(), state.device.clone());
        let mut touched = Touched::default();
        let mut log = Vec::new();
        for (leaf, value) in values {
            touched = touched.or(state.put(name, &leaf, value, &mut log));
        }
        if let Err(error) = persist(&self.paths, &state, touched) {
            state.person = person;
            state.device = device;
            return Err(error);
        }
        change_log::append(&self.paths.change_log(), &log, Some(origin));
        if touched.person || touched.device {
            self.publish_committed(&state);
        }
        // The records keep the change even when the copy cannot be written;
        // the next change or refresh writes it again.
        Ok(match self.copy_access.write_copy(&self.paths, &mut state) {
            Ok(()) => ConditionalUpdate::Stored,
            Err(error) => ConditionalUpdate::StoredWithCopyRepair(error),
        })
    }
}

impl SettingsSource for SessionSettings {
    fn load(&self) -> Map<String, Value> {
        self.refresh();
        self.state.lock_recover().projection()
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        self.update_section_if(name, None, values)?
            .into_sync_result()
    }

    fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        let mut values = Some(values);
        self.update_section_where(name, origin, &mut |_| values.take())?
            .into_sync_result()
    }

    fn update_section_with(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        change: &mut SectionChange<'_>,
    ) -> std::io::Result<()> {
        self.update_section_where(name, origin, &mut |current| {
            let empty = Map::new();
            Some(change(current.and_then(Value::as_object).unwrap_or(&empty)))
        })?
        .into_sync_result()
    }

    fn update_section_report(
        &self,
        name: &str,
        values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate> {
        self.update_section_if(name, None, values)
    }

    fn compare_update_section(
        &self,
        name: &str,
        expected: &Map<String, Value>,
        values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate> {
        self.update_section_if(name, Some(expected), values)
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "canonical state and copy locks cover atomic display comparison, persistence and publication"
    )]
    fn compare_display_update(
        &self,
        update: crate::DisplayUpdate,
    ) -> std::io::Result<ConditionalUpdate> {
        let mut state = self.state.lock_recover();
        let _lock = self.copy_access.lock()?;
        self.take_in_copy(&mut state)?;
        self.apply_pending(&mut state);
        if !update.matches(&state.projection()) {
            return Ok(ConditionalUpdate::Superseded);
        }
        let sections = update.sections();
        if sections.iter().any(|(section, values)| {
            values.keys().any(|leaf| {
                classify(&format!("tilo.{section}.{leaf}")) == Some(SettingScope::Personal)
            })
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "display intent must stay in one device record",
            ));
        }
        let previous = state.device.clone();
        let mut touched = Touched::default();
        let mut log = Vec::new();
        for (section, values) in sections {
            for (leaf, value) in values {
                touched = touched.or(state.put(section, &leaf, value, &mut log));
            }
        }
        if let Err(error) = persist(&self.paths, &state, touched) {
            state.device = previous;
            return Err(error);
        }
        change_log::append(
            &self.paths.change_log(),
            &log,
            Some(&ChangeOrigin::default()),
        );
        if touched.device {
            self.publish_committed(&state);
        }
        Ok(match self.copy_access.write_copy(&self.paths, &mut state) {
            Ok(()) => ConditionalUpdate::Stored,
            Err(error) => ConditionalUpdate::StoredWithCopyRepair(error),
        })
    }

    fn revision(&self) -> u64 {
        self.refresh();
        self.revision.load(Ordering::Acquire)
    }

    fn flush_pending(&self) {
        self.write_pending();
    }

    fn take_deferred_failures(&self) -> Vec<crate::DeferredFailure> {
        SessionSettings::take_deferred_failures(self)
    }

    fn changes(&self) -> &crate::ChangeSignal {
        &self.changes
    }
}

/// Write the records `touched` names.
fn persist(paths: &SessionPaths, state: &State, touched: Touched) -> std::io::Result<()> {
    if touched.person {
        write_atomic(&paths.person, &Value::Object(state.person.clone()))?;
    }
    if touched.device {
        write_atomic(&paths.device, &Value::Object(state.device.clone()))?;
    }
    Ok(())
}

/// With the copy locked: replace it with the projection when it differs.
fn write_copy(paths: &SessionPaths, state: &mut State) -> std::io::Result<()> {
    let projection = state.projection();
    if projection == state.copy && state.copy_identity.is_some() {
        return Ok(());
    }
    if let Err(error) = write_atomic(&paths.copy, &Value::Object(projection.clone())) {
        state.copy_identity = None;
        return Err(error);
    }
    state.copy = projection;
    state.copy_identity = copy_identity(&paths.copy);
    Ok(())
}

/// The lock every writer of a settings file holds across its read, merge
/// and replace, so no write is built on a copy another writer is replacing.
pub(crate) fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Lock the settings file at `path`, creating its directory if needed.
pub(crate) fn lock_copy(path: &Path) -> std::io::Result<tilo_files::Lock> {
    let (dir, name) = tilo_files::file_at(&lock_path(path)).map_err(std::io::Error::other)?;
    tilo_files::blocking::lock(&dir, &name, tilo_files::LockMode::Exclusive)
        .map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn checked_open_rejects_malformed_copies_and_legacy_preserves_unreadable_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = SessionPaths::beside(root.join("settings.json"));
        for invalid in [b"".as_slice(), b"{", b"[]", b"null", b"\xff"] {
            std::fs::write(&paths.copy, invalid).unwrap();
            assert_eq!(
                SessionSettings::open_checked(paths.clone())
                    .err()
                    .unwrap()
                    .kind(),
                std::io::ErrorKind::InvalidData
            );
            assert!(load_at(&paths.copy).is_empty());
            let legacy = SessionSettings::open(paths.clone());
            if std::str::from_utf8(invalid).is_ok() {
                assert!(legacy.is_ok(), "legacy repairs malformed readable JSON");
            } else {
                assert_eq!(
                    legacy
                        .err()
                        .expect("unreadable bytes must refuse import")
                        .kind(),
                    std::io::ErrorKind::InvalidData
                );
                assert_eq!(std::fs::read(&paths.copy).unwrap(), invalid);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn checked_read_errors_leave_records_history_revision_and_pending_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let session =
            SessionSettings::open_checked(SessionPaths::beside(root.join("settings.json")))
                .unwrap();
        session
            .update_section("power", object(json!({"idle": 5})))
            .unwrap();
        session.accept_deferred("power", &ChangeOrigin::default(), &mut |_| {
            object(json!({"idle": 7}))
        });
        let document = session.state.lock_recover().projection();
        let revision = session.revision.load(Ordering::Acquire);
        let history = session.change_log();
        let person = std::fs::read(&session.paths.person).ok();
        let device = std::fs::read(&session.paths.device).ok();
        let committed = session.committed.lock_recover().document.clone();
        for invalid in [b"{".as_slice(), b"[]", b"null", b"\xff"] {
            std::fs::write(&session.paths.copy, invalid).unwrap();
            assert!(session.copy_may_have_changed());
            session.refresh();
            session.write_pending();
            assert!(
                session
                    .update_section("power", object(json!({"idle": 9})))
                    .is_err()
            );
            assert!(
                session
                    .compare_update_section(
                        "power",
                        &object(json!({"idle": 5})),
                        object(json!({"idle": 9}))
                    )
                    .is_err()
            );
            assert_eq!(session.state.lock_recover().projection(), document);
            assert_eq!(session.revision.load(Ordering::Acquire), revision);
            assert_eq!(session.change_log(), history);
            assert_eq!(std::fs::read(&session.paths.person).ok(), person);
            assert_eq!(std::fs::read(&session.paths.device).ok(), device);
            assert_eq!(session.committed.lock_recover().document, committed);
            assert_eq!(session.pending.lock_recover().len(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn checked_session_retains_copy_directory_after_rename() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let original = root.join("original");
        let paths = SessionPaths {
            person: root.join("person.json"),
            device: root.join("device.json"),
            copy: original.join("settings.json"),
        };
        let session = SessionSettings::open_checked(paths).unwrap();
        let moved = root.join("moved");
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("settings.json"), b"{\"foreign\":true}").unwrap();
        crate::update_section_at(
            &moved.join("settings.json"),
            "power",
            object(json!({"idle": 5})),
        )
        .unwrap();
        assert_eq!(session.load()["power"]["idle"], json!(5));
        session
            .update_section("power", object(json!({"idle": 7})))
            .unwrap();
        assert_eq!(
            load_at(&moved.join("settings.json"))["power"]["idle"],
            json!(7)
        );
        assert_eq!(
            load_at(&original.join("settings.json")),
            object(json!({"foreign": true}))
        );
        assert!(!original.join("settings.json.lock").exists());
    }

    #[test]
    fn opening_a_session_preserves_unreadable_records_and_copy() {
        for target in ["person", "device", "copy"] {
            let dir = tempfile::tempdir().unwrap();
            let paths = SessionPaths::beside(dir.path().join("settings.json"));
            std::fs::create_dir_all(paths.person.parent().unwrap()).unwrap();
            let path = match target {
                "person" => &paths.person,
                "device" => &paths.device,
                _ => &paths.copy,
            };
            let bytes = b"{\"region\":\"\xff\"}";
            std::fs::write(path, bytes).unwrap();
            let result = SessionSettings::open(paths.clone());
            let error = result
                .err()
                .expect("unreadable settings must refuse import");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            for other in [&paths.person, &paths.device, &paths.copy] {
                if other != path {
                    assert!(!other.exists(), "opening must not write partial state");
                }
            }
        }
    }

    #[test]
    fn unreadable_copy_blocks_writes_until_the_external_edit_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        session
            .update_section("appearance", object(json!({"dark_theme": "dark"})))
            .unwrap();
        let record = std::fs::read(&session.paths.person).unwrap();
        let bytes = b"{\"appearance\":\"\xff\"}";
        std::fs::write(&session.paths.copy, bytes).unwrap();

        session.refresh();
        assert_eq!(std::fs::read(&session.paths.copy).unwrap(), bytes);
        let error = session
            .update_section("appearance", object(json!({"dark_theme": "light"})))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&session.paths.copy).unwrap(), bytes);
        assert_eq!(std::fs::read(&session.paths.person).unwrap(), record);
        assert_eq!(session.load()["appearance"]["dark_theme"], "dark");

        std::fs::write(
            &session.paths.copy,
            br#"{"appearance":{"dark_theme":"light"}}"#,
        )
        .unwrap();
        session.refresh();
        assert_eq!(session.load()["appearance"]["dark_theme"], "light");
        assert_eq!(
            load_at(&session.paths.person)["appearance"]["dark_theme"],
            "light"
        );
    }

    #[test]
    fn missing_and_malformed_copies_are_repaired_from_durable_records() {
        for missing in [true, false] {
            for reopen in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let session = open(dir.path());
                session
                    .update_section("appearance", object(json!({"dark_theme": "dark"})))
                    .unwrap();
                session
                    .update_section("power", object(json!({"idle": 5})))
                    .unwrap();
                let expected = session.load();
                let person = std::fs::read(&session.paths.person).unwrap();
                let device = std::fs::read(&session.paths.device).unwrap();
                if missing {
                    std::fs::remove_file(&session.paths.copy).unwrap();
                } else {
                    std::fs::write(&session.paths.copy, b"{broken json").unwrap();
                }
                let session = if reopen {
                    SessionSettings::open(session.paths.clone()).unwrap()
                } else {
                    session.refresh();
                    session
                };
                assert_eq!(session.load(), expected);
                assert_eq!(load_at(&session.paths.copy), expected);
                assert_eq!(std::fs::read(&session.paths.person).unwrap(), person);
                assert_eq!(std::fs::read(&session.paths.device).unwrap(), device);
            }
        }
    }

    #[test]
    fn display_update_commits_transaction_and_profiles_in_one_device_record() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let update = crate::DisplayUpdate::new(
            &session.load(),
            object(json!({"id": 7, "state": "confirmed"})),
            Some(object(json!({"profiles": {"panel": {"outputs": {}}}}))),
        );
        assert!(matches!(
            session.compare_display_update(update).unwrap(),
            ConditionalUpdate::Stored
        ));
        let document = session.load();
        assert_eq!(document["display_transaction"]["id"], 7);
        assert_eq!(
            document["display_profiles"]["profiles"]["panel"]["outputs"],
            json!({})
        );
        assert_eq!(
            Value::Object(load_at(&session.paths.device)),
            Value::Object(document.clone())
        );
        let reopened = SessionSettings::open(session.paths).unwrap();
        assert_eq!(reopened.load(), document);
    }

    #[test]
    fn display_update_refuses_a_new_external_profile_without_overwriting_either_section() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let update = crate::DisplayUpdate::new(
            &session.load(),
            object(json!({"id": 7})),
            Some(object(json!({"profiles": {}}))),
        );
        crate::update_section_at(
            &session.paths.copy,
            "display_profiles",
            object(json!({"external_revision": 8})),
        )
        .unwrap();
        assert!(matches!(
            session.compare_display_update(update).unwrap(),
            ConditionalUpdate::Superseded
        ));
        let document = session.load();
        assert!(!document.contains_key("display_transaction"));
        assert_eq!(document["display_profiles"]["external_revision"], 8);
    }

    #[test]
    fn display_update_record_failure_rolls_back_both_sections() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let before = session.load();
        let update = crate::DisplayUpdate::new(
            &before,
            object(json!({"id": 7})),
            Some(object(json!({"profiles": {}}))),
        );
        std::fs::create_dir_all(session.paths.device.parent().unwrap()).unwrap();
        std::fs::create_dir(&session.paths.device).unwrap();
        assert!(session.compare_display_update(update).is_err());
        assert_eq!(session.load(), before);
        assert_eq!(load_at(&session.paths.copy), before);
    }

    #[test]
    fn display_update_copy_failure_reports_durable_both_sections_and_repairs_later() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let update = crate::DisplayUpdate::new(
            &session.load(),
            object(json!({"id": 7})),
            Some(object(json!({"profiles": {}}))),
        );
        let copy = std::fs::read(&session.paths.copy).unwrap();
        // The first publication saves the device record; only the second,
        // replacing the still-readable copy, fails.
        let result = tilo_files::native::atomic::test_support::with_failure_on_nth(
            tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
            2,
            || session.compare_display_update(update),
        );
        assert!(matches!(
            result.unwrap(),
            ConditionalUpdate::StoredWithCopyRepair(_)
        ));
        assert_eq!(std::fs::read(&session.paths.copy).unwrap(), copy);
        let durable = load_at(&session.paths.device);
        assert_eq!(durable["display_transaction"]["id"], 7);
        assert_eq!(durable["display_profiles"]["profiles"], json!({}));
        assert_eq!(session.load(), durable);
        assert_eq!(load_at(&session.paths.copy), durable);
    }

    #[test]
    fn every_change_rings_its_readers_and_nothing_else_does() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let rung = std::sync::Arc::new(AtomicU64::new(0));
        let count = std::sync::Arc::clone(&rung);
        let listener: std::sync::Arc<crate::Listener> = std::sync::Arc::new(move || {
            count.fetch_add(1, Ordering::AcqRel);
        });
        session.changes().listen(&listener);

        // A write in this process rings at once.
        session
            .update_section("power", object(json!({"idle": 5})))
            .unwrap();
        assert_eq!(rung.load(Ordering::Acquire), 1);

        // Another process's write rings when the session takes it in, as a
        // watch on the copy has it do.
        crate::update_section_at(
            &session.paths().copy,
            "appearance",
            object(json!({"theme": "Light"})),
        )
        .unwrap();
        session.refresh();
        assert_eq!(rung.load(Ordering::Acquire), 2);

        // Looking again finds nothing new and rings nothing.
        session.refresh();
        let _ = session.load();
        assert_eq!(rung.load(Ordering::Acquire), 2);

        // A reader that stopped listening is not called.
        drop(listener);
        session
            .update_section("power", object(json!({"idle": 6})))
            .unwrap();
        assert_eq!(rung.load(Ordering::Acquire), 2);
    }

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    fn open(dir: &Path) -> SessionSettings {
        SessionSettings::open(SessionPaths::beside(dir.join("settings.json"))).unwrap()
    }

    /// A host view over a session in `dir`, its calling thread deferring writes.
    fn deferring_view(
        dir: &Path,
    ) -> (
        std::sync::Arc<SessionSettings>,
        std::sync::Arc<dyn SettingsSource>,
    ) {
        let session = std::sync::Arc::new(open(dir));
        let view = session.host_view().unwrap();
        crate::defer_writes_on_this_thread();
        (session, view)
    }

    #[test]
    fn a_deferred_write_returns_while_the_disk_is_parked_and_is_read_back_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let (session, view) = deferring_view(dir.path());
        let before = view.revision();
        // The copy's lock stands for a disk that does not answer: every
        // durable write waits for it.
        let parked = lock_copy(&session.paths.copy).unwrap();
        let (done, returned) = std::sync::mpsc::channel();
        let writer = {
            let view = std::sync::Arc::clone(&view);
            std::thread::spawn(move || {
                crate::defer_writes_on_this_thread();
                view.update_section("appearance", object(json!({"mode": "Dark"})))
                    .unwrap();
                let _ = done.send(view.load());
            })
        };
        let seen = returned
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the write waited for the disk");
        writer.join().unwrap();
        assert_eq!(seen["appearance"]["mode"], "Dark", "read back at once");
        assert!(view.revision() > before, "readers are told");
        assert!(
            !load_at(&session.paths.person).contains_key("appearance"),
            "nothing reached the record while the disk was parked"
        );
        drop(parked);
        view.flush_pending();
        assert_eq!(load_at(&session.paths.person)["appearance"]["mode"], "Dark");
        assert_eq!(load_at(&session.paths.copy)["appearance"]["mode"], "Dark");
    }

    /// A flush joins a worker that took the edits before it finished writing.
    #[test]
    fn a_flush_waits_for_an_in_flight_deferred_write() {
        let dir = tempfile::tempdir().unwrap();
        let (session, view) = deferring_view(dir.path());
        let parked = lock_copy(&session.paths.copy).unwrap();
        view.update_section("appearance", object(json!({"mode": "Dark"})))
            .unwrap();
        tilo_test_env::wait_until("the writer holds the state lock", || {
            session.state.try_lock().is_err()
        });
        let (started, entered) = std::sync::mpsc::channel();
        let (done, returned) = std::sync::mpsc::channel();
        let flush = {
            let view = std::sync::Arc::clone(&view);
            std::thread::spawn(move || {
                started.send(()).unwrap();
                view.flush_pending();
                done.send(()).unwrap();
            })
        };
        tilo_test_env::recv("the flush starts", &entered);
        assert!(
            returned.try_recv().is_err(),
            "the flush cannot finish while storage is parked"
        );
        drop(parked);
        tilo_test_env::recv("the flush joins the writer", &returned);
        flush.join().unwrap();
        assert_eq!(load_at(&session.paths.person)["appearance"]["mode"], "Dark");
        assert_eq!(load_at(&session.paths.copy)["appearance"]["mode"], "Dark");
    }

    #[test]
    fn deferred_writes_keep_their_order_and_their_read_modify_write_steps() {
        let dir = tempfile::tempdir().unwrap();
        let (session, view) = deferring_view(dir.path());
        let parked = lock_copy(&session.paths.copy).unwrap();
        let origin = ChangeOrigin::default();
        let flip = |section: &Map<String, Value>| {
            let on = section.get("on").and_then(Value::as_bool).unwrap_or(false);
            object(json!({ "on": !on }))
        };
        view.update_section_with("sounds", &origin, &mut |s| flip(s))
            .unwrap();
        view.update_section_with("sounds", &origin, &mut |s| flip(s))
            .unwrap();
        view.update_section_with("sounds", &origin, &mut |s| flip(s))
            .unwrap();
        view.update_section("sounds", object(json!({"volume": 1})))
            .unwrap();
        view.update_section("sounds", object(json!({"volume": 2})))
            .unwrap();
        let seen = view.load();
        assert_eq!(
            seen["sounds"],
            json!({"on": true, "volume": 2}),
            "three flips, newest wins"
        );
        drop(parked);
        // A writer that waits for the disk is decided and ordered after them.
        let sync = {
            let view = std::sync::Arc::clone(&view);
            std::thread::spawn(move || {
                view.update_section_with("sounds", &ChangeOrigin::default(), &mut |s| {
                    let volume = s.get("volume").and_then(Value::as_i64).unwrap_or(0);
                    object(json!({ "volume": volume + 10 }))
                })
                .unwrap();
            })
        };
        sync.join().unwrap();
        assert_eq!(view.load()["sounds"], json!({"on": true, "volume": 12}));
        view.flush_pending();
        assert_eq!(
            load_at(&session.paths.copy)["sounds"],
            json!({"on": true, "volume": 12})
        );
    }

    #[test]
    fn dropping_the_view_makes_accepted_writes_durable() {
        let dir = tempfile::tempdir().unwrap();
        let paths = SessionPaths::beside(dir.path().join("settings.json"));
        {
            let (session, view) = deferring_view(dir.path());
            let parked = lock_copy(&session.paths.copy).unwrap();
            view.update_section("appearance", object(json!({"theme": "light"})))
                .unwrap();
            drop(parked);
            drop(view);
        }
        let reopened = SessionSettings::open(paths).unwrap();
        assert_eq!(reopened.load()["appearance"]["theme"], "light");
    }

    #[test]
    fn a_deferred_write_that_cannot_be_stored_is_reverted_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (session, view) = deferring_view(dir.path());
        let before = view.load();
        std::fs::create_dir_all(session.paths.device.parent().unwrap()).unwrap();
        std::fs::create_dir(&session.paths.device).unwrap();
        std::fs::create_dir(&session.paths.person).unwrap();
        view.update_section("appearance", object(json!({"theme": "dark"})))
            .unwrap();
        assert_eq!(
            view.load()["appearance"]["theme"],
            "dark",
            "accepted for now"
        );
        let revision = view.revision();
        view.flush_pending();
        assert_eq!(view.load(), before, "the document went back");
        assert!(view.revision() > revision, "readers are told it went back");
        assert!(
            crate::deferred_failure_pending(),
            "the loop is told cheaply"
        );
        let failures = view.take_deferred_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].sections, vec!["appearance".to_owned()]);
        assert!(!failures[0].error.is_empty());
        assert!(view.take_deferred_failures().is_empty(), "reported once");
    }

    #[test]
    fn one_failed_batch_names_every_section_it_reverted_once() {
        let dir = tempfile::tempdir().unwrap();
        let (session, view) = deferring_view(dir.path());
        let parked = lock_copy(&session.paths.copy).unwrap();
        std::fs::create_dir_all(session.paths.device.parent().unwrap()).unwrap();
        std::fs::create_dir(&session.paths.device).unwrap();
        std::fs::create_dir(&session.paths.person).unwrap();
        view.update_section("appearance", object(json!({"theme": "dark"})))
            .unwrap();
        view.update_section("power", object(json!({"profile": "quiet"})))
            .unwrap();
        view.update_section("appearance", object(json!({"theme": "light"})))
            .unwrap();
        drop(parked);
        view.flush_pending();
        let failures = view.take_deferred_failures();
        assert_eq!(failures.len(), 1, "one batch, one failure");
        assert_eq!(
            failures[0].sections,
            vec!["appearance".to_owned(), "power".to_owned()],
            "each section once, in the order first written"
        );
    }

    #[test]
    fn a_durable_write_wait_does_not_block_the_last_committed_read_snapshot() {
        use std::sync::{Arc, TryLockError, mpsc};
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(open(dir.path()));
        let before = (session.load(), session.revision());
        // Another settings writer owns the real canonical copy lock. No
        // injected persistence callback replaces the production file path.
        let storage_lock = lock_copy(&session.paths.copy).unwrap();
        let writing = Arc::clone(&session);
        let writer =
            std::thread::spawn(move || writing.update_section("power", object(json!({"idle": 5}))));
        let limit = Instant::now().checked_add(Duration::from_secs(2)).unwrap();
        let mut contended = false;
        while Instant::now() < limit {
            if matches!(session.state.try_lock(), Err(TryLockError::WouldBlock)) {
                contended = true;
                break;
            }
            std::thread::yield_now();
        }
        let reading = session.host_view().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let snapshot = (reading.load(), reading.revision());
            sender.send(snapshot).unwrap();
        });
        let observed = receiver.recv_timeout(Duration::from_secs(1));
        // Always release and join before asserting, including the known-red
        // baseline. A failed read proof must not leave a locked writer behind.
        drop(storage_lock);
        writer.join().unwrap().unwrap();
        reader.join().unwrap();
        assert!(contended, "the real writer reached the held storage lock");
        assert_eq!(
            observed.expect("committed reads must remain available during storage contention"),
            before,
            "an unpersisted write must not appear in the read snapshot"
        );
        let committed = session.load();
        assert_eq!(committed["power"]["idle"], json!(5));
        assert_eq!(open(dir.path()).load(), committed);
    }

    #[test]
    fn host_view_refreshes_external_writes_and_notifies_after_the_committed_snapshot() {
        use std::sync::{Arc, mpsc};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(open(dir.path()));
        let view = session.host_view().unwrap();
        let initial_revision = view.revision();
        let observed_view = Arc::downgrade(&view);
        let (sender, receiver) = mpsc::channel();
        let listener: Arc<crate::Listener> = Arc::new(move || {
            if let Some(view) = observed_view.upgrade() {
                // This callback runs while the storage update owns its state
                // lock. Reading the public host view must not recurse into it.
                sender.send((view.load(), view.revision())).unwrap();
            }
        });
        view.changes().listen(&listener);
        crate::update_section_at(
            &session.paths.copy,
            "appearance",
            object(json!({"theme": "Light"})),
        )
        .unwrap();
        // No file watcher or manually forced refresh: ordinary view reads
        // supply the fallback when the host's watcher is unavailable.
        let _ = view.revision();
        let (document, revision) = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(document["appearance"]["theme"], json!("Light"));
        assert!(revision > initial_revision);
        assert_eq!(open(dir.path()).load(), document);
    }

    /// A view and a channel that hears each time its change signal rings.
    fn ringing_view(
        view: &std::sync::Arc<dyn SettingsSource>,
    ) -> (
        std::sync::Arc<crate::Listener>,
        std::sync::mpsc::Receiver<()>,
    ) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        let listener: std::sync::Arc<crate::Listener> = std::sync::Arc::new(move || {
            let _ = sender.lock().unwrap().send(());
        });
        view.changes().listen(&listener);
        (listener, receiver)
    }

    #[test]
    fn a_host_view_looks_at_the_copy_only_when_read() {
        use std::sync::Arc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(open(dir.path()));
        let view = session.host_view().unwrap();
        let (_listener, rings) = ringing_view(&view);
        crate::update_section_at(
            &session.paths.copy,
            "appearance",
            object(json!({"theme": "Light"})),
        )
        .unwrap();
        // No timer: the host's watch of the copy (or a read) takes the write
        // in, so an idle view costs no wakeups. 1.5 s outlasts the 1 s
        // stamp poll a reader used to drive.
        assert!(
            rings.recv_timeout(Duration::from_millis(1500)).is_err(),
            "an unread view looked at the copy on its own"
        );
        let _ = view.revision();
        rings.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(view.load()["appearance"]["theme"], json!("Light"));
    }

    /// Age the copy past [`SAME_TICK`] and take that in, so its size and
    /// time alone say it is unchanged, as for a copy last written a while ago.
    fn settle_copy(session: &SessionSettings) {
        let an_hour_ago = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&session.paths.copy)
            .unwrap()
            .set_modified(an_hour_ago)
            .unwrap();
        session.refresh();
    }

    #[test]
    fn a_settled_copy_needs_no_look_until_another_process_writes_it() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        assert!(
            session.copy_may_have_changed(),
            "a copy written within the file system's clock tick needs its contents read"
        );
        settle_copy(&session);
        assert!(!session.copy_may_have_changed());
        crate::update_section_at(
            &session.paths.copy,
            "appearance",
            object(json!({"theme": "Light"})),
        )
        .unwrap();
        assert!(session.copy_may_have_changed());
        session.refresh();
        assert_eq!(session.load()["appearance"]["theme"], json!("Light"));
    }

    /// Reads of an unchanged copy leave the view's refresh worker asleep:
    /// an idle desktop reads settings at each clock-minute repaint, and
    /// every read used to wake it (8 to 21 wakeups in one repaint).
    #[cfg(target_os = "linux")]
    #[test]
    fn reads_of_an_unchanged_copy_leave_the_refresh_worker_asleep() {
        use std::sync::Arc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(open(dir.path()));
        settle_copy(&session);
        let (view, worker) =
            tilo_test_env::idle::spawned_thread("tilo-settings-r", || session.host_view().unwrap());
        let (_listener, rings) = ringing_view(&view);
        std::thread::sleep(Duration::from_millis(50));
        let before = tilo_test_env::idle::context_switches(&worker);
        for _ in 0..50 {
            let _ = (view.load(), view.revision());
        }
        std::thread::sleep(Duration::from_millis(200));
        let woke = tilo_test_env::idle::context_switches(&worker).saturating_sub(before);
        assert_eq!(
            woke, 0,
            "50 reads of an unchanged copy woke the worker {woke} times"
        );
        crate::update_section_at(
            &session.paths.copy,
            "appearance",
            object(json!({"theme": "Light"})),
        )
        .unwrap();
        let _ = view.revision();
        rings.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert_eq!(view.load()["appearance"]["theme"], json!("Light"));
    }

    #[test]
    fn a_polling_host_view_takes_in_an_unwatched_copy_write_unread() {
        use std::sync::Arc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(open(dir.path()));
        let view = session
            .host_view_polling_copy(Duration::from_millis(50))
            .unwrap();
        let (_listener, rings) = ringing_view(&view);
        crate::update_section_at(
            &session.paths.copy,
            "appearance",
            object(json!({"theme": "Light"})),
        )
        .unwrap();
        // Nothing reads the view: its own timer finds the write.
        rings.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert_eq!(session.load()["appearance"]["theme"], json!("Light"));
        drop(view);
    }

    #[test]
    fn a_refused_record_write_never_replaces_the_host_committed_view() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(open(dir.path()));
        let view = session.host_view().unwrap();
        let before = (view.load(), view.revision());
        // A directory at the canonical record's destination makes the real
        // atomic replacement fail even when the test runner is privileged.
        std::fs::create_dir_all(&session.paths.device).unwrap();
        assert!(
            view.update_section("power", object(json!({"idle": 5})))
                .is_err()
        );
        assert_eq!((view.load(), view.revision()), before);
        assert_eq!(session.load(), before.0);
        assert_eq!(load_at(&session.paths.copy), before.0);
    }

    #[test]
    fn dropping_the_last_host_view_retires_its_pending_reader_and_record_owner() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let session = Arc::new(open(dir.path()));
        let owner = Arc::downgrade(&session);
        let view = session.host_view().unwrap();
        // Exercise the actual refresh channel before releasing custody.
        let _ = view.load();
        drop(session);
        assert!(
            owner.upgrade().is_some(),
            "the live host view owns its records"
        );
        drop(view);
        assert!(
            owner.upgrade().is_none(),
            "a queued refresh cannot retain the records after host-view teardown"
        );
    }

    #[test]
    fn a_write_lands_in_its_scopes_record_reaches_the_copy_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let paths = session.paths().clone();
        let before = session.revision();
        session
            .update_section(
                "appearance",
                object(json!({"mode": "light", "wallpaper_path": "/home/ada/sea.png"})),
            )
            .unwrap();
        assert!(session.revision() > before);

        let expected =
            json!({"appearance": {"mode": "light", "wallpaper_path": "/home/ada/sea.png"}});
        assert_eq!(Value::Object(session.load()), expected);
        assert_eq!(
            Value::Object(load_at(&paths.copy)),
            expected,
            "a process without the source reads the same document"
        );
        assert_eq!(
            Value::Object(load_at(&paths.person)),
            json!({"appearance": {"mode": "light"}}),
            "only personal leaves are the person's"
        );
        assert_eq!(
            Value::Object(load_at(&paths.device)),
            json!({"appearance": {"wallpaper_path": "/home/ada/sea.png"}}),
        );

        drop(session);
        let restarted = open(dir.path());
        assert_eq!(Value::Object(restarted.load()), expected);
    }

    #[test]
    fn origin_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let paths = session.paths().clone();
        let origin = ChangeOrigin::action("x").by("agent:alice");
        session
            .update_section_by(
                "input",
                object(json!({"touchpad_enabled": false, "tap_to_click": true})),
                &origin,
            )
            .unwrap();
        // Writing what is already stored changes nothing and records nothing.
        session
            .update_section_by("input", object(json!({"tap_to_click": true})), &origin)
            .unwrap();
        crate::update_section_at(&paths.copy, "power", object(json!({"auto_suspend": false})))
            .unwrap();
        session.refresh();

        let log = session.change_log();
        let entry = |leaf: &str| {
            log.iter()
                .find(|entry| entry["leaf"] == leaf)
                .unwrap_or_else(|| panic!("{leaf} is logged: {log:?}"))
                .clone()
        };
        assert_eq!(log.len(), 3, "one line per changed leaf: {log:?}");
        let touchpad = entry("touchpad_enabled");
        assert_eq!(touchpad["section"], "input");
        assert_eq!(touchpad["before"], Value::Null, "it was unset");
        assert_eq!(touchpad["after"], false);
        assert_eq!(touchpad["action"], "x");
        assert_eq!(touchpad["principal"], "agent:alice");
        assert_eq!(touchpad["via"], "session");
        assert_eq!(entry("tap_to_click")["action"], "x");
        let external = entry("auto_suspend");
        assert_eq!(external["section"], "power");
        assert_eq!(
            external["action"],
            Value::Null,
            "a copy take-in has no origin"
        );
        assert_eq!(external["principal"], Value::Null);
        assert_eq!(external["via"], "copy");
    }

    #[test]
    fn a_section_change_reads_and_writes_under_the_records_lock() {
        let dir = tempfile::tempdir().unwrap();
        let session = std::sync::Arc::new(open(dir.path()));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let session = std::sync::Arc::clone(&session);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        session
                            .update_section_with(
                                "input",
                                &ChangeOrigin::local("media-key.touchpad-toggle"),
                                &mut |section| {
                                    let on = section
                                        .get("touchpad_enabled")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(true);
                                    object(json!({"touchpad_enabled": !on}))
                                },
                            )
                            .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            session.load()["input"]["touchpad_enabled"],
            true,
            "200 flips from on end on only if none was lost"
        );
        assert_eq!(session.change_log().len(), 200, "every flip is attributed");
    }

    #[test]
    fn the_change_log_keeps_its_newest_half_once_full() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let filler = "x".repeat(4096);
        let mut writes = 0_u64;
        while std::fs::metadata(session.paths().change_log()).map_or(0, |meta| meta.len())
            <= change_log::CAPACITY.div_euclid(2)
            || writes < 100
        {
            writes = writes.saturating_add(1);
            session
                .update_section(
                    "notes",
                    object(json!({"text": format!("{writes}{filler}")})),
                )
                .unwrap();
        }
        let size = std::fs::metadata(session.paths().change_log())
            .unwrap()
            .len();
        assert!(size <= change_log::CAPACITY, "bounded: {size} bytes");
        let log = session.change_log();
        assert!(!log.is_empty(), "compaction keeps the newest entries");
        assert_eq!(
            log.last().unwrap()["after"],
            format!("{writes}{filler}"),
            "the newest write survives"
        );
    }

    #[test]
    fn another_processs_write_to_the_copy_reaches_the_session_and_its_records() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let paths = session.paths().clone();
        session
            .update_section("appearance", object(json!({"mode": "dark"})))
            .unwrap();
        let before = session.revision();

        crate::update_section_at(
            &paths.copy,
            "notifications",
            object(json!({"do_not_disturb": true})),
        )
        .unwrap();
        crate::update_section_at(&paths.copy, "power", object(json!({"auto_suspend": false})))
            .unwrap();

        assert!(
            session.revision() > before,
            "a reader polling the revision notices"
        );
        let expected = json!({
            "appearance": {"mode": "dark"},
            "notifications": {"do_not_disturb": true},
            "power": {"auto_suspend": false}
        });
        assert_eq!(Value::Object(session.load()), expected);
        assert_eq!(
            Value::Object(load_at(&paths.person)),
            json!({"appearance": {"mode": "dark"}, "notifications": {"do_not_disturb": true}})
        );
        assert_eq!(
            Value::Object(load_at(&paths.device)),
            json!({"power": {"auto_suspend": false}})
        );

        // The session's next write keeps what the other process wrote.
        session
            .update_section("appearance", object(json!({"mode": "light"})))
            .unwrap();
        assert_eq!(
            load_at(&paths.copy)["notifications"]["do_not_disturb"],
            true
        );
        assert_eq!(load_at(&paths.copy)["appearance"]["mode"], "light");

        drop(session);
        assert_eq!(
            open(dir.path()).load()["notifications"]["do_not_disturb"],
            true
        );
    }

    #[test]
    fn what_the_copy_gained_while_no_session_ran_is_taken_in_and_the_records_keep_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("settings.json");
        // First boot writes the copy before any session exists.
        crate::update_section_at(
            &copy,
            "input",
            object(json!({"mouse_natural_scroll": true, "touchpad_enabled": false})),
        )
        .unwrap();
        let session = open(dir.path());
        let paths = session.paths().clone();
        assert_eq!(
            Value::Object(load_at(&paths.person)),
            json!({"input": {"mouse_natural_scroll": true}})
        );
        assert_eq!(
            Value::Object(load_at(&paths.device)),
            json!({"input": {"touchpad_enabled": false}})
        );
        session
            .update_section("appearance", object(json!({"mode": "dark"})))
            .unwrap();
        drop(session);

        // While the session is down the copy is edited, and then lost.
        crate::update_section_at(&copy, "appearance", object(json!({"mode": "light"}))).unwrap();
        let session = open(dir.path());
        assert_eq!(session.load()["appearance"]["mode"], "light");
        drop(session);
        std::fs::remove_file(&copy).unwrap();
        let session = open(dir.path());
        let expected = json!({
            "appearance": {"mode": "light"},
            "input": {"mouse_natural_scroll": true, "touchpad_enabled": false}
        });
        assert_eq!(Value::Object(session.load()), expected);
        assert_eq!(
            Value::Object(load_at(&copy)),
            expected,
            "the copy is written again from the records"
        );
    }

    #[test]
    fn concurrent_writers_in_and_out_of_the_session_lose_nothing() {
        const WRITES: usize = 40;
        let dir = tempfile::tempdir().unwrap();
        let session = std::sync::Arc::new(open(dir.path()));
        let copy = session.paths().copy.clone();
        let outside = std::thread::spawn(move || {
            for i in 0..WRITES {
                crate::update_section_at(&copy, "power", object(json!({format!("k{i}"): i})))
                    .unwrap();
            }
        });
        for i in 0..WRITES {
            session
                .update_section(
                    "app_personal",
                    object(json!({format!("so.tilo.app{i}"): {"n": i}})),
                )
                .unwrap();
        }
        outside.join().unwrap();

        for doc in [session.load(), load_at(&session.paths().copy)] {
            for i in 0..WRITES {
                assert_eq!(doc["power"][format!("k{i}")], i, "{doc:?}");
                assert_eq!(
                    doc["app_personal"][format!("so.tilo.app{i}")]["n"],
                    i,
                    "{doc:?}"
                );
            }
        }
    }
}
