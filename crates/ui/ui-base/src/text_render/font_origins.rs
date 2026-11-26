//! The font files the text renderer loaded from disk, and whether a render service draws this
//! process's frames. A render service maps the same file from the shared read-only view instead of
//! receiving its bytes, so it needs to know which file each shaped font came from.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_lock::MutexRecover as _;
use vello::peniko::FontData;

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Frames go to a render service from here on.
#[cfg(target_os = "linux")]
pub(crate) fn mark_active() {
    ACTIVE.store(true, Ordering::Release);
}

/// Whether frames go to a render service, cheaply, for code that records what the service will need
/// (font origins).
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Fonts the text renderer loaded from a file, by blob.
fn font_files() -> &'static Mutex<HashMap<u64, PathBuf>> {
    static FILES: OnceLock<Mutex<HashMap<u64, PathBuf>>> = OnceLock::new();
    FILES.get_or_init(Mutex::default)
}

/// `font` was loaded from `path`.
pub(crate) fn note_font_file(font: &FontData, path: &Path) {
    font_files()
        .lock_recover()
        .entry(font.data.id())
        .or_insert_with(|| path.to_path_buf());
}

/// The file `font` was loaded from, when the text renderer noted it.
#[cfg(target_os = "linux")]
pub(crate) fn font_file(font: &FontData) -> Option<PathBuf> {
    font_files().lock_recover().get(&font.data.id()).cloned()
}
