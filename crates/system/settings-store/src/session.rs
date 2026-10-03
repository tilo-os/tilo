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
use crate::{SettingsSource, load_at, merge_section, write_atomic};

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
}

/// What tells two versions of the copy apart. Every writer replaces the file
/// by rename, so on Unix the inode alone changes with each write; the size
/// and time cover the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CopyIdentity {
    #[cfg(unix)]
    inode: (u64, u64),
    len: u64,
    modified: Option<std::time::SystemTime>,
}

fn copy_identity(path: &Path) -> Option<CopyIdentity> {
    let meta = std::fs::metadata(path).ok()?;
    Some(CopyIdentity {
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt as _;
            (meta.dev(), meta.ino())
        },
        len: meta.len(),
        modified: meta.modified().ok(),
    })
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

    /// Store `value` at `section.leaf` in the record its scope names.
    fn put(&mut self, section: &str, leaf: &str, value: Value) -> Touched {
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
        leaves.insert(leaf.to_string(), value);
        Touched {
            person: personal,
            device: !personal,
        }
    }

    /// A top-level value that is not a section (`version`): device-local.
    fn put_top_level(&mut self, name: &str, value: Value) -> Touched {
        if self.device.get(name) == Some(&value) {
            return Touched::default();
        }
        self.device.insert(name.to_string(), value);
        Touched {
            person: false,
            device: true,
        }
    }

    /// Apply every leaf of `doc` that differs from `previous`, the copy it
    /// replaced. Leaves `doc` no longer has are left alone: writers merge
    /// sections, they never remove keys.
    fn take_in(&mut self, doc: &Map<String, Value>, previous: &Map<String, Value>) -> Touched {
        let mut touched = Touched::default();
        for (name, value) in doc {
            match (value, previous.get(name)) {
                (Value::Object(leaves), before) => {
                    let before = before.and_then(Value::as_object);
                    for (leaf, value) in leaves {
                        if before.and_then(|b| b.get(leaf)) != Some(value) {
                            touched = touched.or(self.put(name, leaf, value.clone()));
                        }
                    }
                }
                (value, before) if before != Some(value) => {
                    touched = touched.or(self.put_top_level(name, value.clone()));
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
        let touched = state.take_in(&copy, &previous);
        persist(&paths, &state, touched)?;
        state.copy = copy;
        state.copy_identity = copy_identity(&paths.copy);
        write_copy(&paths, &mut state)?;
        Ok(Self {
            paths,
            state: Mutex::new(state),
            revision: AtomicU64::new(1),
        })
    }

    /// Where the records and the copy are.
    #[must_use]
    pub fn paths(&self) -> &SessionPaths {
        &self.paths
    }

    /// Take in a write another process made to the copy since this source
    /// last looked. Readers call it through [`SettingsSource::load`] and
    /// [`SettingsSource::revision`], so a reader woken by the copy's file
    /// watch sees the write.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the state stays locked while the copy is, so one change is taken in and written at a time"
    )]
    fn refresh(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if copy_identity(&self.paths.copy) == state.copy_identity {
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
        let identity = copy_identity(&self.paths.copy);
        if identity == state.copy_identity {
            return Ok(());
        }
        let copy = load_at(&self.paths.copy);
        let previous = std::mem::take(&mut state.copy);
        let touched = state.take_in(&copy, &previous);
        if let Err(error) = persist(&self.paths, state, touched) {
            state.copy = previous;
            return Err(error);
        }
        state.copy = copy;
        state.copy_identity = identity;
        if touched.person || touched.device {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        Ok(())
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

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the state stays locked while the copy is, so one change is taken in and written at a time"
    )]
    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _lock = lock_copy(&self.paths.copy)?;
        self.take_in_copy(&mut state)?;
        let (person, device) = (state.person.clone(), state.device.clone());
        let mut touched = Touched::default();
        for (leaf, value) in values {
            touched = touched.or(state.put(name, &leaf, value));
        }
        if let Err(error) = persist(&self.paths, &state, touched) {
            state.person = person;
            state.device = device;
            return Err(error);
        }
        if touched.person || touched.device {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        // The records keep the change even when the copy cannot be written;
        // the next change or refresh writes it again.
        write_copy(&self.paths, &mut state)
    }

    fn revision(&self) -> u64 {
        self.refresh();
        self.revision.load(Ordering::Acquire)
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
fn lock_copy(path: &Path) -> std::io::Result<advisory_lock::FileLock> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    advisory_lock::FileLock::open(&lock_path(path), advisory_lock::LockMode::Exclusive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn a_write_lands_in_its_scopes_record_reaches_the_copy_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let paths = session.paths().clone();
        let before = session.revision();
        session
            .update_section(
                "appearance",
                object(json!({"theme": "Light", "wallpaper_path": "/home/ada/sea.png"})),
            )
            .unwrap();
        assert!(session.revision() > before);

        let expected =
            json!({"appearance": {"theme": "Light", "wallpaper_path": "/home/ada/sea.png"}});
        assert_eq!(Value::Object(session.load()), expected);
        assert_eq!(
            Value::Object(load_at(&paths.copy)),
            expected,
            "a process without the source reads the same document"
        );
        assert_eq!(
            Value::Object(load_at(&paths.person)),
            json!({"appearance": {"theme": "Light"}}),
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
    fn another_processs_write_to_the_copy_reaches_the_session_and_its_records() {
        let dir = tempfile::tempdir().unwrap();
        let session = open(dir.path());
        let paths = session.paths().clone();
        session
            .update_section("appearance", object(json!({"theme": "Dark"})))
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
            "appearance": {"theme": "Dark"},
            "notifications": {"do_not_disturb": true},
            "power": {"auto_suspend": false}
        });
        assert_eq!(Value::Object(session.load()), expected);
        assert_eq!(
            Value::Object(load_at(&paths.person)),
            json!({"appearance": {"theme": "Dark"}, "notifications": {"do_not_disturb": true}})
        );
        assert_eq!(
            Value::Object(load_at(&paths.device)),
            json!({"power": {"auto_suspend": false}})
        );

        // The session's next write keeps what the other process wrote.
        session
            .update_section("appearance", object(json!({"theme": "Light"})))
            .unwrap();
        assert_eq!(
            load_at(&paths.copy)["notifications"]["do_not_disturb"],
            true
        );
        assert_eq!(load_at(&paths.copy)["appearance"]["theme"], "Light");

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
            object(json!({"natural_scroll": true, "touchpad_enabled": false})),
        )
        .unwrap();
        let session = open(dir.path());
        let paths = session.paths().clone();
        assert_eq!(
            Value::Object(load_at(&paths.person)),
            json!({"input": {"natural_scroll": true}})
        );
        assert_eq!(
            Value::Object(load_at(&paths.device)),
            json!({"input": {"touchpad_enabled": false}})
        );
        session
            .update_section("appearance", object(json!({"theme": "Dark"})))
            .unwrap();
        drop(session);

        // While the session is down the copy is edited, and then lost.
        crate::update_section_at(&copy, "appearance", object(json!({"theme": "Light"}))).unwrap();
        let session = open(dir.path());
        assert_eq!(session.load()["appearance"]["theme"], "Light");
        drop(session);
        std::fs::remove_file(&copy).unwrap();
        let session = open(dir.path());
        let expected = json!({
            "appearance": {"theme": "Light"},
            "input": {"natural_scroll": true, "touchpad_enabled": false}
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
