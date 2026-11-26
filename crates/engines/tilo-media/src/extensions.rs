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
