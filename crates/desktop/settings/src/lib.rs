//! The person's and the device's settings: independent sections, each a resource of the
//! workspace ([`workspace`]), read through typed keys that never fail.
//!
//! A process reads the settings its host installed ([`install_source`]): the session's own, an
//! app's channel to them, or a copy its host gives it. A read that finds a missing setting, a
//! section of the wrong type or a value out of range returns the caller's default instead of
//! failing.

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
pub mod appearance;
pub mod clipboard_history;
pub mod clock;
pub mod continuity_privacy;
pub mod control_hints;
pub mod day_schedule;
pub mod desktop_controls;
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
#[cfg(any(test, feature = "test-support"))]
pub mod in_memory;
pub mod index;
mod key;
pub mod keyboard_shortcuts;
pub mod keyboard_sources;
pub mod lockdown;
pub mod magnifier;
pub mod network;
pub mod notification_policy;
#[cfg(not(target_arch = "wasm32"))]
pub mod outside;
pub mod paste_shortcut;
pub mod person_displays;
pub mod personal_dictionary;
pub mod placement;
pub mod power;
pub mod published;
pub mod relay;
pub mod search_history;
pub mod shell_views;
pub mod sign_in_comforts;
mod source;
pub mod system_sounds;
#[cfg(feature = "watch")]
pub mod watch;
pub mod widgets;
pub mod workspace;

pub use accessibility::Labelled;
pub use key::Key;
pub use source::{
    ChangeOrigin, ChangeSignal, ConditionalUpdate, DeferredFailure, HostDocument, Listener,
    SectionChange, SettingsSource, SourceScope, compare_update_section, deferred_failure_pending,
    flush_pending_writes, install_source, installed_source, installed_version, merge_section,
    note_deferred_failure, scoped_source, take_deferred_failures,
};

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

/// The settings of this process: the source its host installed; in a native process that
/// installed none, what the profile's session left for processes outside it
/// ([`outside::Outside`]); `None` on a page that installed none, where every setting reads as its
/// default.
#[must_use]
pub fn source() -> Option<Arc<dyn SettingsSource>> {
    installed_source().or_else(outside_source)
}

#[cfg(not(target_arch = "wasm32"))]
fn outside_source() -> Option<Arc<dyn SettingsSource>> {
    static OUTSIDE: std::sync::Mutex<Vec<(PathBuf, Arc<outside::Outside>)>> =
        std::sync::Mutex::new(Vec::new());
    if tilo_platform_caps::WEB {
        return None;
    }
    let path = outside::path()?;
    let mut known = OUTSIDE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, held)) = known.iter().find(|(held, _)| *held == path) {
        return Some(Arc::clone(held) as Arc<dyn SettingsSource>);
    }
    let made = Arc::new(outside::Outside::of_this_profile());
    known.push((path, Arc::clone(&made)));
    drop(known);
    Some(made)
}

#[cfg(target_arch = "wasm32")]
const fn outside_source() -> Option<Arc<dyn SettingsSource>> {
    None
}

/// Why this process must not change the person's settings: a test harness started by hand
/// (`target/debug/deps/<crate>-<hash>`) outside the test environment keeps the caller's home, and
/// would change the settings of the caller's own session.
#[cfg(not(target_arch = "wasm32"))]
fn unisolated_test_write() -> Option<&'static str> {
    let harness = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.file_name()? == "deps"))
        .unwrap_or(false);
    (harness && std::env::var_os("TILO_TEST_ENVIRONMENT").is_none()).then_some(
        "a test harness outside tools/with-test-environment.sh would change the caller's own \
         settings; run it through cargo or that script",
    )
}

/// The settings of a profile kept apart from the person's (a headless app's, a test's, the
/// models a host keeps for a workspace it serves), by its root: one per root for the life of the
/// process, kept in the root's `settings.json`, which a write replaces whole.
#[must_use]
pub fn profile(root: &Path) -> Arc<HostDocument> {
    static PROFILES: std::sync::Mutex<Vec<(PathBuf, Arc<HostDocument>)>> =
        std::sync::Mutex::new(Vec::new());
    let mut profiles = PROFILES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, held)) = profiles.iter().find(|(held, _)| held == root) {
        return Arc::clone(held);
    }
    let file = root.join(PROFILE_FILE);
    let kept = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let document = Arc::new(HostDocument::new(kept, move |document| {
        let bytes = serde_json::to_vec(document).map_err(std::io::Error::other)?;
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let partial = file.with_extension("json.partial");
        std::fs::write(&partial, bytes)?;
        std::fs::rename(&partial, &file)
    }));
    profiles.push((root.to_owned(), Arc::clone(&document)));
    document
}

/// The file a [`profile`] is kept in, below its root.
const PROFILE_FILE: &str = "settings.json";

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
/// setting through a [`Key`] instead, so a misspelt section or a mistyped value cannot compile.
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

/// A test's scratch directory, removed with it, in memory where the system has one.
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
        test_dir().keep()
    }

    tilo_test_env::scenario! {
        fn reads_forgive_missing_and_mistyped_sections;

        phase reads_forgive_missing_and_mistyped_sections;
        {
            let store = profile(&tmpdir());
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
