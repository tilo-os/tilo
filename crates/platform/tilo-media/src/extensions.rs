//! Keep the `GStreamer` registry in step with merged system extensions.
//!
//! The optional additional-codecs pack arrives as a `systemd-sysext` image, its
//! plugins and libraries appearing under `/usr` exactly while its `extension-release`
//! file is there. [`refresh_registry`] reads that one small directory before anything
//! consults the registry, and does nothing more unless it changed.
//!
//! When it changed, an ordinary registry update is not enough: the registry keeps a
//! plugin's feature list for as long as the plugin *file* keeps its modification time
//! and size, and the pack replaces libraries under unchanged plugin files (a
//! freeworld `libavcodec` beneath the stock `libgstlibav.so`, whose element list
//! depends on it). So a changed stamp drops every plugin this process has not loaded
//! and rescans from the files on disk. The stamp is also persisted per user, so a
//! process started after the merge rescans once too.
//!
//! A plugin already loaded keeps the elements it registered and the libraries it
//! linked: new hardware entry points of a loaded VA-API plugin apply to processes
//! started afterwards, while new plugins are usable at once.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use gstreamer as gst;

pub const EXTENSION_RELEASE_DIR: &str = "/usr/lib/extension-release.d";

/// The merged extensions: file name and modification time (nanoseconds since
/// the epoch, 0 when unknown), sorted by name.
pub type ExtensionStamp = Vec<(OsString, u128)>;

pub fn extension_stamp(dir: &Path) -> ExtensionStamp {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: ExtensionStamp = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (entry.file_name(), modified)
        })
        .collect();
    out.sort();
    out
}

pub fn encode_stamp(stamp: &ExtensionStamp) -> String {
    let mut out = String::new();
    for (name, modified) in stamp {
        out.push_str(&name.to_string_lossy());
        out.push('\t');
        out.push_str(&modified.to_string());
        out.push('\n');
    }
    out
}

/// Parse [`encode_stamp`]'s output. `None` for anything malformed, which the
/// caller treats as "never recorded".
pub fn decode_stamp(text: &str) -> Option<ExtensionStamp> {
    let mut out = ExtensionStamp::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let (name, modified) = line.rsplit_once('\t')?;
        out.push((OsString::from(name), modified.parse().ok()?));
    }
    out.sort();
    Some(out)
}

/// Whether the registry must be rebuilt from the plugin files.
///
/// * Within a process, compare with what this process last saw.
/// * On a process's first look, compare with what the user's processes last
///   recorded, because that is what the on-disk registry cache reflects.
/// * With nothing recorded at all, rebuild only if an extension is merged:
///   the cache may predate it.
pub fn needs_rescan(
    process_last: Option<&ExtensionStamp>,
    persisted: Option<&ExtensionStamp>,
    current: &ExtensionStamp,
) -> bool {
    match (process_last, persisted) {
        (Some(last), _) => last != current,
        (None, Some(recorded)) => recorded != current,
        (None, None) => !current.is_empty(),
    }
}

pub fn default_stamp_path() -> Option<PathBuf> {
    let cache = tilo_paths::xdg::cache_home()?;
    Some(cache.join("tilo").join("media-extensions.stamp"))
}

/// What this process last saw. Held across the rescan so concurrent callers
/// wait for one rebuild instead of racing their own.
static PROCESS_LAST: Mutex<Option<ExtensionStamp>> = Mutex::new(None);

pub fn refresh_registry() -> bool {
    refresh_registry_at(
        Path::new(EXTENSION_RELEASE_DIR),
        default_stamp_path().as_deref(),
    )
}

/// [`refresh_registry`] against an explicit extension directory and stamp
/// file (`None`: do not persist).
pub fn refresh_registry_at(dir: &Path, stamp_path: Option<&Path>) -> bool {
    let current = extension_stamp(dir);
    let Ok(mut last) = PROCESS_LAST.lock() else {
        return false;
    };
    let persisted = if last.is_none() {
        stamp_path
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| decode_stamp(&text))
    } else {
        None
    };
    let rescan = needs_rescan(last.as_ref(), persisted.as_ref(), &current);
    if rescan {
        rebuild_registry();
    }
    let record = rescan || (last.is_none() && persisted.as_ref() != Some(&current));
    if record && let Some(path) = stamp_path {
        write_stamp(path, &current);
    }
    *last = Some(current);
    rescan
}

fn write_stamp(path: &Path, stamp: &ExtensionStamp) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    // A torn stamp only costs a rescan, so nothing is synced.
    let _ = tilo_files::native::atomic::write_regenerable(path, encode_stamp(stamp).as_bytes());
}

/// Drop every plugin this process has not loaded and rescan the plugin
/// paths, so each is registered again from the files now on disk. Loaded
/// plugins stay: their code is mapped and cannot be replaced in-process.
pub fn rebuild_registry() {
    if gst::init().is_err() {
        return;
    }
    let registry = gst::Registry::get();
    let mut dropped = 0usize;
    for plugin in registry.plugins() {
        if !plugin.is_loaded() && plugin.filename().is_some() {
            registry.remove_plugin(&plugin);
            dropped = dropped.saturating_add(1);
        }
    }
    let updated = gst::Registry::update().is_ok();
    tracing::info!(
        dropped,
        updated,
        "media: codec registry rebuilt after system extensions changed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(entries: &[(&str, u128)]) -> ExtensionStamp {
        entries
            .iter()
            .map(|(name, modified)| (OsString::from(name), *modified))
            .collect()
    }

    #[test]
    fn within_a_process_only_a_change_rescans() {
        let a = stamp(&[("extension-release.tilo-codecs", 1)]);
        let b = stamp(&[("extension-release.tilo-codecs", 2)]);
        assert!(
            !needs_rescan(Some(&a), Some(&b), &a),
            "persisted is ignored once seen"
        );
        assert!(needs_rescan(Some(&a), Some(&a), &b));
        assert!(needs_rescan(Some(&a), None, &Vec::new()), "removal rescans");
    }

    #[test]
    fn a_new_process_compares_with_what_the_cache_reflects() {
        let a = stamp(&[("extension-release.tilo-codecs", 1)]);
        assert!(!needs_rescan(None, Some(&a), &a));
        assert!(
            needs_rescan(None, Some(&Vec::new()), &a),
            "merged while no process ran"
        );
        assert!(
            needs_rescan(None, Some(&a), &Vec::new()),
            "unmerged while no process ran"
        );
    }

    #[test]
    fn with_nothing_recorded_only_a_merged_extension_rescans() {
        assert!(!needs_rescan(None, None, &Vec::new()));
        assert!(needs_rescan(
            None,
            None,
            &stamp(&[("extension-release.tilo-codecs", 1)])
        ));
    }

    #[test]
    fn stamp_round_trips_and_rejects_garbage() {
        let s = stamp(&[
            ("extension-release.tilo-codecs", 1_700_000_000_123_456_789),
            ("extension-release.tilo-codecs-mesa", 0),
        ]);
        assert_eq!(decode_stamp(&encode_stamp(&s)), Some(s));
        assert_eq!(decode_stamp(""), Some(Vec::new()));
        assert_eq!(decode_stamp("no-tab-here"), None);
        assert_eq!(decode_stamp("name\tnot-a-number"), None);
    }

    #[test]
    fn stamp_reads_names_and_times_sorted() {
        // Beside the test binary, not in a shared tmpfs.
        let root = std::env::current_exe()
            .unwrap()
            .with_file_name(format!("media-ext-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("extension-release.b"), "ID=fedora\n").unwrap();
        std::fs::write(root.join("extension-release.a"), "ID=fedora\n").unwrap();
        let read = extension_stamp(&root);
        let names: Vec<_> = read
            .iter()
            .map(|(n, _)| n.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["extension-release.a", "extension-release.b"]);
        assert!(read.iter().all(|(_, modified)| *modified > 0));
        assert!(extension_stamp(&root.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
