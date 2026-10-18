//! The shared **desktop settings** file: one path resolver, one forgiving read, one
//! non-destructive write.
//!
//! # Why one crate
//!
//! `settings.json` is the contract between the Settings app (owner), several apps that read
//! a slice of it, and the compositor. Before this crate, six readers each re-derived where
//! the file lives in three different ways — hand-rolled `XDG_CONFIG_HOME` → `$HOME/.config`,
//! the same via `dirs::home_dir()`, and `dirs::config_dir()` — and those are not equivalent.
//! The XDG spec says `XDG_CONFIG_HOME` must be **absolute** and a relative value must be
//! ignored; `dirs::config_dir()` implements that, the hand-rolled resolvers did not (they
//! joined a relative value against the process's working directory). So under a relative
//! `XDG_CONFIG_HOME`, the compositor and power policy read one file while other apps read
//! another, silently. [`settings_path`] now resolves it once, following the spec, through
//! the workspace's shared resolver (`tilo-paths`).
//!
//! # The shape on disk
//!
//! One pretty-printed JSON object of independent sections (`display`, `region`,
//! `appearance`, `power`, …), each owned by whichever pane or app writes it. Nothing owns the
//! document as a whole, so [`update_section`] merges rather than replaces — panes compile
//! into different binaries shipping at different times, and a writer that serialised its own
//! view would silently drop sections it didn't know about.
//!
//! # Who holds the truth
//!
//! In a session the file is a copy. The shell keeps the person's record and
//! the device's record ([`SessionSettings`]), reads and writes them through
//! the installed [`SettingsSource`], and rewrites `settings.json` after every
//! change for the processes that read it instead. Their section writes keep
//! working: the session takes them in from the copy. A sandboxed app sees
//! neither: it asks the shell, which answers with the part of the records
//! [`app_scope`] gives that app.
//!
//! # Reading is always forgiving
//!
//! A missing file, unreadable file, malformed JSON, a section of the wrong type, or a value
//! out of range must never take an app or session down — every read path returns the
//! caller's default instead.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

/// `Serialize`/`Deserialize` of a type deriving them with
/// `#[serde(remote = "Self")]`: writing runs the derived code, reading goes
/// through one `serde_json::Value` and one function compiled here, rather
/// than a derived decoder per deserializer in every crate reading the type.
/// The calling crate needs `serde` and `serde_json`.
#[doc(hidden)]
#[macro_export]
macro_rules! read_through_json_value {
    ($ty:ty) => {
        impl serde::Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                Self::serialize(self, serializer)
            }
        }
        $crate::read_through_json_value!($ty, deserialize);
    };
    ($ty:ty, deserialize) => {
        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
                Self::from_json_value(value).map_err(serde::de::Error::custom)
            }
        }

        impl $ty {
            #[inline(never)]
            fn from_json_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
                Self::deserialize(value)
            }
        }
    };
}
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub mod accessibility;
pub mod app_scope;
pub mod appearance;
pub mod continuity;
pub mod continuity_privacy;
pub mod day_schedule;
pub mod desk_widgets;
pub mod device_displays;
pub mod dictation;
pub mod display_control;
pub mod display_identify;
pub mod display_mode;
pub mod display_placement;
pub mod display_scale;
pub mod display_tools;
pub mod display_tuning;
pub mod hardware;
pub mod index;
mod key;
pub mod keyboard_shortcuts;
pub mod keyboard_sources;
#[cfg(feature = "watch")]
pub mod live;
pub mod magnifier;
pub mod notification_policy;
pub mod paste_shortcut;
pub mod person_displays;
pub mod personal_dictionary;
pub mod power;
pub mod published;
pub mod roaming_provenance;
mod section_digest;
mod session;
mod source;
pub mod system_sounds;
#[cfg(feature = "watch")]
pub mod watch;

pub use key::{Key, SectionWrite};
pub use section_digest::SectionDigest;
pub use session::{SessionPaths, SessionSettings};
pub use source::{
    ChangeOrigin, ChangeSignal, ConditionalUpdate, DeferredFailure, DisplayUpdate, HostDocument,
    Listener, SectionChange, SettingsSource, defer_writes_on_this_thread, deferred_failure_pending,
    flush_pending_writes, install_source, installed_source, merge_section, take_deferred_failures,
    with_installed_committed, writes_deferred_on_this_thread,
};

/// The directory under the Tilo config root that holds the settings document
/// and nothing else of Tilo's: its lock, and the temp files a replace leaves
/// for a moment. Settings, sandboxed, is given this directory and not the
/// root beside it, which holds the identity store, the trust lists and the
/// workspace tree.
pub const SESSION_DIR: &str = "session";

/// The settings document's file name.
const FILE: &str = "settings.json";

/// The per-user Tilo config root: `$XDG_CONFIG_HOME/tilo`, else `$HOME/.config/tilo`. `None`
/// only when neither variable yields a usable path. Per the XDG spec, `XDG_CONFIG_HOME` is
/// honoured only when set, non-empty **and absolute** — a relative value falls back to
/// `$HOME`, and getting that rule wrong in one reader and right in another is what made this
/// crate necessary.
pub fn config_root() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join("tilo"))
}

/// Resolve one settings file under the config root.
pub fn config_path(relative: &Path) -> Option<PathBuf> {
    Some(config_root()?.join(relative))
}

/// Env a sandbox sets when the session `settings.json` is bound at a path
/// other than the remapped `$XDG_CONFIG_HOME`. Empty is ignored.
pub const SESSION_SETTINGS_ENV: &str = "TILO_SESSION_SETTINGS";

/// The shared settings file: `<config root>/session/settings.json`, or the
/// session document named by [`SESSION_SETTINGS_ENV`] when a sandbox
/// remapped XDG.
///
/// The document used to live at `<config root>/settings.json`. Resolving the
/// path moves a document still there into place, once
/// ([`adopt_legacy_document`]), so neither the session nor a reader started
/// before the session loses the person's settings.
pub fn settings_path() -> Option<PathBuf> {
    match std::env::var_os(SESSION_SETTINGS_ENV) {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => {
            let root = config_root()?;
            let path = settings_path_in(&root);
            adopt_legacy_document(&root, &path);
            Some(path)
        }
    }
}

/// Where the settings document lives under the config root `root`.
pub fn settings_path_in(root: &Path) -> PathBuf {
    root.join(SESSION_DIR).join(FILE)
}

/// Move a settings document left at `<root>/settings.json` into
/// [`SESSION_DIR`], unless a document is already there. The move never
/// replaces one, so two processes resolving the path at once move it once.
/// Failing leaves the old document where it was and reads start from
/// defaults, as for any unreadable document.
///
/// Remembers the last path it found settled, so the readers that poll the
/// document's stamp do not look for the old one on every poll.
fn adopt_legacy_document(root: &Path, path: &Path) {
    static SETTLED: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
    let mut settled = SETTLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if settled.as_deref() == Some(path) {
        return;
    }
    match move_legacy_document(root) {
        Ok(true) => *settled = Some(path.to_path_buf()),
        Ok(false) => {}
        Err(error) => eprintln!(
            "settings-store: could not move {} into {}: {error}",
            root.join(FILE).display(),
            root.join(SESSION_DIR).display()
        ),
    }
}

/// Whether the document is in [`SESSION_DIR`] under `root` once this returns:
/// it was there, or the one left at the old path moved there.
fn move_legacy_document(root: &Path) -> Result<bool, tilo_files::FsError> {
    use tilo_files::{FsError, Kind, Location, VPath, blocking};
    let invalid = |_| FsError::InvalidPath("settings document path");
    let dir = match tilo_files::Dir::open(Location::Native(root.to_path_buf())) {
        Ok(dir) => dir,
        Err(FsError::NotFound) => return Ok(false),
        Err(error) => return Err(error),
    };
    let current = VPath::new(&format!("{SESSION_DIR}/{FILE}")).map_err(invalid)?;
    if blocking::metadata(&dir, &current).is_ok() {
        return Ok(true);
    }
    let legacy = VPath::new(FILE).map_err(invalid)?;
    match blocking::metadata(&dir, &legacy) {
        Ok(meta) if meta.kind == Kind::File && !meta.symlink => {}
        _ => return Ok(false),
    }
    blocking::create_dir_all(&dir, &VPath::new(SESSION_DIR).map_err(invalid)?)?;
    match blocking::rename_noreplace(&dir, &legacy, &current) {
        Ok(()) | Err(FsError::AlreadyExists | FsError::NotFound) => Ok(true),
        Err(error) => Err(error),
    }
}

/// Read and parse the whole document. Returns an empty object for a missing, unreadable or
/// malformed file, so callers never distinguish "absent" from "broken" — both mean "use
/// defaults". A host-installed [`SettingsSource`] replaces the file.
pub fn load() -> Map<String, Value> {
    if let Some(source) = installed_source() {
        return source.load();
    }
    match settings_path() {
        Some(p) => load_at(&p),
        None => Map::new(),
    }
}

/// Identifies one version of the settings document, for readers that cache it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsStamp {
    /// The settings file's modification time.
    Modified(std::time::SystemTime),
    /// A host-installed source's [`SettingsSource::revision`].
    Revision(u64),
}

/// Whether a poller that last observed `cached` must parse the document again.
///
/// The first observation (`seen == false`) loads even when both stamps are
/// `None` (no file yet). Later polls skip the parse while the stamp holds,
/// so an idle shell does not re-read an unchanged settings document.
pub fn document_changed(
    seen: bool,
    cached: Option<SettingsStamp>,
    current: Option<SettingsStamp>,
) -> bool {
    !seen || cached != current
}

/// The current [`SettingsStamp`]: the installed source's revision, else the file's
/// modification time. `None` when there is no readable file.
pub fn stamp() -> Option<SettingsStamp> {
    if let Some(source) = installed_source() {
        return Some(SettingsStamp::Revision(source.revision()));
    }
    stamp_at(&settings_path()?)
}

/// [`stamp`] of the document at `path`, ignoring any installed source.
pub fn stamp_at(path: &Path) -> Option<SettingsStamp> {
    published::note_file_read();
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(SettingsStamp::Modified(modified))
}

/// [`load`] from an explicit path. Exposed so tests can drive a scratch file without
/// touching the configured settings file.
pub fn load_at(path: &Path) -> Map<String, Value> {
    published::note_file_read();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Map::new();
    };
    parse(&text)
}

/// Parse a settings document body. Anything that is not a JSON object (truncated write,
/// stray array, empty file) reads as "no settings".
pub fn parse(text: &str) -> Map<String, Value> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Read one top-level section as an object. A missing section, or one that is not an
/// object, yields an empty map.
pub fn section(text: &str, name: &str) -> Map<String, Value> {
    match parse(text).get(name) {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// Read one top-level section from the configured settings file.
pub fn load_section(name: &str) -> Map<String, Value> {
    object_section(load(), name)
}

/// [`load_section`] from an explicit settings file.
pub fn load_section_at(path: &Path, name: &str) -> Map<String, Value> {
    object_section(load_at(path), name)
}

fn object_section(mut doc: Map<String, Value>, name: &str) -> Map<String, Value> {
    match doc.remove(name) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Merge `values` into the top-level section `name`, preserving every other section and
/// every key of `name` that `values` does not mention. Creates the config directory and
/// file on first use, and writes through a temp file + rename so a crash mid-write can't
/// leave a half-written document — which would otherwise reset every pane at once.
///
/// This is the raw, untyped write every other write reduces to. Production code names a
/// setting through a [`Key`] (one leaf) or a [`SectionWrite`] (several leaves of one
/// section) instead, so a misspelt section or a mistyped value cannot compile.
/// `tools/policy/check_settings_writes.py` refuses a string-literal section name here
/// outside this crate; the callers left are the ones that move a section they own as a
/// whole: the framework's `ui::storage` scopes and app settings channel, the device
/// fabric's roamed write-back, the Settings app's multi-section document writers and
/// schema owners that name their section through their own constant.
///
/// # Errors
///
/// Returns an error if no config directory is available (see
/// [`config_root`]), or [`update_section_at`] fails. With a host-installed
/// [`SettingsSource`], the source's error.
pub fn update_section(name: &str, values: Map<String, Value>) -> std::io::Result<()> {
    if let Some(source) = installed_source() {
        return source.update_section(name, values);
    }
    update_section_at(&file_write_path()?, name, values)
}

/// [`update_section`], recording who and what made the change with every leaf it changes.
/// Only the session's records keep a history; a process writing the file directly is
/// taken in by the session with no origin.
///
/// # Errors
///
/// As [`update_section`].
pub fn update_section_by(
    name: &str,
    values: Map<String, Value>,
    origin: &ChangeOrigin,
) -> std::io::Result<()> {
    if let Some(source) = installed_source() {
        return source.update_section_by(name, values, origin);
    }
    update_section_at(&file_write_path()?, name, values)
}

/// Read section `name` and merge what `change` makes of it, as one step no other write
/// lands inside: under the installed source's lock, else under the file's lock. `change`
/// is called once. This is the read-modify-write behind [`Key::update`].
///
/// # Errors
///
/// As [`update_section`].
pub fn update_section_with(
    name: &str,
    origin: &ChangeOrigin,
    change: &mut SectionChange<'_>,
) -> std::io::Result<()> {
    if let Some(source) = installed_source() {
        return source.update_section_with(name, origin, change);
    }
    update_section_with_at(&file_write_path()?, name, change)
}

/// [`update_section_with`] on the settings file at `path`, under its lock. Taking the lock
/// creates the directory, so that even the first write is taken under it.
///
/// # Errors
///
/// The file cannot be locked, or the document can't be serialised or written.
pub fn update_section_with_at(
    path: &Path,
    name: &str,
    change: &mut SectionChange<'_>,
) -> std::io::Result<()> {
    let _lock = session::lock_copy(path)?;
    let mut doc = load_for_update(path)?;
    let current = match doc.get(name) {
        Some(Value::Object(section)) => section.clone(),
        _ => Map::new(),
    };
    merge_section(&mut doc, name, change(&current));
    write_atomic(path, &Value::Object(doc))
}

/// The canonical file path, with the same test isolation guard as writes.
///
/// # Errors
/// No configuration directory exists, or this is an unisolated test process.
pub fn file_write_path() -> std::io::Result<PathBuf> {
    let Some(path) = settings_path() else {
        return Err(std::io::Error::other("no config directory available"));
    };
    if let Some(reason) = std::env::current_exe().ok().and_then(|exe| {
        unisolated_test_write(
            &exe,
            std::env::var_os("TILO_TEST_ENVIRONMENT").is_some(),
            std::env::var_os("XDG_CONFIG_HOME").is_some(),
        )
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("refusing to write {}: {reason}", path.display()),
        ));
    }
    Ok(path)
}

/// Why a write through the default path would land in the settings of whoever started this
/// test harness, or `None`.
///
/// Cargo starts test binaries inside `tools/with-test-environment.sh` (a fresh HOME). A
/// harness started by hand — `target/debug/deps/<crate>-<hash>` — keeps the caller's HOME,
/// and a test that saves a setting would rewrite their real `settings.json`, as fixture
/// values like a Do Not Disturb switch or a manual status once did. A test that points
/// `XDG_CONFIG_HOME` at its own directory still writes there.
fn unisolated_test_write(
    exe: &Path,
    in_test_environment: bool,
    config_home_set: bool,
) -> Option<&'static str> {
    let harness = exe
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "deps");
    (harness && !in_test_environment && !config_home_set).then_some(
        "a test harness outside tools/with-test-environment.sh would write the caller's own \
         settings; run it through cargo or that script",
    )
}

// Display reads may be forgiving; a writer must not erase a file it cannot read.
// Malformed JSON remains repairable, but I/O and UTF-8 failures are preserved.
fn load_for_update(path: &Path) -> std::io::Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse(&text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(error),
    }
}

/// [`update_section`] against an explicit path.
///
/// # Errors
///
/// Returns an error if an existing document can't be read, or the merged document can't
/// be serialised or written. A missing file starts empty; malformed JSON is still repaired.
pub fn update_section_at(
    path: &Path,
    name: &str,
    values: Map<String, Value>,
) -> std::io::Result<()> {
    update_document_at(path, |doc| merge_section(doc, name, values))
}

/// Conditionally merge under the canonical file lock. Missing or replaced
/// sections refuse the old intent; successful replacement is the receipt.
///
/// # Errors
/// The file cannot be locked, serialised or durably replaced.
pub fn compare_update_section_at(
    path: &Path,
    name: &str,
    expected: &Map<String, Value>,
    values: Map<String, Value>,
) -> std::io::Result<ConditionalUpdate> {
    let _lock = session::lock_copy(path)?;
    let mut doc = load_for_update(path)?;
    if doc.get(name) != Some(&Value::Object(expected.clone())) {
        return Ok(ConditionalUpdate::Superseded);
    }
    merge_section(&mut doc, name, values);
    write_atomic(path, &Value::Object(doc))?;
    Ok(ConditionalUpdate::Stored)
}

/// Persist only display transaction/profile sections under their canonical
/// file lock. This is the file-backed equivalent of the device-record receipt.
///
/// # Errors
/// Storage cannot create, lock or atomically replace the settings file.
pub fn compare_display_update_at(
    path: &Path,
    update: DisplayUpdate,
) -> std::io::Result<ConditionalUpdate> {
    let _lock = session::lock_copy(path)?;
    let mut document = load_for_update(path)?;
    if !update.matches(&document) {
        return Ok(ConditionalUpdate::Superseded);
    }
    for (section, values) in update.sections() {
        merge_section(&mut document, section, values);
    }
    write_atomic(path, &Value::Object(document))?;
    Ok(ConditionalUpdate::Stored)
}

/// Read the document at `path`, let `change` edit it, and replace it, all
/// under the file's lock. For a writer that owns several sections at once;
/// one that writes a section uses [`update_section_at`].
///
/// A session keeps its records behind the file and takes in what other
/// writers change in it ([`SessionSettings`]); the lock keeps each read,
/// change and replace whole. The first writer also creates and locks the
/// directory before reading, so concurrent first writes cannot lose edits.
///
/// # Errors
///
/// The file cannot be locked, or the document can't be serialised or
/// written.
pub fn update_document_at(
    path: &Path,
    change: impl FnOnce(&mut Map<String, Value>),
) -> std::io::Result<()> {
    let _lock = session::lock_copy(path)?;
    let mut doc = load_for_update(path)?;
    change(&mut doc);
    write_atomic(path, &Value::Object(doc))
}

/// Serialise and replace the document. `tilo_files::native::atomic` does the crash-safe part: a truncated
/// `settings.json` would reset every pane at once.
fn write_atomic(path: &Path, doc: &Value) -> std::io::Result<()> {
    let mut body = serde_json::to_string_pretty(doc).map_err(std::io::Error::other)?;
    body.push('\n');
    tilo_files::native::atomic::write_creating_parent(path, body.as_bytes())
}

/// Read a `f64` at `section.key`, clamped into `range`, falling back to `default` when
/// absent, the wrong type, or out of range.
pub fn f64_in(
    doc: &Map<String, Value>,
    section: &str,
    key: &str,
    range: std::ops::RangeInclusive<f64>,
    default: f64,
) -> f64 {
    doc.get(section)
        .and_then(|s| s.get(key))
        .and_then(serde_json::Value::as_f64)
        .filter(|v| range.contains(v))
        .unwrap_or(default)
}

/// Read a `&str` at `section.key`.
pub fn str_at<'a>(doc: &'a Map<String, Value>, section: &str, key: &str) -> Option<&'a str> {
    doc.get(section)
        .and_then(|s| s.get(key))
        .and_then(|v| v.as_str())
}

/// Read a `bool` at `section.key`.
pub fn bool_at(doc: &Map<String, Value>, section: &str, key: &str) -> Option<bool> {
    doc.get(section)
        .and_then(|s| s.get(key))
        .and_then(serde_json::Value::as_bool)
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMPDIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn an_unchanged_settings_stamp_does_not_parse_again() {
        let stamp = SettingsStamp::Revision(1);
        assert!(document_changed(false, None, None));
        assert!(!document_changed(true, None, None));
        assert!(!document_changed(true, Some(stamp), Some(stamp)));
        assert!(document_changed(
            true,
            Some(stamp),
            Some(SettingsStamp::Revision(2))
        ));
    }

    #[test]
    fn a_hand_started_test_harness_never_writes_the_callers_settings() {
        // This test binary is itself a harness under `deps/`: with the test environment's
        // marker and XDG_CONFIG_HOME gone, the default path is the fake caller's own
        // `~/.config`, and the write must be refused before anything lands there.
        let home = tmpdir();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &home)
            .remove("XDG_CONFIG_HOME")
            .remove("TILO_TEST_ENVIRONMENT");
        let mut values = Map::new();
        values.insert("do_not_disturb".into(), Value::Bool(true));
        let error = update_section("notifications", values.clone())
            .expect_err("a hand-started harness wrote the caller's settings");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!home.join(".config/tilo/session/settings.json").exists());
        assert!(!home.join(".config/tilo/settings.json").exists());

        // A test that names its own config directory still writes there.
        let config = home.join("own-config");
        env.set("XDG_CONFIG_HOME", &config);
        update_section("notifications", values).expect("an explicit config directory");
        assert!(config.join("tilo/session/settings.json").is_file());
        drop(env);
        let _ = std::fs::remove_dir_all(&home);

        // Installed binaries and `cargo run` targets are never harnesses.
        for exe in ["/usr/bin/tilo-shell", "/work/target/debug/settings"] {
            assert_eq!(
                unisolated_test_write(Path::new(exe), false, false),
                None,
                "{exe}"
            );
        }
        let harness = Path::new("/work/target/debug/deps/settings_store-0123456789abcdef");
        assert!(unisolated_test_write(harness, false, false).is_some());
        assert_eq!(unisolated_test_write(harness, true, false), None);
    }

    pub(super) fn tmpdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "tilo-settings-store-{}-{:?}-{}",
            std::process::id(),
            tilo_clock::try_unix_now_ns().unwrap(),
            TMPDIR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn first_document_write_holds_the_lock_before_running_the_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new-config/tilo/settings.json");
        assert!(!path.parent().unwrap().exists());
        update_document_at(&path, |doc| {
            let (root, name) = tilo_files::file_at(&session::lock_path(&path)).unwrap();
            let competing =
                tilo_files::blocking::try_lock(&root, &name, tilo_files::LockMode::Exclusive)
                    .unwrap();
            assert!(
                competing.is_none(),
                "a competing writer must wait for the first edit"
            );
            doc.insert("region".into(), json!({"units": "metric"}));
        })
        .unwrap();
        update_section_at(
            &path,
            "appearance",
            json!({"dark_theme": "dark"}).as_object().unwrap().clone(),
        )
        .unwrap();
        assert_eq!(load_at(&path)["region"], json!({"units": "metric"}));
        assert_eq!(load_at(&path)["appearance"], json!({"dark_theme": "dark"}));
    }

    #[test]
    fn conditional_write_to_a_missing_directory_is_superseded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new-config/tilo/settings.json");
        let result =
            compare_update_section_at(&path, "display_transaction", &Map::new(), Map::new())
                .unwrap();
        assert!(matches!(result, ConditionalUpdate::Superseded));
        assert!(!path.exists(), "a stale answer must not create settings");
    }

    #[test]
    fn conditional_file_answers_reject_replacement_and_preserve_other_sections() {
        let dir = tmpdir();
        let path = dir.join("settings.json");
        let expected = json!({"id":1,"state":"awaiting_confirmation"})
            .as_object()
            .unwrap()
            .clone();
        let answer = json!({"id":1,"state":"confirm_requested"})
            .as_object()
            .unwrap()
            .clone();
        update_section_at(&path, "display_transaction", expected.clone()).unwrap();
        update_section_at(
            &path,
            "region",
            json!({"units":"metric"}).as_object().unwrap().clone(),
        )
        .unwrap();
        assert!(matches!(
            compare_update_section_at(&path, "display_transaction", &expected, answer.clone())
                .unwrap(),
            ConditionalUpdate::Stored
        ));
        assert_eq!(
            load_at(&path)["display_transaction"],
            Value::Object(answer.clone())
        );
        assert_eq!(load_at(&path)["region"], json!({"units":"metric"}));
        let replacement = json!({"id":2,"state":"awaiting_confirmation"})
            .as_object()
            .unwrap()
            .clone();
        update_section_at(&path, "display_transaction", replacement.clone()).unwrap();
        assert!(matches!(
            compare_update_section_at(&path, "display_transaction", &expected, answer).unwrap(),
            ConditionalUpdate::Superseded
        ));
        assert_eq!(
            load_at(&path)["display_transaction"],
            Value::Object(replacement)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Path resolution: the divergence this crate ends

    #[test]
    fn a_session_settings_env_wins_over_the_remapped_xdg_root() {
        {
            let session = tmpdir();
            let xdg = tmpdir();
            let session_file = session.join("settings.json");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set(SESSION_SETTINGS_ENV, &session_file);
            assert_eq!(settings_path(), Some(session_file));
            drop(env);
            let _ = std::fs::remove_dir_all(&session);
            let _ = std::fs::remove_dir_all(&xdg);
        }
    }

    #[test]
    fn the_document_lives_in_its_own_directory_and_an_old_one_moves_there() {
        // Phase `a_fresh_root_names_the_session_directory`.
        {
            let xdg = tmpdir();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .remove(SESSION_SETTINGS_ENV);
            let path = settings_path().expect("a config root");
            assert_eq!(path, xdg.join("tilo/session/settings.json"));
            update_section(
                "region",
                json!({"units": "metric"}).as_object().unwrap().clone(),
            )
            .unwrap();
            assert_eq!(
                load_section_at(&xdg.join("tilo/session/settings.json"), "region")["units"],
                "metric"
            );
            assert!(
                !xdg.join("tilo/settings.json").exists(),
                "nothing is written beside the identity store and trust lists"
            );
            drop(env);
            std::fs::remove_dir_all(&xdg).unwrap();
        }
        // Phase `a_document_at_the_old_path_moves_once_and_keeps_every_section`.
        {
            let xdg = tmpdir();
            let root = xdg.join("tilo");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("settings.json"),
                r#"{"appearance":{"theme":"Light"},"region":{"units":"imperial"}}"#,
            )
            .unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .remove(SESSION_SETTINGS_ENV);
            assert_eq!(load_section("appearance")["theme"], "Light");
            assert_eq!(load_section("region")["units"], "imperial");
            assert!(!root.join("settings.json").exists(), "the old copy is gone");
            assert!(root.join("session/settings.json").is_file());
            // A later document at the old path is not taken over the current one.
            std::fs::write(
                root.join("settings.json"),
                r#"{"region":{"units":"metric"}}"#,
            )
            .unwrap();
            assert_eq!(load_section("region")["units"], "imperial");
            drop(env);
            std::fs::remove_dir_all(&xdg).unwrap();
        }
        // Phase `the_session_records_stay_under_the_root`.
        {
            let xdg = tmpdir();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .remove(SESSION_SETTINGS_ENV);
            let paths = SessionPaths::configured().expect("a config root");
            assert_eq!(paths.copy, xdg.join("tilo/session/settings.json"));
            assert_eq!(paths.person, xdg.join("tilo/records/person.json"));
            assert_eq!(paths.device, xdg.join("tilo/records/device.json"));
            drop(env);
            std::fs::remove_dir_all(&xdg).unwrap();
        }
    }

    // Forgiving reads

    #[test]
    fn reads_forgive_broken_missing_and_mistyped_documents() {
        // Phase `broken_documents_read_as_no_settings`.
        {
            for text in ["", "not json {", "[1,2,3]", "null", "\"a string\""] {
                assert!(parse(text).is_empty(), "{text:?} must read as no settings");
                assert!(section(text, "display").is_empty());
            }
        }
        // Phase `a_missing_file_reads_as_no_settings`.
        {
            let dir = tmpdir();
            assert!(load_at(&dir.join("absent.json")).is_empty());
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `a_section_of_the_wrong_type_reads_as_absent`.
        {
            let text = r#"{"display": 3, "region": ["x"], "power": {"idle": 5}}"#;
            assert!(
                section(text, "display").is_empty(),
                "a number is not a section"
            );
            assert!(
                section(text, "region").is_empty(),
                "an array is not a section"
            );
            assert_eq!(section(text, "power").get("idle"), Some(&json!(5)));
        }
        // Phase `typed_readers_clamp_and_fall_back`.
        {
            let doc = parse(r#"{"display":{"scale":1.5,"mode":"dark","hidpi":true,"bad":"x"}}"#);
            assert_eq!(f64_in(&doc, "display", "scale", 0.5..=3.0, 1.0), 1.5);
            // Out of range → the default, never the stored value.
            assert_eq!(f64_in(&doc, "display", "scale", 0.5..=1.2, 1.0), 1.0);
            // Wrong type and absent both → the default.
            assert_eq!(f64_in(&doc, "display", "bad", 0.0..=9.0, 7.0), 7.0);
            assert_eq!(f64_in(&doc, "display", "nope", 0.0..=9.0, 7.0), 7.0);
            assert_eq!(f64_in(&doc, "absent", "scale", 0.0..=9.0, 7.0), 7.0);

            assert_eq!(str_at(&doc, "display", "mode"), Some("dark"));
            assert_eq!(
                str_at(&doc, "display", "scale"),
                None,
                "a number is not a string"
            );
            assert_eq!(bool_at(&doc, "display", "hidpi"), Some(true));
            assert_eq!(bool_at(&doc, "display", "mode"), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_merge_refuses_unreadable_input_without_replacing_it() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A symlink loop fails even for privileged test runners, unlike mode bits.
        symlink("settings.json", &path).unwrap();
        let read_error = std::fs::read_to_string(&path).unwrap_err();
        let error = update_section_at(&path, "display", Map::new()).unwrap_err();
        assert_eq!(error.raw_os_error(), read_error.raw_os_error());
        assert_eq!(
            std::fs::read_link(&path).unwrap(),
            Path::new("settings.json")
        );
    }

    #[test]
    fn a_merge_does_not_replace_non_utf8_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let bytes = b"{\"region\":\"\xff\"}";
        std::fs::write(&path, bytes).unwrap();
        let error = update_section_at(&path, "display", Map::new()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    // Non-destructive writes

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn section_writes_merge_preserve_and_repair_atomically() {
        // Phase `writing_one_section_preserves_every_other`.
        {
            // The reason writes merge: the panes are separate binaries, and one that serialised its
            // own view would drop the sections it predates.
            let dir = tmpdir();
            let path = dir.join("settings.json");
            std::fs::write(
                &path,
                r#"{"version":1,"display":{"scale":1.5},"region":{"units":"imperial"}}"#,
            )
            .unwrap();

            let mut vals = Map::new();
            vals.insert("idle_suspend".into(), json!(600));
            update_section_at(&path, "power", vals).unwrap();

            let doc = load_at(&path);
            assert_eq!(doc.get("version"), Some(&json!(1)), "unknown keys survive");
            assert_eq!(f64_in(&doc, "display", "scale", 0.0..=9.0, 0.0), 1.5);
            assert_eq!(str_at(&doc, "region", "units"), Some("imperial"));
            assert_eq!(
                doc.get("power").unwrap().get("idle_suspend"),
                Some(&json!(600))
            );
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `writing_a_section_merges_into_it_rather_than_replacing_it`.
        {
            let dir = tmpdir();
            let path = dir.join("settings.json");
            std::fs::write(&path, r#"{"region":{"units":"metric","first_day":"mon"}}"#).unwrap();

            let mut vals = Map::new();
            vals.insert("units".into(), json!("imperial"));
            update_section_at(&path, "region", vals).unwrap();

            let doc = load_at(&path);
            assert_eq!(
                str_at(&doc, "region", "units"),
                Some("imperial"),
                "the write lands"
            );
            assert_eq!(
                str_at(&doc, "region", "first_day"),
                Some("mon"),
                "a key the writer did not mention must survive"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `writing_creates_the_directory_and_the_file`.
        {
            let dir = tmpdir();
            let path = dir.join("nested").join("deeper").join("settings.json");
            let mut vals = Map::new();
            vals.insert("scale".into(), json!(2.0));
            update_section_at(&path, "display", vals).unwrap();
            assert_eq!(
                f64_in(&load_at(&path), "display", "scale", 0.0..=9.0, 0.0),
                2.0
            );
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `writing_over_a_corrupt_file_does_not_propagate_the_corruption`.
        {
            let dir = tmpdir();
            let path = dir.join("settings.json");
            std::fs::write(&path, "{ truncated").unwrap();

            let mut vals = Map::new();
            vals.insert("scale".into(), json!(1.25));
            update_section_at(&path, "display", vals).unwrap();

            let doc = load_at(&path);
            assert_eq!(f64_in(&doc, "display", "scale", 0.0..=9.0, 0.0), 1.25);
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `a_section_stored_as_the_wrong_type_is_replaced_on_write`.
        {
            let dir = tmpdir();
            let path = dir.join("settings.json");
            std::fs::write(&path, r#"{"display": "not an object", "keep": 1}"#).unwrap();

            let mut vals = Map::new();
            vals.insert("scale".into(), json!(1.0));
            update_section_at(&path, "display", vals).unwrap();

            let doc = load_at(&path);
            assert_eq!(f64_in(&doc, "display", "scale", 0.0..=9.0, 0.0), 1.0);
            assert_eq!(
                doc.get("keep"),
                Some(&json!(1)),
                "other sections still survive"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `no_temp_file_is_left_behind`.
        {
            let dir = tmpdir();
            let path = dir.join("settings.json");
            let mut vals = Map::new();
            vals.insert("scale".into(), json!(1.0));
            update_section_at(&path, "display", vals).unwrap();
            let leftovers: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(std::result::Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    Path::new(n)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"))
                })
                .collect();
            assert!(leftovers.is_empty(), "atomic write left {leftovers:?}");
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod parent_durability_tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    use tilo_files::native::atomic::test_support::{
        ProvisionCheckpoint, with_provision_failure, with_provision_observer,
    };
    use tilo_files::native::atomic::{DirectoryState, directory_state};

    #[test]
    fn existing_copy_parent_is_not_reprovisioned_before_locking() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        let visits = Rc::new(RefCell::new(Vec::new()));
        let observed = visits.clone();
        let guard = with_provision_observer(
            move |event| observed.borrow_mut().push(event.path.clone()),
            || session::lock_copy(&path),
        )
        .unwrap();
        assert!(visits.borrow().is_empty());
        assert!(session::lock_path(&path).is_file());
        assert!(!path.exists(), "locking must not publish settings");
        drop(guard);
    }

    #[test]
    fn public_update_preserves_file_publication_phase_and_visible_format() {
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
        use tilo_files::native::atomic::{PublicationState, publication_state};
        for (point, state, visible) in [
            (
                Checkpoint::BeforePublish,
                PublicationState::NotPublished,
                false,
            ),
            (
                Checkpoint::AfterPublish,
                PublicationState::PublicationUnknown,
                true,
            ),
            (
                Checkpoint::BeforeCommit,
                PublicationState::PublishedDurabilityUnknown,
                true,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("settings.json");
            std::fs::write(&path, b"{\n  \"previous\": true\n}\n").unwrap();
            let values = serde_json::from_str::<Map<String, Value>>(r#"{"theme":"dark"}"#).unwrap();
            let error =
                with_failure(point, || update_section_at(&path, "display", values)).unwrap_err();
            assert_eq!(publication_state(&error), Some(state));
            let bytes = std::fs::read_to_string(&path).unwrap();
            assert!(bytes.ends_with('\n'));
            assert!(bytes.starts_with("{\n  "));
            assert_eq!(load_at(&path).get("display").is_some(), visible);
            assert_eq!(load_at(&path)["previous"], true);
        }
    }

    #[test]
    fn public_update_propagates_parent_failure_and_preserves_pretty_json_format() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let profile = root.join("profile");
        let path = profile.join("config/settings.json");
        assert!(!profile.exists(), "the update must create its parent chain");
        let values = || serde_json::from_str::<Map<String, Value>>(r#"{"theme":"dark"}"#).unwrap();
        let error = with_provision_failure(ProvisionCheckpoint::BeforeSync, &profile, || {
            update_section_at(&path, "display", values())
        })
        .unwrap_err();
        assert_eq!(
            directory_state(&error),
            Some(DirectoryState::CreatedDurabilityUnknown)
        );
        assert!(profile.join("config").is_dir());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_dir(profile.join("config")).unwrap().count(),
            0
        );
        drop(error);
        let visits = Rc::new(RefCell::new(Vec::new()));
        let observed = visits.clone();
        with_provision_observer(
            move |event| {
                if event.checkpoint == ProvisionCheckpoint::Synced {
                    observed.borrow_mut().push(event.path.clone());
                }
            },
            || update_section_at(&path, "display", values()),
        )
        .unwrap();
        assert_eq!(
            *visits.borrow(),
            path.parent()
                .unwrap()
                .ancestors()
                .map(Path::to_owned)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"display\": {\n    \"theme\": \"dark\"\n  }\n}\n"
        );
        assert_eq!(load_at(&path)["display"]["theme"], "dark");
    }
}
