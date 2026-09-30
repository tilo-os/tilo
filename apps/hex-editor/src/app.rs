//! Root widget assembly for the hex editor binary.
//!
//! The editor surface is a single custom-drawn [`HexView`] that fills the
//! window; this module is the thin entry point that decides what buffer to
//! open onto. The live app with no file argument opens the honest empty state
//! ([`HexView::empty`]); the bundled sample buffer is reserved for `--shots`.

use std::path::Path;

use crate::sample;
use crate::view::HexView;

/// Construct the editor opened on the bundled sample buffer.
///
/// The cursor starts on the IHDR width field (offset `0x10`) with the
/// inspector in big-endian — PNG's native byte order — so the data
/// inspector immediately reads the image's `2 × 2` dimensions as clean
/// `u32` values rather than landing on the signature byte.
pub fn sample_view() -> HexView {
    HexView::new(sample::png_header(), "sample.png")
        .with_big_endian()
        .with_cursor(0x10)
}

/// The bytes of a file a chooser or the command line named.
pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>, tilo_files::FsError> {
    let (dir, name) = tilo_files::file_at(path)?;
    tilo_files::blocking::read(&dir, &name)
}

/// Construct the editor opened on a file's bytes. The display name is the
/// file's final path component, and the path is remembered so `Ctrl+S` writes
/// straight back. An unreadable path yields the honest empty state — never a
/// fabricated buffer.
pub fn view_for_path(path: &Path) -> HexView {
    match read_file(path) {
        Ok(bytes) => {
            let name = path.file_name().map_or_else(
                || "untitled".to_string(),
                |s| s.to_string_lossy().into_owned(),
            );
            HexView::new(bytes, name).with_path(path.to_path_buf())
        }
        Err(_) => HexView::empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_views_are_sample_png_or_honest_empty_state() {
        // Sample view: opens on the PNG header.
        let v = sample_view();
        assert_eq!(&v.model().bytes()[..4], &[0x89, 0x50, 0x4E, 0x47]);
        assert!(v.model().len() > 32);
        // Cursor parks on the IHDR width field, inspector in big-endian.
        assert_eq!(v.model().cursor(), 0x10);
        assert_eq!(v.model().endian(), crate::model::Endian::Big);

        // Missing path: the empty state, not a fake buffer.
        let v = view_for_path(Path::new("/nonexistent/definitely-not-here.bin"));
        assert!(
            !v.has_file(),
            "an unreadable path must not fabricate a buffer"
        );
        assert!(v.model().is_empty());
    }
}
