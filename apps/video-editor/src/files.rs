//! Media, projects and renders by native path, through `tilo-files`.

#![cfg_attr(
    not(feature = "decode"),
    allow(dead_code, reason = "renders and thumbnails need the decode feature")
)]

use std::path::Path;

use tilo_files::{FsError, Meta, blocking};

fn at(path: &Path) -> Result<(tilo_files::Dir, tilo_files::VPath), FsError> {
    tilo_files::file_at(path)
}

pub(crate) fn metadata(path: &Path) -> Result<Meta, FsError> {
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

/// Whether chosen files have native paths here; without them (a browser
/// page) media is imported by its contents.
pub(crate) fn native_paths() -> bool {
    matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
}

/// The largest media file imported by contents.
pub(crate) const KEEP_LIMIT: usize = 512 * 1024 * 1024;

/// Keep media imported by contents in the editor's cache location (the
/// origin-private file system in a page) as `media/<token>`, as an app task.
/// Best effort: the project keeps the item either way.
pub(crate) fn keep_imported(token: String, bytes: Vec<u8>) {
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
