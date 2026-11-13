//! Content descriptors and image signatures, independent of storage and
//! transport.

mod document;
mod sniff;
pub use document::{DocumentFormat, RECORDING, RECORDING_MAGIC, document_format, opened_by};
pub use sniff::{ImageFormat, image_format, image_size, sniff};

/// Whether `hash` has the shape of a content address: 64 lower- or upper-case
/// hex digits. Addresses arrive from documents and peers, so anything else is
/// refused before it can name a path.
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The file extension and media type content is taken as, from its bytes: a
/// picture or video by signature, a document by format, text as text, anything
/// else opaque. Content travels without the name its sender gave it, so the
/// bytes alone say which app's file it is.
#[must_use]
pub fn opened_as(bytes: &[u8]) -> (&'static str, &'static str) {
    if let Some(image) = image_format(bytes) {
        return (image.extension, image.mime);
    }
    if bytes.get(4..8).is_some_and(|header| header == b"ftyp") {
        return ("mp4", "video/mp4");
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return ("webm", "video/webm");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12).is_some_and(|header| header == b"AVI ") {
        return ("avi", "video/x-msvideo");
    }
    if let Some(format) = document_format(bytes) {
        return (format.extension, format.mime);
    }
    if is_text(bytes) {
        return ("txt", "text/plain");
    }
    ("bin", "application/octet-stream")
}

/// Whether `bytes` read as text: UTF-8, a leading byte-order mark allowed, with
/// no control characters but tabs and line breaks. Empty content is not text.
fn is_text(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    !bytes.is_empty()
        && std::str::from_utf8(bytes).is_ok_and(|text| {
            text.chars()
                .all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        })
}

/// What a document records about content instead of its bytes, so placeholders
/// reserve the eventual dimensions before those bytes arrive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentMeta {
    /// Media type, sniffed from the content (`image/png`, …). Empty if unknown.
    pub mime: String,
    pub len: u64,
    /// Pixel width; 0 when no header can be read.
    pub w: u32,
    /// Pixel height; 0 when no header can be read.
    pub h: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opened_content_is_taken_as_what_its_bytes_say() {
        assert_eq!(opened_as(b"\x89PNG\r\n\x1a\n...."), ("png", "image/png"));
        assert_eq!(opened_as(b"\0\0\0\x18ftypmp42"), ("mp4", "video/mp4"));
        assert_eq!(
            opened_as(&[0x1A, 0x45, 0xDF, 0xA3, 0]),
            ("webm", "video/webm")
        );
        assert_eq!(
            opened_as(RECORDING_MAGIC),
            (RECORDING.extension, RECORDING.mime)
        );
        assert_eq!(opened_as(&[0, 1, 2]), ("bin", "application/octet-stream"));
        assert_eq!(
            opened_as("Application from Ada\n\tCV attached, ünïcode too.\r\n".as_bytes()),
            ("txt", "text/plain")
        );
        assert_eq!(opened_as(b"\xef\xbb\xbfwith a BOM"), ("txt", "text/plain"));
        assert_eq!(
            opened_as(b"half text\x00 then a NUL"),
            ("bin", "application/octet-stream")
        );
        assert_eq!(
            opened_as(b"\xff\xfe not UTF-8"),
            ("bin", "application/octet-stream")
        );
        assert_eq!(opened_as(b""), ("bin", "application/octet-stream"));
    }
}
