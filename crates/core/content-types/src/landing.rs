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
    pub own_resource: bool,
    /// What the action is handed of the file.
    pub takes: Takes,
}

/// What a landing's action takes of the file it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takes {
    /// Its reference as `content`, and its name as `name`.
    ContentNamed,
    /// Its reference as `content`, and its name as the record's `title`.
    ContentTitled,
    /// Its reference as `content` alone.
    Content,
    /// Its words: the file's name as `title`, its text as `markdown`.
    Text,
    /// A picture: `image`, with its name, its size and its reference.
    Image,
}

/// The landing of the app `app` (`pdf-viewer`, or its package id
/// `so.tilo.pdf-viewer`): which of its actions takes a file, and how. An app
/// that takes files of more than one kind (Sheets: a workbook, or a table as
/// text) answers its first; [`Landing::is_in`] asks whether a file's landing
/// is in it.
#[must_use]
pub fn landing_in(app: &str) -> Option<Landing> {
    let short = short_app(app);
    [PDF, WRITER, SHEETS, SLIDES, PHOTOS, NOTES]
        .into_iter()
        .find(|landing| landing.app == short)
}

/// `app` as a landing names it: `sheets` for `so.tilo.sheets`.
fn short_app(app: &str) -> &str {
    app.rsplit('.').next().unwrap_or(app)
}

impl Landing {
    /// Whether this landing is in the app `app` (`sheets`, or its package id
    /// `so.tilo.sheets`).
    #[must_use]
    pub fn is_in(&self, app: &str) -> bool {
        self.app == short_app(app)
    }
}

/// The most of a file read to tell what it is: every format here is known
/// from its first kilobyte.
pub const HEAD_BYTES: usize = 8 * 1024;

const PDF: Landing = Landing {
    app: "pdf-viewer",
    action: "open_source",
    own_resource: true,
    takes: Takes::ContentNamed,
};
const WRITER: Landing = Landing {
    app: "word-processor",
    action: "import-docx-content",
    own_resource: true,
    takes: Takes::ContentNamed,
};
const SHEETS: Landing = Landing {
    app: "sheets",
    action: "import-xlsx-content",
    own_resource: true,
    takes: Takes::ContentTitled,
};
/// A table kept as text (`.csv`, `.tsv`): its words are its cells.
const SHEETS_TABLE: Landing = Landing {
    app: "sheets",
    action: "import-csv-content",
    own_resource: true,
    takes: Takes::ContentTitled,
};
const SLIDES: Landing = Landing {
    app: "slides",
    action: "import-pptx-content",
    own_resource: true,
    takes: Takes::Content,
};
const PHOTOS: Landing = Landing {
    app: "photos",
    action: "import-photo",
    own_resource: false,
    takes: Takes::Image,
};
const NOTES: Landing = Landing {
    app: "notes",
    action: "create-page",
    own_resource: false,
    takes: Takes::Text,
};

/// The app `name` lands in, for a file whose first bytes are `head`.
///
/// A word-processing document is landable as any of the formats the Writer
/// opens, and a spreadsheet as a workbook; a text file is a note, and a
/// table kept as text (`.csv`, `.tsv`) a spreadsheet. The name decides
/// nothing but which text a file is: a picture, a PDF or a workbook is what
/// its bytes say, however it was called.
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
    // note, a table kept as text a spreadsheet, and a file nothing else
    // lands is not made one by its name. An empty file has no bytes to say
    // otherwise, so its name decides: a new text file opens as a note to be
    // written.
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let landing = match extension.as_deref() {
        Some("md" | "markdown" | "txt" | "text") => NOTES,
        Some("csv" | "tsv") => SHEETS_TABLE,
        _ => return None,
    };
    (head.is_empty() || crate::is_text(head)).then_some(landing)
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
        assert_eq!(landing(b"", "New file.txt"), Some(NOTES));
        assert_eq!(landing(b"", "empty"), None);
        // A table kept as text is a spreadsheet, by its name; bytes that are no text are not.
        assert_eq!(
            landing(b"name,amount\nTea,4\n", "data.csv"),
            Some(SHEETS_TABLE)
        );
        assert_eq!(landing(b"name\tamount\n", "DATA.TSV"), Some(SHEETS_TABLE));
        assert_eq!(landing(&[0_u8, 1, 2, 3], "data.csv"), None);
        assert!(SHEETS_TABLE.is_in("so.tilo.sheets"));
        assert!(SHEETS.is_in("sheets"));
        assert!(!SHEETS_TABLE.is_in("so.tilo.notes"));
    }

    #[test]
    fn an_app_names_its_landing_by_its_short_or_package_id() {
        assert_eq!(landing_in("so.tilo.pdf-viewer"), Some(PDF));
        assert_eq!(landing_in("slides"), Some(SLIDES));
        assert_eq!(landing_in("so.tilo.terminal"), None);
    }
}
