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
//! # Reading is always forgiving
//!
//! A missing file, unreadable file, malformed JSON, a section of the wrong type, or a value
//! out of range must never take an app or session down — every read path returns the
//! caller's default instead.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub mod continuity;
pub mod day_schedule;
pub mod display_identify;
pub mod display_mode;
pub mod display_tools;
pub mod hardware;
pub mod keyboard_shortcuts;
pub mod magnifier;
pub mod notification_policy;
pub mod roaming_provenance;
mod source;
pub mod system_sounds;
#[cfg(feature = "watch")]
pub mod watch;

pub use source::{HostDocument, SettingsSource, install_source, installed_source, merge_section};

/// The file name under the Tilo config root.
const FILE: &str = "settings.json";

/// The per-user Tilo config root: `$XDG_CONFIG_HOME/tilo`, else `$HOME/.config/tilo`. `None`
/// only when neither variable yields a usable path. Per the XDG spec, `XDG_CONFIG_HOME` is
/// honoured only when set, non-empty **and absolute** — a relative value falls back to
/// `$HOME`, and getting that rule wrong in one reader and right in another is what made this
/// crate necessary.
pub fn config_root() -> Option<PathBuf> {
    Some(tilo_paths::xdg::config_home()?.join("tilo"))
}

/// [`config_root`] from explicit values of `XDG_CONFIG_HOME` and `HOME` instead of the process
/// environment, applying the same rules.
pub fn config_root_from(
    xdg_config_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    Some(
        tilo_paths::xdg::Base::Config
            .resolve_from(xdg_config_home, home)?
            .join("tilo"),
    )
}

/// Resolve one settings file under the config root.
pub fn config_path(relative: &Path) -> Option<PathBuf> {
    Some(config_root()?.join(relative))
}

/// The shared settings file: `<config root>/settings.json`.
pub fn settings_path() -> Option<PathBuf> {
    config_path(Path::new(FILE))
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsStamp {
    /// The settings file's modification time.
    Modified(std::time::SystemTime),
    /// A host-installed source's [`SettingsSource::revision`].
    Revision(u64),
}

/// The current [`SettingsStamp`]: the installed source's revision, else the file's
/// modification time. `None` when there is no readable file.
pub fn stamp() -> Option<SettingsStamp> {
    if let Some(source) = installed_source() {
        return Some(SettingsStamp::Revision(source.revision()));
    }
    let modified = std::fs::metadata(settings_path()?).ok()?.modified().ok()?;
    Some(SettingsStamp::Modified(modified))
}

/// [`load`] from an explicit path. Exposed so tests can drive a scratch file without
/// touching the configured settings file.
pub fn load_at(path: &Path) -> Map<String, Value> {
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
/// every key of `name` that `values` does not mention. This is the only supported write:
/// creates the config directory and file on first use, and writes through a temp file +
/// rename so a crash mid-write can't leave a half-written document — which would otherwise
/// reset every pane at once.
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
    update_section_at(&path, name, values)
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

/// [`update_section`] against an explicit path.
///
/// # Errors
///
/// Returns an error if the document can't be serialised or written.
pub fn update_section_at(
    path: &Path,
    name: &str,
    values: Map<String, Value>,
) -> std::io::Result<()> {
    let mut doc = load_at(path);
    merge_section(&mut doc, name, values);
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
        assert!(!home.join(".config/tilo/settings.json").exists());

        // A test that names its own config directory still writes there.
        let config = home.join("own-config");
        env.set("XDG_CONFIG_HOME", &config);
        update_section("notifications", values).expect("an explicit config directory");
        assert!(config.join("tilo/settings.json").is_file());
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
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            TMPDIR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // Path resolution: the divergence this crate ends

    #[test]
    fn config_root_resolution_accepts_only_absolute_xdg_paths() {
        // Phase `an_absolute_xdg_config_home_wins`.
        {
            let dir = tmpdir();
            assert_eq!(
                config_root_from(Some(dir.as_os_str()), Some("/home/ada".as_ref())),
                Some(dir.join("tilo"))
            );
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `a_relative_xdg_config_home_is_ignored_not_joined`.
        {
            // The live bug: hand-rolled resolvers joined a relative value verbatim, so `settings.json`
            // moved with the working directory while `dirs::config_dir()` readers stayed on
            // `$HOME/.config` — one file, two locations, silently.
            let root =
                config_root_from(Some("relative/config".as_ref()), Some("/home/ada".as_ref()))
                    .expect("falls back to $HOME/.config");
            assert!(
                root.is_absolute(),
                "a settings path must never be relative: {root:?}"
            );
            assert_eq!(root, Path::new("/home/ada/.config/tilo"));
        }
        // Phase `an_empty_xdg_config_home_is_ignored`.
        {
            assert_eq!(
                config_root_from(Some("".as_ref()), Some("/home/ada".as_ref())),
                Some(PathBuf::from("/home/ada/.config/tilo"))
            );
            assert_eq!(
                config_root_from(None, None),
                None,
                "no usable variable and no home names no config root"
            );
        }
        // Phase `the_configured_root_applies_the_same_rules_to_the_process_environment`.
        {
            let home = tilo_paths::home();
            assert_eq!(
                config_root(),
                config_root_from(
                    std::env::var_os("XDG_CONFIG_HOME").as_deref(),
                    home.as_deref().map(Path::as_os_str),
                )
            );
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

#[cfg(all(test, unix))]
mod parent_durability_tests {
    use super::*;
    use atomic_file::test_support::{
        ProvisionCheckpoint, with_provision_failure, with_provision_observer,
    };
    use atomic_file::{DirectoryState, directory_state};
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn public_update_preserves_file_publication_phase_and_visible_format() {
        use atomic_file::test_support::{Checkpoint, with_failure};
        use atomic_file::{PublicationState, publication_state};
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
