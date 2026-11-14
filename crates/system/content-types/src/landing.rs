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
    /// Its words: the file's name as `title`, its text as `body`.
    Text,
    /// A picture: `image`, with its name, its size and its reference.
    Image,
}

/// The landing of the app `app` (`pdf-viewer`, or its package id
/// `so.tilo.pdf-viewer`): which of its actions takes a file, and how.
#[must_use]
pub fn landing_in(app: &str) -> Option<Landing> {
    let short = app.rsplit('.').next().unwrap_or(app);
    [PDF, WRITER, SHEETS, SLIDES, PHOTOS, NOTES]
        .into_iter()
        .find(|landing| landing.app == short)
}

/// The most of a file read to tell what it is: every format here is known
/// from its first kilobyte.
pub const HEAD_BYTES: usize = 8 * 1024;

const PDF: Landing = Landing {
    app: "pdf-viewer",
    action: "import-pdf-content",
    own_record: true,
    takes: Takes::ContentNamed,
};
const WRITER: Landing = Landing {
    app: "word-processor",
    action: "import-docx-content",
    own_record: true,
    takes: Takes::ContentNamed,
};
const SHEETS: Landing = Landing {
    app: "sheets",
    action: "import-xlsx-content",
    own_record: true,
    takes: Takes::ContentTitled,
};
const SLIDES: Landing = Landing {
    app: "slides",
    action: "import-pptx-content",
    own_record: true,
    takes: Takes::Content,
};
const PHOTOS: Landing = Landing {
    app: "photos",
    action: "import-photo",
    own_record: false,
    takes: Takes::Image,
};
const NOTES: Landing = Landing {
    app: "notes",
    action: "import-markdown-content",
    own_record: false,
    takes: Takes::Text,
};

/// The app `name` lands in, for a file whose first bytes are `head`.
///
/// A word-processing document is landable as any of the formats the Writer
/// opens, and a spreadsheet only as a workbook; any text file is a note. The
/// name decides nothing: a picture, a PDF, a workbook or a text file is what
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
    // Text is a note whatever it is called: the bytes say it is text, which
    // is what a note holds, so a file no other app opens is still a record a
    // person and an agent can find (`config.json`, `deploy.sh`, a `README`).
    // Bytes that are not text land nowhere: no note can hold them.
    (!name.trim().is_empty() && crate::is_text(head)).then_some(NOTES)
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
        // Text no app owns is still a record a person can open, whatever it
        // is called.
        assert_eq!(landing(b"{\"port\": 8080}\n", "config.json"), Some(NOTES));
        assert_eq!(landing(b"#!/bin/sh\necho hi\n", "deploy.sh"), Some(NOTES));
        assert_eq!(landing(b"plain words\n", "README"), Some(NOTES));
        assert_eq!(landing(&[0_u8, 1, 2, 3], "blob.bin"), None);
        assert_eq!(landing(b"", "empty.pdf"), None);
    }

    #[test]
    fn an_app_names_its_landing_by_its_short_or_package_id() {
        assert_eq!(landing_in("so.tilo.pdf-viewer"), Some(PDF));
        assert_eq!(landing_in("slides"), Some(SLIDES));
        assert_eq!(landing_in("so.tilo.terminal"), None);
    }
}
