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

#[cfg(feature = "gst")]
pub(crate) fn remove(path: &Path) {
    if let Ok((dir, name)) = at(path) {
        let _ = blocking::remove(&dir, &name);
    }
}

/// The tokens of the media this device imported by contents: their bytes
/// are here (kept in the cache location), whatever a path lookup says.
static KEPT_HERE: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Whether a project media `path` names a copy this device imported by
/// contents.
pub(crate) fn kept_here(path: &str) -> bool {
    let Some(token) = path
        .strip_prefix(crate::media::KEPT_PREFIX)
        .and_then(|rest| rest.strip_prefix('/'))
    else {
        return false;
    };
    KEPT_HERE.lock().is_ok_and(|kept| kept.contains(token))
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
pub(crate) fn keep_imported(token: String, bytes: Vec<u8>) {
    if let Ok(mut kept) = KEPT_HERE.lock() {
        kept.insert(token.clone());
    }
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
