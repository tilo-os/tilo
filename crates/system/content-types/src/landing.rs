//! Where a file lands in a workspace: the app that opens its type and the
//! action that keeps it in a record there. An upload's bytes are only
//! content until an app takes them, and which app that is is decided by the
//! bytes, never by the name a sender gave them ([`crate::opened_as`]).

/// The app a file lands in when it is uploaded, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Landing {
    /// The app, as a resource path names it (`pdf-viewer`).
    pub app: &'static str,
    /// The action that puts the bytes in a record there.
    pub action: &'static str,
    /// Whether the file lands in a record of its own: the app opens one
    /// first, and its import fills it. An app that keeps many files in the
    /// record it already has says no.
    pub own_record: bool,
}

/// The most of a file read to tell what it is: every format here is known
/// from its first kilobyte.
pub const HEAD_BYTES: usize = 8 * 1024;

const PDF: Landing = Landing {
    app: "pdf-viewer",
    action: "import-pdf-content",
    own_record: true,
};
const WRITER: Landing = Landing {
    app: "word-processor",
    action: "import-docx-content",
    own_record: true,
};
const SHEETS: Landing = Landing {
    app: "sheets",
    action: "import-xlsx-content",
    own_record: true,
};
const SLIDES: Landing = Landing {
    app: "slides",
    action: "import-pptx-content",
    own_record: true,
};
const PHOTOS: Landing = Landing {
    app: "photos",
    action: "import-photo",
    own_record: false,
};
const NOTES: Landing = Landing {
    app: "notes",
    action: "import-markdown-content",
    own_record: false,
};

/// The app `name` lands in, for a file whose first bytes are `head`.
///
/// A word-processing document is landable as any of the formats the Writer
/// opens, and a spreadsheet only as a workbook; a text file is a note. The
/// name decides nothing but text against everything else: a picture, a PDF
/// or a workbook is what its bytes say, however it was called.
#[must_use]
pub fn landing(head: &[u8], name: &str) -> Option<Landing> {
    if crate::image_format(head).is_some() {
        return Some(PHOTOS);
    }
    if let Some(format) = crate::document_format(head) {
        let landing = match format.extension {
            "pdf" => PDF,
            "docx" | "odt" | "rtf" | "html" => WRITER,
            "xlsx" => SHEETS,
            "pptx" => SLIDES,
            _ => return None,
        };
        return Some(landing);
    }
    // Text is the one the name decides: a markdown or plain-text file is a
    // note, and a file nothing else lands is not made one by its name.
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let textual = matches!(
        extension.as_deref(),
        Some("md" | "markdown" | "txt" | "text")
    );
    (textual && crate::is_text(head)).then_some(NOTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_bytes_say_which_app_opens_it() {
        assert_eq!(landing(b"%PDF-1.7\n", "scan.pdf"), Some(PDF));
        assert_eq!(landing(b"%PDF-1.7\n", "notes.md"), Some(PDF));
        assert_eq!(landing(b"\x89PNG\r\n\x1a\n....", "scan.pdf"), Some(PHOTOS));
        assert_eq!(landing(b"# Plan\n\n- one\n", "plan.md"), Some(NOTES));
        assert_eq!(landing(b"plain words\n", "todo.txt"), Some(NOTES));
        assert_eq!(landing(b"plain words\n", "README"), None);
        assert_eq!(landing(&[0_u8, 1, 2, 3], "blob.bin"), None);
        assert_eq!(landing(b"", "empty.pdf"), None);
    }
}
