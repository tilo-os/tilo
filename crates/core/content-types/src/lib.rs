//! Content descriptors and image signatures, independent of storage and
//! transport.

mod document;
mod landing;
mod sniff;
pub use document::{DocumentFormat, RECORDING, RECORDING_MAGIC, document_format, opened_by};
pub use landing::{HEAD_BYTES, Landing, Takes, landing, landing_in};
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

/// The extension of the kind of file `name` is, whose first bytes are `head`: what an app's
/// manifest claims to open, so the apps that declare it decide which one opens the file.
///
/// The bytes decide what a file is (a picture called `.txt` is a picture, a workbook called
/// `report.md` a workbook), and text is what its name's extension says it is: bytes alone cannot
/// tell a note from a table. Empty content is what its name says too, so a new text file opens as
/// one. `None` when neither names a kind, as none opens it.
#[must_use]
pub fn opened_extension(head: &[u8], name: &str) -> Option<String> {
    if !head.is_empty() {
        let (extension, _) = opened_as(head);
        match extension {
            // Opaque bytes: no app opens what nothing here names.
            "bin" => return None,
            // Text: the name decides which kind of text it is.
            "txt" => {}
            known => return Some(known.to_owned()),
        }
    }
    let extension = std::path::Path::new(name)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    (!extension.is_empty()).then_some(extension)
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

/// The most a file a person hands an app may be: chosen in a chooser or dropped on a window, it
/// is what a `File` value reads. A picture a document keeps is smaller than this, and a record's
/// content is not what a file is for.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;

/// The media type of a file called `name`, as far as its extension tells: what a chooser starts
/// from, what a dropped file is taken as, and what a saved file is written as.
/// `application/octet-stream` when it says nothing.
#[must_use]
pub fn media_of(name: &str) -> String {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let media = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "md" | "markdown" => "text/markdown",
        "txt" => "text/plain",
        "html" => "text/html",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    };
    media.to_owned()
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

    #[test]
    fn the_kind_of_a_file_is_what_its_bytes_say_and_the_name_of_its_text() {
        assert_eq!(
            opened_extension(b"\x89PNG\r\n\x1a\n....", "shot.txt").as_deref(),
            Some("png"),
            "a picture called `.txt` is a picture"
        );
        assert_eq!(
            opened_extension(b"# Plan\n\n- one\n", "plan.md").as_deref(),
            Some("md"),
            "text is the kind the name says it is"
        );
        assert_eq!(
            opened_extension(b"plain words\n", "notes.TXT").as_deref(),
            Some("txt"),
            "and the name is read whatever case it is written in"
        );
        assert_eq!(
            opened_extension(b"%PDF-1.7\n", "plan.md").as_deref(),
            Some("pdf")
        );
        assert_eq!(
            opened_extension(b"", "New file.md").as_deref(),
            Some("md"),
            "an empty file is what its name says"
        );
        assert_eq!(opened_extension(b"", "README"), None);
        assert_eq!(
            opened_extension(&[0_u8, 1, 2, 3], "plan.md"),
            None,
            "opaque bytes are no kind the name makes one"
        );
    }
}
