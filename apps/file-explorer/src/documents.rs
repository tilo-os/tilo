//! What a file says as text, and text written as a file.

use std::fmt::Write as _;
use std::path::Path;

pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

const DOCUMENT_FORMATS: &[&str] = &["docx", "odt", "rtf", "epub"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileText {
    pub format: &'static str,
    pub text: String,
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_document(ext: &str) -> bool {
    DOCUMENT_FORMATS.contains(&ext)
}

pub fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    read_bounded(path)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        crate::i18n::CATALOG.t_args(
            "files-cannot-read",
            &[
                ("path", ui::i18n::ArgValue::Text(path.display().to_string())),
                ("detail", ui::i18n::ArgValue::Text(error.to_string())),
            ],
        )
    })?;
    if metadata.is_dir() {
        return Err(crate::i18n::CATALOG.t_args(
            "files-is-a-folder",
            &[("path", ui::i18n::ArgValue::Text(path.display().to_string()))],
        ));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(crate::i18n::CATALOG.t_args(
            "files-too-large",
            &[
                ("path", ui::i18n::ArgValue::Text(path.display().to_string())),
                (
                    "limit",
                    ui::i18n::ArgValue::Text(ui::format::memory(MAX_FILE_BYTES)),
                ),
            ],
        ));
    }
    std::fs::read(path).map_err(|error| read_failure(path, &error))
}

fn read_failure(path: &Path, error: &std::io::Error) -> String {
    if crate::model::read_availability(path).is_some_and(|availability| availability.unavailable())
    {
        format!(
            "{} is unavailable until a device that keeps it is online",
            path.display()
        )
    } else {
        format!("cannot read {}: {error}", path.display())
    }
}

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

pub fn bytes_for(name: &str, text: &str) -> Result<Vec<u8>, String> {
    writable(name)?;
    Ok(text.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_file_no_device_delivered_reads_as_unavailable_not_as_an_io_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Report.pdf");
        std::fs::write(&path, b"").unwrap();
        let eio = std::io::Error::from_raw_os_error(5);
        assert!(
            read_failure(&path, &eio).starts_with("cannot read"),
            "a file with no availability record keeps its error"
        );
        let availability = |last_error: Option<&str>| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "current_version": "a0d4e23f-c7cb-4ded-a427-b32cf9da9342",
                "residency": {"state": "not_resident"},
                "transfer": {"phase": "idle", "progress": null, "last_error": last_error,
                    "last_error_at_ms": last_error.map(|_| 12), "error_truncated": false},
                "policy": "automatic",
                "replicas": {"state": "unknown"},
                "freshness": {"metadata_observed_at_ms": 10,
                    "local_verified_at_ms": null, "replicas_observed_at_ms": null}
            }))
            .unwrap()
        };
        host_fs::attributes::set(&path, fs_protocol::AVAILABILITY_XATTR, &availability(None))
            .unwrap();
        assert!(read_failure(&path, &eio).starts_with("cannot read"));

        host_fs::attributes::set(
            &path,
            fs_protocol::AVAILABILITY_XATTR,
            &availability(Some(fs_protocol::NO_PEER_DELIVERED_ERROR)),
        )
        .unwrap();
        let failure = read_failure(&path, &eio);
        assert!(
            failure.ends_with("is unavailable until a device that keeps it is online"),
            "{failure}"
        );
        assert!(!failure.contains("os error"), "{failure}");
    }

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
