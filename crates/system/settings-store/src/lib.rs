//! The person's desktop settings: one document of independent sections, and reads that never
//! fail.
//!
//! A profile's settings are one Automerge document ([`Store`]), the sections the person holds a
//! copy of their record ([`RoamingSettings`]). The process holding the profile writes it; every
//! other process reads the same document and
//! follows the holder. A read that finds a missing setting, a section of the wrong type or a value
//! out of range returns the caller's default instead of failing.

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
use std::sync::Arc;

use serde_json::{Map, Value};

pub mod accessibility;
pub mod app_scope;
pub mod appearance;
pub mod clipboard_history;
pub mod clock;
pub mod continuity_privacy;
pub mod control_hints;
pub mod day_schedule;
pub mod desktop_controls;
pub mod device_class;
pub mod device_vpn;
pub mod dictation;
pub mod display_control;
pub mod display_identify;
pub mod display_mode;
pub mod display_placement;
pub mod display_scale;
pub mod display_tools;
pub mod display_tuning;
pub mod hardware;
pub mod identity;
pub mod index;
mod key;
pub mod keyboard_shortcuts;
pub mod keyboard_sources;
#[cfg(feature = "watch")]
pub mod live;
pub mod lockdown;
pub mod magnifier;
pub mod mcp_server;
pub mod notification_policy;
pub mod paste_shortcut;
pub mod person_displays;
pub mod personal_dictionary;
pub mod placement;
pub mod power;
pub mod published;
pub mod relay;
pub mod search_history;
mod section_digest;
pub mod shell_views;
pub mod sign_in_comforts;
mod source;
mod store;
pub mod system_sounds;
#[cfg(feature = "watch")]
pub mod watch;
pub mod widgets;

pub use accessibility::Labelled;
pub use device_class::DeviceClass;
pub use key::{Key, SectionWrite};
pub use section_digest::SectionDigest;
pub use source::{
    ChangeOrigin, ChangeSignal, ConditionalUpdate, DeferredFailure, DisplayUpdate, HostDocument,
    InvokingAction, Listener, SectionChange, SettingsSource, SourceScope, deferred_failure_pending,
    flush_pending_writes, install_source, installed_source, installed_version, merge_section,
    note_deferred_failure, scoped_source, take_deferred_failures,
};
pub use store::{BOOKKEEPING, RoamingSettings, Store, device, made_by, profile};

/// The per-user Tilo config root: `$XDG_CONFIG_HOME/tilo`, else `$HOME/.config/tilo`. `None`
/// only when neither variable yields a usable path. Per the XDG spec, `XDG_CONFIG_HOME` is
/// honoured only when set, non-empty **and absolute** — a relative value falls back to
/// `$HOME`, and getting that rule wrong in one reader and right in another is what made this
/// crate necessary.
pub fn config_root() -> Option<PathBuf> {
    tilo_paths::profile_root()
}

pub fn config_path(relative: &Path) -> Option<PathBuf> {
    Some(config_root()?.join(relative))
}

/// This person's settings in this process: the installed source when the
/// host provides them (the session's own store, an app's channel to the
/// session, a page's host document), else this device's settings, held or
/// followed in the device document store ([`device`]). `None` only without a
/// data home, as in a browser, whose page always installs its source.
#[must_use]
pub fn source() -> Option<Arc<dyn SettingsSource>> {
    if let Some(source) = installed_source() {
        return Some(source);
    }
    if tilo_platform_caps::WEB {
        return None;
    }
    local().map(|store| store as Arc<dyn SettingsSource>)
}

/// This device's settings: the store this process holds or follows when the
/// host installed no source, kept in the device document store the session
/// and its apps' documents live in. A test harness started outside the test
/// environment never writes them ([`unisolated_test_write`]).
#[must_use]
pub fn local() -> Option<Arc<Store>> {
    let store = device()?;
    if let Some(reason) = executable().and_then(|exe| {
        unisolated_test_write(
            exe,
            std::env::var_os("TILO_TEST_ENVIRONMENT").is_some(),
            std::env::var_os("XDG_DATA_HOME").is_some(),
        )
    }) {
        store.refuse_writes(reason);
    }
    Some(store)
}

/// This process's own executable, resolved the first time it is asked for.
///
/// [`std::env::current_exe`] reads `/proc/self/exe`, a path lookup on the
/// thread that asks; the answer is fixed for the life of the process, and
/// [`local`] is asked for a stamp on every frame of a session's loop.
fn executable() -> Option<&'static Path> {
    static EXECUTABLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    EXECUTABLE
        .get_or_init(|| std::env::current_exe().ok())
        .as_deref()
}

/// The whole document. Never fails: without settings it is empty, so callers
/// never distinguish "absent" from "broken" — both mean "use defaults".
pub fn load() -> Map<String, Value> {
    source().map(|source| source.load()).unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SettingsStamp(pub u64);

/// Whether a poller that last observed `cached` must read the document again.
///
/// The first observation (`seen == false`) loads even when both stamps are
/// `None`. Later polls skip the read while the stamp holds.
pub fn document_changed(
    seen: bool,
    cached: Option<SettingsStamp>,
    current: Option<SettingsStamp>,
) -> bool {
    !seen || cached != current
}

/// What a follower of the settings document keeps between passes: whether it
/// has looked yet and the [`SettingsStamp`] it last saw.
///
/// The first check reads even when no settings store is present (`None`); later
/// checks skip the read while the stamp holds, including staying `None`, until
/// a stamp appears or changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsFollower {
    seen: bool,
    stamp: Option<SettingsStamp>,
}

impl SettingsFollower {
    /// Whether the settings document must be read again at `current`, recording
    /// `current` as what this follower has now seen.
    pub fn changed(&mut self, current: Option<SettingsStamp>) -> bool {
        let changed = document_changed(self.seen, self.stamp, current);
        self.seen = true;
        self.stamp = current;
        changed
    }
}

pub fn stamp() -> Option<SettingsStamp> {
    source().map(|source| SettingsStamp(source.revision()))
}

pub fn parse(text: &str) -> Map<String, Value> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Read one top-level section of a settings document body as an object. A missing section,
/// or one that is not an object, yields an empty map.
pub fn section(text: &str, name: &str) -> Map<String, Value> {
    object_section(parse(text), name)
}

pub fn load_section(name: &str) -> Map<String, Value> {
    object_section(load(), name)
}

/// Sections `names` of this process's settings read as one: the leaves of
/// each, a later section's over an earlier's. A setting whose leaves the
/// person and the device hold apart (`input` and `input_device`) reads
/// through it.
pub fn load_sections(names: &[&str]) -> Map<String, Value> {
    merged_sections(&load(), names)
}

pub fn merged_sections(document: &Map<String, Value>, names: &[&str]) -> Map<String, Value> {
    let mut merged = Map::new();
    for name in names {
        if let Some(Value::Object(leaves)) = document.get(*name) {
            merged.extend(
                leaves
                    .iter()
                    .map(|(leaf, value)| (leaf.clone(), value.clone())),
            );
        }
    }
    merged
}

pub fn load_section_in(source: &dyn SettingsSource, name: &str) -> Map<String, Value> {
    object_section(source.load(), name)
}

fn object_section(mut doc: Map<String, Value>, name: &str) -> Map<String, Value> {
    match doc.remove(name) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn no_settings() -> std::io::Error {
    std::io::Error::other("no config directory available")
}

/// Merge `values` into the top-level section `name`, preserving every other section and
/// every key of `name` that `values` does not mention.
///
/// This is the raw, untyped write every other write reduces to. Production code names a
/// setting through a [`Key`] (one leaf) or a [`SectionWrite`] (several leaves of one
/// section) instead, so a misspelt section or a mistyped value cannot compile.
/// `tools/policy/check_settings_writes.py` refuses a string-literal section name here
/// outside this crate; the callers left are the ones that move a section they own as a
/// whole: the framework's `ui::storage` scopes and app settings channel, the device
/// fabric's roamed write-back, the Settings app's multi-section document writers and
/// schema owners that name their section through their own constant.
pub fn update_section(name: &str, values: Map<String, Value>) -> std::io::Result<()> {
    source()
        .ok_or_else(no_settings)?
        .update_section(name, values)
}

pub fn update_section_by(
    name: &str,
    values: Map<String, Value>,
    origin: &ChangeOrigin,
) -> std::io::Result<()> {
    source()
        .ok_or_else(no_settings)?
        .update_section_by(name, values, origin)
}

/// Read section `name` and merge what `change` makes of it, as one step no other write
/// lands inside. `change` is called once. This is the read-modify-write behind
/// [`Key::update`].
///
/// # Errors
///
/// As [`update_section`].
pub fn update_section_with(
    name: &str,
    origin: &ChangeOrigin,
    change: &mut SectionChange<'_>,
) -> std::io::Result<()> {
    source()
        .ok_or_else(no_settings)?
        .update_section_with(name, origin, change)
}

/// Why a write to this device's settings would land in the settings of whoever started this
/// test harness, or `None`.
///
/// Cargo starts test binaries inside `tools/with-test-environment.sh` (a fresh HOME). A
/// harness started by hand — `target/debug/deps/<crate>-<hash>` — keeps the caller's HOME,
/// and a test that saves a setting would rewrite their real settings, as fixture
/// values like a Do Not Disturb switch or a manual status once did. A test that points
/// `XDG_DATA_HOME` at its own directory still writes there, since the settings documents live
/// in that document store.
fn unisolated_test_write(
    exe: &Path,
    in_test_environment: bool,
    data_home_set: bool,
) -> Option<&'static str> {
    let harness = exe
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "deps");
    (harness && !in_test_environment && !data_home_set).then_some(
        "a test harness outside tools/with-test-environment.sh would write the caller's own \
         settings; run it through cargo or that script",
    )
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

pub fn str_at<'a>(doc: &'a Map<String, Value>, section: &str, key: &str) -> Option<&'a str> {
    doc.get(section)
        .and_then(|s| s.get(key))
        .and_then(|v| v.as_str())
}

pub fn bool_at(doc: &Map<String, Value>, section: &str, key: &str) -> Option<bool> {
    doc.get(section)
        .and_then(|s| s.get(key))
        .and_then(serde_json::Value::as_bool)
}

/// A test's scratch directory, removed with it. It is in memory where the
/// system has one ([`tilo_paths::memory_temp_dir`], `/dev/shm` on Linux), as
/// `tilo-app`'s test profiles are: these tests prove locking, merging and
/// recovery, and every write still syncs as it does for a person, but a sync
/// in memory costs nothing where on a shared disk it waits for every other
/// writer's data. Two writers flipping one key 100 times each took over 80 s
/// on a busy build node's disk. The parent-directory durability tests keep the
/// real disk ([`tempfile::tempdir`]).
#[cfg(test)]
pub(crate) fn test_dir() -> tempfile::TempDir {
    if let Some(memory) = tilo_paths::memory_temp_dir()
        && let Ok(directory) = tempfile::Builder::new()
            .prefix("settings-store-")
            .tempdir_in(memory)
    {
        return directory;
    }
    tempfile::tempdir().expect("a temporary directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMPDIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[cfg(all(target_os = "linux", feature = "watch"))]
    pub(crate) fn blocked(tid: &str) -> (u64, bool) {
        let status = std::fs::read_to_string(format!("/proc/self/task/{tid}/status")).unwrap();
        let field = |name: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .map(str::trim)
                .unwrap_or_default()
                .to_owned()
        };
        (
            field("voluntary_ctxt_switches:").parse().unwrap(),
            field("State:").starts_with('S'),
        )
    }

    #[cfg(all(target_os = "linux", feature = "watch"))]
    pub(crate) fn blocked_count(tid: &str) -> u64 {
        tilo_test_env::wait_for(format_args!("thread {tid} to block"), || {
            let (switches, blocked) = blocked(tid);
            blocked.then_some(switches)
        })
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

    /// A case that points the environment at a fresh directory gets the
    /// store under that directory, never the one a case before it opened:
    /// this device's store is kept per data home, resolved when it is asked
    /// for.
    #[test]
    fn a_new_data_home_answers_a_store_of_its_own() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", first.path().join("data"))
            .set("XDG_CONFIG_HOME", first.path().join("config"));
        let one = device().expect("a store under the first data home");
        env.set("XDG_DATA_HOME", second.path().join("data"))
            .set("XDG_CONFIG_HOME", second.path().join("config"));
        let two = device().expect("a store under the second data home");
        assert!(
            !Arc::ptr_eq(&one, &two),
            "the second case's settings are its own, not the first's"
        );
    }

    #[test]
    fn a_hand_started_test_harness_never_writes_the_callers_settings() {
        // This test binary is itself a harness under `deps/`: with the test environment's
        // marker and XDG_DATA_HOME gone, the settings are the fake caller's own, and the write
        // must be refused before anything lands there.
        let home = tmpdir();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &home)
            .remove("XDG_DATA_HOME")
            .remove("TILO_TEST_ENVIRONMENT");
        let mut values = Map::new();
        values.insert("do_not_disturb".into(), Value::Bool(true));
        let error = update_section("notifications", values.clone())
            .expect_err("a hand-started harness wrote the caller's settings");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            !home
                .join(".local/share/so.tilo.settings-device/state.automerge")
                .exists()
        );

        let data = home.join("own-data");
        env.set("XDG_DATA_HOME", &data);
        update_section("notifications", values).expect("an explicit data directory");
        assert_eq!(load_section("notifications")["do_not_disturb"], json!(true));
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

    tilo_test_env::scenario! {
        fn reads_forgive_missing_and_mistyped_sections;

        phase reads_forgive_missing_and_mistyped_sections;
        {
            let root = crate::test_dir();
            let store = profile(root.path());
            assert!(load_section_in(&*store, "display").is_empty());
            store.update_section("display", Map::new()).unwrap();
            assert!(parse("[1]").is_empty());
            assert!(parse("{").is_empty());
            let read = f64_in(&parse(r#"{"d":{"s":"x"}}"#), "d", "s", 0.0..=1.0, 0.5);
            assert!((read - 0.5).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn settings_follower_reads_once_without_a_stamp_and_only_when_it_moves() {
        let mut follower = SettingsFollower::default();
        assert!(
            follower.changed(None),
            "first look with no stamp must read once"
        );
        assert!(
            !follower.changed(None),
            "later look with still no stamp must not read again"
        );

        assert!(
            follower.changed(Some(SettingsStamp(1))),
            "a stamp appearing must read once"
        );
        assert!(
            !follower.changed(Some(SettingsStamp(1))),
            "the same stamp must not read again"
        );

        assert!(
            follower.changed(Some(SettingsStamp(2))),
            "a stamp that changed must read once"
        );
        assert!(
            !follower.changed(Some(SettingsStamp(2))),
            "the unchanged new stamp must not read again"
        );

        assert!(
            follower.changed(None),
            "a stamp disappearing must read once"
        );
        assert!(
            !follower.changed(None),
            "staying without a stamp again must not read again"
        );

        let mut started_stamped = SettingsFollower::default();
        assert!(
            started_stamped.changed(Some(SettingsStamp(9))),
            "first look with a stamp must read once"
        );
        assert!(
            !started_stamped.changed(Some(SettingsStamp(9))),
            "repeating that stamp must not read again"
        );
    }
}
