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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use serde_json::{Map, Value};

use crate::continuity::{SettingScope, classify};
use crate::{
    ChangeOrigin, ConditionalUpdate, SectionChange, SettingsSource, load_at, merge_section,
    write_atomic,
};

mod change_log;
mod view;

use change_log::LeafChange;

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
        let records = copy
            .parent()
            .map_or_else(|| PathBuf::from("records"), |dir| dir.join("records"));
        Self {
            person: records.join("person.json"),
            device: records.join("device.json"),
            copy,
        }
    }

    /// The records beside the configured settings file
    /// ([`crate::settings_path`]).
    #[must_use]
    pub fn configured() -> Option<Self> {
        crate::settings_path().map(Self::beside)
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
    })
}

/// Whether the copy is not what `state` last wrote or read. Size and time
/// answer unless the copy is recent enough that a second write could share
/// both; then its contents do.
fn copy_changed(path: &Path, state: &State) -> bool {
    let identity = copy_identity(path);
    // A failed copy rewrite forgets its identity. If the obstruction is
    // removed, both identities can be absent; refresh must still repair it.
    if identity != state.copy_identity || identity.is_none() {
        return true;
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
    recent && load_at(path) != state.copy
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
    state: Mutex<State>,
    revision: AtomicU64,
    changes: crate::ChangeSignal,
    committed: Mutex<std::sync::Arc<Committed>>,
}

impl SessionSettings {
    /// Open the records at `paths`, take in whatever the copy holds that they
    /// do not (writes made while no session ran), and bring the copy up to
    /// date.
    ///
    /// # Errors
    ///
    /// The copy cannot be locked, or a record or the copy cannot be written.
    pub fn open(paths: SessionPaths) -> std::io::Result<Self> {
        let _lock = lock_copy(&paths.copy)?;
        let mut state = State {
            person: load_at(&paths.person),
            device: load_at(&paths.device),
            copy: Map::new(),
            copy_identity: None,
        };
        let copy = load_at(&paths.copy);
        let previous = state.projection();
        let mut log = Vec::new();
        let touched = state.take_in(&copy, &previous, &mut log);
        persist(&paths, &state, touched)?;
        change_log::append(&paths.change_log(), &log, None);
        state.copy = copy;
        state.copy_identity = copy_identity(&paths.copy);
        write_copy(&paths, &mut state)?;
        let committed = Committed {
            document: state.projection(),
            revision: 1,
        };
        Ok(Self {
            paths,
            state: Mutex::new(state),
            revision: AtomicU64::new(1),
            changes: crate::ChangeSignal::default(),
            committed: Mutex::new(std::sync::Arc::new(committed)),
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

    fn publish_committed(&self, state: &State) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        let committed = std::sync::Arc::new(Committed {
            document: state.projection(),
            revision: self.revision.load(Ordering::Acquire),
        });
        *self
            .committed
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = committed;
        self.changes.ring();
    }

    /// Take in a write another process made to the copy since this source
    /// last looked. Readers call it through [`SettingsSource::load`] and
    /// [`SettingsSource::revision`], so a reader woken by the copy's file
    /// watch sees the write.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the state stays locked while the copy is, so one change is taken in and written at a time"
    )]
    /// Take in what another process wrote to the copy. Readers do so
    /// before they read; a host that watches the copy calls this when it
    /// changes, so those waiting for a change hear of it at once.
    pub fn refresh(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !copy_changed(&self.paths.copy, &state) {
            return;
        }
        let Ok(_lock) = lock_copy(&self.paths.copy) else {
            return;
        };
        if self.take_in_copy(&mut state).is_ok() {
            // A failed write forgets the copy's identity, so the next reader
            // tries again.
            let _ = write_copy(&self.paths, &mut state);
        }
    }

    /// With the copy locked: apply what another process wrote to it.
    fn take_in_copy(&self, state: &mut State) -> std::io::Result<()> {
        if !copy_changed(&self.paths.copy, state) {
            return Ok(());
        }
        let identity = copy_identity(&self.paths.copy);
        let copy = load_at(&self.paths.copy);
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
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _lock = lock_copy(&self.paths.copy)?;
        self.take_in_copy(&mut state)?;
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
        Ok(match write_copy(&self.paths, &mut state) {
            Ok(()) => ConditionalUpdate::Stored,
            Err(error) => ConditionalUpdate::StoredWithCopyRepair(error),
        })
    }
}

impl SettingsSource for SessionSettings {
    fn load(&self) -> Map<String, Value> {
        self.refresh();
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .projection()
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
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _lock = lock_copy(&self.paths.copy)?;
        self.take_in_copy(&mut state)?;
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
        Ok(match write_copy(&self.paths, &mut state) {
            Ok(()) => ConditionalUpdate::Stored,
            Err(error) => ConditionalUpdate::StoredWithCopyRepair(error),
        })
    }

    fn revision(&self) -> u64 {
        self.refresh();
        self.revision.load(Ordering::Acquire)
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
        std::fs::remove_file(&session.paths.copy).unwrap();
        std::fs::create_dir(&session.paths.copy).unwrap();
        assert!(matches!(
            session.compare_display_update(update).unwrap(),
            ConditionalUpdate::StoredWithCopyRepair(_)
        ));
        let durable = load_at(&session.paths.device);
        assert_eq!(durable["display_transaction"]["id"], 7);
        assert_eq!(durable["display_profiles"]["profiles"], json!({}));
        std::fs::remove_dir(&session.paths.copy).unwrap();
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
