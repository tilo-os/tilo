//! Root widget assembly for the hex editor binary.
//!
//! The editor surface is a single custom-drawn [`HexView`] that fills the
//! window; this module is the thin entry point that decides what buffer to
//! open onto. With no file argument it opens the bundled sample so the app
//! starts on real, recognisable bytes (a PNG header) instead of a blank
//! canvas.

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

/// Construct the editor opened on a file's bytes, falling back to the
/// bundled sample if the path can't be read. The display name is the
/// file's final path component.
pub fn view_for_path(path: &Path) -> HexView {
    match std::fs::read(path) {
        Ok(bytes) => {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".to_string());
            HexView::new(bytes, name)
        }
        Err(_) => sample_view(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_view_opens_on_png_header() {
        let v = sample_view();
        assert_eq!(&v.model().bytes()[..4], &[0x89, 0x50, 0x4E, 0x47]);
        assert!(v.model().len() > 32);
        // Cursor parks on the IHDR width field, inspector in big-endian.
        assert_eq!(v.model().cursor(), 0x10);
        assert_eq!(v.model().endian(), crate::model::Endian::Big);
    }

    #[test]
    fn view_for_missing_path_falls_back_to_sample() {
        let v = view_for_path(Path::new("/nonexistent/definitely-not-here.bin"));
        assert_eq!(&v.model().bytes()[..2], &[0x89, 0x50]);
    }
}
