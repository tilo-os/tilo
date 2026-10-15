//! What a file says, as text, and text written as a file.
//!
//! Reading a file to quote it, summarise it or mail what it says, and
//! writing what was written into a new file: a PDF reads as the text of its
//! pages, anything else that is text reads as itself, and text is written
//! as given. A document format (`.docx`, `.odt`, `.rtf`, `.epub`) is neither
//! read nor written here: Writer links the document codecs and reads,
//! writes and converts those files (its `read-file` and `convert-file`), so
//! Files stays free of them.

use std::fmt::Write as _;
use std::path::Path;

/// The largest file read.
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Document formats Writer reads and writes; Files names it for them.
const DOCUMENT_FORMATS: &[&str] = &["docx", "odt", "rtf", "epub"];

/// What a file says, as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileText {
    /// `markdown`, `text`, or `pdf` (the text of its pages, each opened by a
    /// `[page N]` line).
    pub format: &'static str,
    pub text: String,
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Whether `ext` names a document format only Writer reads and writes.
fn is_document(ext: &str) -> bool {
    DOCUMENT_FORMATS.contains(&ext)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if metadata.is_dir() {
        return Err(format!("{} is a folder, not a file", path.display()));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} is larger than {} MiB",
            path.display(),
            MAX_FILE_BYTES / (1024 * 1024)
        ));
    }
    std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// The text of the PDF `bytes`, page by page.
fn pdf_text(bytes: &[u8]) -> Result<String, String> {
    // The portable engine is memory-safe and runs in process; nothing here
    // needs what only the sandboxed one draws.
    let document = tilo_pdf::Engine::portable()
        .open_bytes(bytes)
        .map_err(|error| format!("the PDF cannot be read: {error}"))?;
    let mut text = String::new();
    for page in 0..document.page_count() {
        let layer = document
            .text(page)
            .map_err(|error| format!("page {} cannot be read: {error}", page + 1))?;
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        let _ = writeln!(text, "[page {}]", page + 1);
        text.push_str(layer.text().trim_end());
    }
    Ok(text)
}

/// What the file at `path` says.
///
/// # Errors
///
/// The file cannot be read, is too large, is a folder, is a document only
/// Writer reads, or is binary data that does not read as text.
pub fn read_text(path: &Path) -> Result<FileText, String> {
    let ext = extension(path);
    if is_document(&ext) {
        return Err(format!(
            "{} is a .{ext} document: Writer's `read-file` reads it as Markdown",
            path.display()
        ));
    }
    let bytes = read_bounded(path)?;
    if ext == "pdf" {
        return pdf_text(&bytes).map(|text| FileText {
            format: "pdf",
            text,
        });
    }
    let format = if matches!(ext.as_str(), "md" | "markdown") {
        "markdown"
    } else {
        "text"
    };
    String::from_utf8(bytes)
        .map(|text| FileText { format, text })
        .map_err(|_| format!("{} cannot be read as text: it is not text", path.display()))
}

/// Whether Files writes a file called `name`: any text file, but not a PDF
/// or a document format, which Writer makes.
///
/// # Errors
///
/// The name is a PDF or a document format: write the text as Markdown and
/// convert that with Writer's `convert-file`.
pub fn writable(name: &str) -> Result<(), String> {
    let ext = extension(Path::new(name));
    if ext == "pdf" || is_document(&ext) {
        return Err(format!(
            "Files writes text; write {name} as Markdown and turn it into .{ext} with \
             Writer's `convert-file`"
        ));
    }
    Ok(())
}

/// The bytes of a file named `name` that holds `text`: the text as given.
///
/// # Errors
///
/// As [`writable`].
pub fn bytes_for(name: &str, text: &str) -> Result<Vec<u8>, String> {
    writable(name)?;
    Ok(text.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_written_as_given_and_reads_back() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Summary.md");
        std::fs::write(
            &path,
            bytes_for("Summary.md", "# Findings\n\nAll good.").unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_text(&path).unwrap(),
            FileText {
                format: "markdown",
                text: "# Findings\n\nAll good.".into()
            }
        );

        let notes = root.path().join("notes.txt");
        std::fs::write(&notes, "plain *text*\nsecond line\n").unwrap();
        assert_eq!(
            read_text(&notes).unwrap(),
            FileText {
                format: "text",
                text: "plain *text*\nsecond line\n".into()
            }
        );
        assert_eq!(bytes_for("data.csv", "a,b\n1,2\n").unwrap(), b"a,b\n1,2\n");
    }

    #[test]
    fn documents_are_left_to_writer_with_a_reason() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            bytes_for("Letter.docx", "# Dear reader")
                .unwrap_err()
                .contains("convert-file")
        );
        assert!(bytes_for("Letter.pdf", "# Dear reader").is_err());
        let document = root.path().join("Letter.odt");
        std::fs::write(&document, b"PK").unwrap();
        assert!(read_text(&document).unwrap_err().contains("read-file"));
    }

    #[test]
    fn binary_files_and_folders_are_refused_with_a_reason() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("image.bin");
        std::fs::write(&binary, [0xff_u8, 0x00, 0xfe]).unwrap();
        assert!(
            read_text(&binary)
                .unwrap_err()
                .contains("cannot be read as text")
        );
        assert!(read_text(root.path()).unwrap_err().contains("is a folder"));
    }
}
