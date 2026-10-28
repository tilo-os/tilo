//! Media, projects and renders by native path, through `tilo-files`.

use std::path::Path;

use tilo_files::{FsError, blocking};

fn at(path: &Path) -> Result<(tilo_files::Dir, tilo_files::VPath), FsError> {
    tilo_files::file_at(path)
}

#[cfg(any(feature = "decode", feature = "gst"))]
pub(crate) fn metadata(path: &Path) -> Result<tilo_files::Meta, FsError> {
    let (dir, name) = at(path)?;
    blocking::metadata(&dir, &name)
}

pub(crate) fn read(path: &Path) -> Result<Vec<u8>, FsError> {
    let (dir, name) = at(path)?;
    blocking::read(&dir, &name)
}

/// At most the first `max` bytes; only those are read.
pub(crate) fn read_prefix(path: &Path, max: u64) -> Result<Vec<u8>, FsError> {
    let (dir, name) = at(path)?;
    blocking::read_prefix(&dir, &name, usize::try_from(max).unwrap_or(usize::MAX))
}

/// Replace the file atomically, creating its folders.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<(), FsError> {
    let (dir, name) = at(path)?;
    blocking::write_atomic(&dir, &name, bytes)
}

/// Save a document the user chose, keeping an existing file's link and mode.
pub(crate) fn save(path: &Path, bytes: &[u8]) -> Result<(), FsError> {
    let (dir, name) = at(path)?;
    blocking::save_document(&dir, &name, bytes)
}

#[cfg(feature = "gst")]
pub(crate) fn create_dir_all(path: &Path) {
    if let Ok((dir, name)) = at(path) {
        let _ = blocking::create_dir_all(&dir, &name);
    }
}

pub(crate) fn remove(path: &Path) {
    if let Ok((dir, name)) = at(path) {
        let _ = blocking::remove(&dir, &name);
    }
}

/// The tokens of the media this device imported by contents: their bytes
/// are here (kept in the cache location), whatever a path lookup says.
static KEPT_HERE: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// The token of a media `path` that names a copy imported by contents.
fn kept_token(path: &str) -> Option<&str> {
    path.strip_prefix(crate::media::KEPT_PREFIX)
        .and_then(|rest| rest.strip_prefix('/'))
}

/// Whether a project media `path` names a copy imported by contents, on
/// this device or another.
#[cfg_attr(
    feature = "decode",
    expect(
        dead_code,
        reason = "only the seeking frame source of a build without GStreamer asks"
    )
)]
pub(crate) fn is_kept(path: &str) -> bool {
    kept_token(path).is_some()
}

/// Whether a project media `path` names a copy this device imported by
/// contents.
pub(crate) fn kept_here(path: &str) -> bool {
    let Some(token) = kept_token(path) else {
        return false;
    };
    KEPT_HERE.lock().is_ok_and(|kept| kept.contains(token))
}

/// Kept copies a decoder can open by their bytes in memory (a page's media
/// elements load them from a `blob:` URL), by token. Only a host whose files
/// have no paths makes these ([`media::edit::in_memory_source`]).
static KEPT_SOURCES: std::sync::Mutex<std::collections::BTreeMap<String, media::Source>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Kept copies being read back from the cache location.
static READING: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// The media type a decoder is told a kept copy named `name` holds.
fn media_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        _ => "video/mp4",
    }
}

fn remember_source(token: &str, bytes: &[u8], name: &str) -> bool {
    let Some(source) = media::edit::in_memory_source(bytes, media_type(name)) else {
        return false;
    };
    if let Ok(mut sources) = KEPT_SOURCES.lock() {
        sources.insert(token.to_string(), source);
    }
    true
}

/// The source a decoder opens for the kept copy `path` names, on a host that
/// opens sources from memory. `None` while it is not ready: a copy kept by
/// an earlier visit is read back from the cache location first, once, and
/// is here (and [`kept_here`]) when that finishes.
pub(crate) fn kept_source(path: &str) -> Option<media::Source> {
    let token = kept_token(path)?;
    if let Some(source) = KEPT_SOURCES.lock().ok()?.get(token) {
        return Some(source.clone());
    }
    if !READING.lock().ok()?.insert(token.to_string()) {
        return None;
    }
    let token = token.to_string();
    let Ok(sub) = tilo_files::VPath::new("media") else {
        return None;
    };
    let location = tilo_paths::AppPaths::for_app("so.tilo.video-editor")
        .cache()
        .join(&sub);
    drop(ui::rt::app().spawn_pinned(move || async move {
        let (Ok(dir), Ok(file)) = (
            tilo_files::Dir::open(location),
            tilo_files::VPath::new(&token),
        ) else {
            return;
        };
        let read = match dir.read(&file).await {
            Ok(bytes) => remember_source(&token, &bytes, ""),
            Err(_) => false,
        };
        if read && let Ok(mut kept) = KEPT_HERE.lock() {
            kept.insert(token);
        } else if let Ok(mut missing) = MISSING.lock() {
            missing.insert(token);
        }
    }));
    None
}

/// Kept copies that were looked for here and are not here.
static MISSING: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Whether the kept copy `path` names was looked for here and is not here,
/// so [`kept_source`] will never have it.
#[cfg_attr(
    feature = "decode",
    expect(
        dead_code,
        reason = "only the seeking frame source of a build without GStreamer asks"
    )
)]
pub(crate) fn kept_missing(path: &str) -> bool {
    kept_token(path).is_some_and(|token| MISSING.lock().is_ok_and(|m| m.contains(token)))
}

/// Record `token` as kept here without writing bytes, for tests that cannot
/// run the app task a real import spawns.
#[cfg(test)]
pub(crate) fn mark_kept_for_test(token: &str) {
    if let Ok(mut kept) = KEPT_HERE.lock() {
        kept.insert(token.to_string());
    }
}

/// The largest media file imported by contents.
pub(crate) const KEEP_LIMIT: usize = 512 * 1024 * 1024;

/// Keep media imported by contents in the editor's cache location (the
/// origin-private file system in a page) as `media/<token>`, as an app task.
/// Best effort: the project keeps the item either way.
pub(crate) fn keep_imported(token: String, name: &str, bytes: Vec<u8>) {
    if let Ok(mut kept) = KEPT_HERE.lock() {
        kept.insert(token.clone());
    }
    remember_source(&token, &bytes, name);
    let Ok(sub) = tilo_files::VPath::new("media") else {
        return;
    };
    let location = tilo_paths::AppPaths::for_app("so.tilo.video-editor")
        .cache()
        .join(&sub);
    drop(ui::rt::app().spawn_pinned(move || async move {
        if let (Ok(dir), Ok(path)) = (
            tilo_files::Dir::open(location),
            tilo_files::VPath::new(&token),
        ) {
            let _ = dir.write_atomic(&path, &bytes).await;
        }
    }));
}
