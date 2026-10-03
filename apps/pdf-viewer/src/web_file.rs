//! A PDF opened by its contents, where it has no path (a browser page),
//! remembered in the app's state location so the open document outlives a
//! reload.

use tilo_files::{Availability, Dir, Feature, VPath, blocking};

/// The largest PDF kept; a bigger one opens but is not reopened.
const KEEP_LIMIT: usize = 64 * 1024 * 1024;

/// Whether chosen files have native paths here; without them the viewer
/// opens by contents.
pub(crate) fn native_paths() -> bool {
    matches!(
        tilo_files::availability(Feature::NativePaths),
        Availability::Available
    )
}

/// A PDF the user opened by contents.
pub(crate) struct Remembered {
    pub name: String,
    pub bytes: Vec<u8>,
}

fn state() -> Option<Dir> {
    Dir::open(tilo_paths::AppPaths::for_app(APP).state()).ok()
}

fn path(file: &str) -> Option<VPath> {
    VPath::new(file).ok()
}

/// Keep `bytes` as the PDF this viewer has open. Best effort.
pub(crate) fn remember(name: &str, bytes: &[u8]) {
    if bytes.len() > KEEP_LIMIT {
        return;
    }
    let (Some(dir), Some(data), Some(label)) = (state(), path("opened.pdf"), path("opened.name"))
    else {
        return;
    };
    if blocking::write_atomic(&dir, &data, bytes).is_ok() {
        let _ = blocking::write_atomic(&dir, &label, name.as_bytes());
    }
}

/// The PDF [`remember`] kept, if any.
pub(crate) fn recall() -> Option<Remembered> {
    let dir = state()?;
    let name = String::from_utf8(blocking::read(&dir, &path("opened.name")?).ok()?).ok()?;
    let bytes = blocking::read(&dir, &path("opened.pdf")?).ok()?;
    Some(Remembered { name, bytes })
}

const APP: &str = "so.tilo.pdf-viewer";
