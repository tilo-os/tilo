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
//! another, silently. [`settings_path`] now resolves it once, following the spec.
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
        reason = "a test that unwraps is stating an invariant"
    )
)]

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub mod hardware;
pub mod notification_policy;

/// The file name under the qOS config root.
const FILE: &str = "settings.json";

/// The per-user qOS config root: `$XDG_CONFIG_HOME/qos`, else `$HOME/.config/qos`. `None`
/// only when neither variable yields a usable path. Per the XDG spec, `XDG_CONFIG_HOME` is
/// honoured only when set, non-empty **and absolute** — a relative value falls back to
/// `$HOME`, and getting that rule wrong in one reader and right in another is what made this
/// crate necessary.
pub fn config_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty() && p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
    Some(base.join("qos"))
}

/// The shared settings file: `<config root>/settings.json`.
pub fn settings_path() -> Option<PathBuf> {
    config_root().map(|r| r.join(FILE))
}

/// Read and parse the whole document. Returns an empty object for a missing, unreadable or
/// malformed file, so callers never distinguish "absent" from "broken" — both mean "use
/// defaults".
pub fn load() -> Map<String, Value> {
    match settings_path() {
        Some(p) => load_at(&p),
        None => Map::new(),
    }
}

/// [`load`] from an explicit path. Exposed so tests can drive a scratch file without
/// touching the real one.
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

/// Read one top-level section from the real settings file.
pub fn load_section(name: &str) -> Map<String, Value> {
    match load().get(name) {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// Merge `values` into the top-level section `name`, preserving every other section and
/// every key of `name` that `values` does not mention. This is the only supported write:
/// creates the config directory and file on first use, and writes through a temp file +
/// rename so a crash mid-write can't leave a half-written document — which would otherwise
/// reset every pane at once.
pub fn update_section(name: &str, values: Map<String, Value>) -> std::io::Result<()> {
    let Some(path) = settings_path() else {
        return Err(std::io::Error::other("no config directory available"));
    };
    update_section_at(&path, name, values)
}

/// [`update_section`] against an explicit path.
pub fn update_section_at(
    path: &Path,
    name: &str,
    values: Map<String, Value>,
) -> std::io::Result<()> {
    let mut doc = load_at(path);
    let entry = doc
        .entry(name.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    // A section on disk as the wrong type is replaced rather than merged into — there's nothing
    // to preserve in a value we cannot read.
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    if let Some(obj) = entry.as_object_mut() {
        for (k, v) in values {
            obj.insert(k, v);
        }
    }
    write_atomic(path, &Value::Object(doc))
}

/// Serialise and replace the document. `atomic_file` does the crash-safe part: a truncated
/// `settings.json` would reset every pane at once.
fn write_atomic(path: &Path, doc: &Value) -> std::io::Result<()> {
    let mut body = serde_json::to_string_pretty(doc).map_err(std::io::Error::other)?;
    body.push('\n');
    atomic_file::write_creating_parent(path, body.as_bytes())
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
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMPDIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn tmpdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "qos-settings-store-{}-{:?}-{}",
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
    #[serial_test::serial]
    fn an_absolute_xdg_config_home_wins() {
        let dir = tmpdir();
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        assert_eq!(config_root(), Some(dir.join("qos")));
        assert_eq!(settings_path(), Some(dir.join("qos").join("settings.json")));
        match prev {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[serial_test::serial]
    fn a_relative_xdg_config_home_is_ignored_not_joined() {
        // The live bug: hand-rolled resolvers joined a relative value verbatim, so `settings.json`
        // moved with the working directory while `dirs::config_dir()` readers stayed on
        // `$HOME/.config` — one file, two locations, silently.
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "relative/config") };

        let root = config_root().expect("falls back to $HOME/.config");
        assert!(
            root.is_absolute(),
            "a settings path must never be relative: {root:?}"
        );
        assert!(
            !root.starts_with("relative"),
            "a relative XDG_CONFIG_HOME must be ignored, got {root:?}"
        );
        let home = dirs::home_dir().expect("test needs a home dir");
        assert_eq!(root, home.join(".config").join("qos"));

        match prev {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }

    #[test]
    #[serial_test::serial]
    fn an_empty_xdg_config_home_is_ignored() {
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "") };
        let root = config_root().expect("falls back to $HOME/.config");
        assert_eq!(root, dirs::home_dir().unwrap().join(".config").join("qos"));
        match prev {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }

    // Forgiving reads

    #[test]
    fn broken_documents_read_as_no_settings() {
        for text in ["", "not json {", "[1,2,3]", "null", "\"a string\""] {
            assert!(parse(text).is_empty(), "{text:?} must read as no settings");
            assert!(section(text, "display").is_empty());
        }
    }

    #[test]
    fn a_missing_file_reads_as_no_settings() {
        let dir = tmpdir();
        assert!(load_at(&dir.join("absent.json")).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_section_of_the_wrong_type_reads_as_absent() {
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

    #[test]
    fn typed_readers_clamp_and_fall_back() {
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

    // Non-destructive writes

    #[test]
    fn writing_one_section_preserves_every_other() {
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

    #[test]
    fn writing_a_section_merges_into_it_rather_than_replacing_it() {
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

    #[test]
    fn writing_creates_the_directory_and_the_file() {
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

    #[test]
    fn writing_over_a_corrupt_file_does_not_propagate_the_corruption() {
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

    #[test]
    fn a_section_stored_as_the_wrong_type_is_replaced_on_write() {
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

    #[test]
    fn no_temp_file_is_left_behind() {
        let dir = tmpdir();
        let path = dir.join("settings.json");
        let mut vals = Map::new();
        vals.insert("scale".into(), json!(1.0));
        update_section_at(&path, "display", vals).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "atomic write left {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
