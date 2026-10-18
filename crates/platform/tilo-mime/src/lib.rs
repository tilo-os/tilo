//! A file's MIME type by its extension.
//!
//! The mapping is `mime_guess`'s (`tools/mime-table.py` generates
//! [`table`] from it), keeping each extension's first, most prevalent type.
//! `mime_guess` holds it as a table of string slices: about 1400 entries of
//! pointers that the loader relocates in every process mapping the shared
//! runtime, before `main`, whether the process ever asks for a type. This
//! table is integers into one string literal, which costs nothing until it
//! is read.

mod table;

use std::path::Path;

/// The type of content nothing more is known about.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// The MIME type of files ending in `.extension` (without the dot; any
/// case), or `None` for an extension with no known type.
#[must_use]
pub fn from_ext(extension: &str) -> Option<&'static str> {
    let wanted = extension.as_bytes();
    let found = table::EXTENSIONS
        .binary_search_by(|&(_, _, start, len)| {
            string(start, len)
                .as_bytes()
                .iter()
                .copied()
                .cmp(wanted.iter().map(u8::to_ascii_lowercase))
        })
        .ok()?;
    let &(start, len, _, _) = table::EXTENSIONS.get(found)?;
    Some(string(start, len))
}

/// The MIME type of `path` by its extension, or `None` when it has none or
/// one with no known type.
#[must_use]
pub fn from_path(path: impl AsRef<Path>) -> Option<&'static str> {
    from_ext(path.as_ref().extension()?.to_str()?)
}

/// [`from_path`], or [`OCTET_STREAM`].
#[must_use]
pub fn from_path_or_octet_stream(path: impl AsRef<Path>) -> &'static str {
    from_path(path).unwrap_or(OCTET_STREAM)
}

/// The string at `start..start + len` of the table's strings.
fn string(start: u16, len: u8) -> &'static str {
    let start = usize::from(start);
    table::STRINGS
        .get(start..start.saturating_add(usize::from(len)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_follow_extensions_in_any_case() {
        assert_eq!(from_ext("png"), Some("image/png"));
        assert_eq!(from_ext("PNG"), Some("image/png"));
        assert_eq!(from_ext("jpg"), Some("image/jpeg"));
        assert_eq!(from_ext("pdf"), Some("application/pdf"));
        assert_eq!(from_ext("7z"), Some("application/x-7z-compressed"));
        assert_eq!(from_ext("no-such-extension"), None);
        assert_eq!(from_ext(""), None);
        assert_eq!(from_path("Holiday/IMG_0001.JPG"), Some("image/jpeg"));
        assert_eq!(from_path("notes/README"), None);
        assert_eq!(
            from_path_or_octet_stream("archive.unknownext"),
            OCTET_STREAM
        );
        assert_eq!(from_path(".bashrc"), None, "a dotfile has no extension");
    }

    #[test]
    fn the_table_is_sorted_and_every_entry_reads_back() {
        let extensions: Vec<&str> = table::EXTENSIONS
            .iter()
            .map(|&(_, _, start, len)| string(start, len))
            .collect();
        assert!(extensions.windows(2).all(|pair| pair[0] < pair[1]));
        for &(mime, mime_len, extension, extension_len) in &table::EXTENSIONS {
            let extension = string(extension, extension_len);
            assert!(!extension.is_empty());
            assert_eq!(extension, extension.to_ascii_lowercase());
            let mime = string(mime, mime_len);
            assert!(mime.contains('/'), "{extension}: {mime}");
            assert_eq!(from_ext(extension), Some(mime));
        }
    }
}
