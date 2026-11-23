//! What a file says as text, and text written as a file.

use std::fmt::Write as _;
use std::path::Path;

pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

const DOCUMENT_FORMATS: &[&str] = &["docx", "odt", "rtf", "epub"];

/// Workbooks, whose cells Sheets reads; a `.csv` file is text and reads here.
const SPREADSHEET_FORMATS: &[&str] = &["xlsx", "ods"];

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
    read_file_with_limit(path, MAX_FILE_BYTES)
}

fn too_large(path: &Path, limit: u64) -> String {
    crate::i18n::CATALOG.t_args(
        "files-too-large",
        &[
            ("path", ui::i18n::ArgValue::Text(path.display().to_string())),
            ("limit", ui::i18n::ArgValue::Text(ui::format::memory(limit))),
        ],
    )
}

fn is_a_folder(path: &Path) -> String {
    crate::i18n::CATALOG.t_args(
        "files-is-a-folder",
        &[("path", ui::i18n::ArgValue::Text(path.display().to_string()))],
    )
}

fn open_failure(path: &Path, error: &std::io::Error) -> String {
    if crate::model::read_availability(path).is_some_and(|availability| availability.unavailable())
    {
        format!(
            "{} is unavailable until a device that keeps it is online",
            path.display()
        )
    } else {
        crate::i18n::CATALOG.t_args(
            "files-cannot-read",
            &[
                ("path", ui::i18n::ArgValue::Text(path.display().to_string())),
                ("detail", ui::i18n::ArgValue::Text(error.to_string())),
            ],
        )
    }
}

fn read_file_with_limit(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            if path.is_dir() {
                return Err(is_a_folder(path));
            }
            return Err(open_failure(path, &error));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| open_failure(path, &error))?;
    if metadata.is_dir() {
        return Err(is_a_folder(path));
    }
    if metadata.len() > limit {
        return Err(too_large(path, limit));
    }
    tilo_files::read_from(file, limit).map_err(|error| {
        if error.kind() == std::io::ErrorKind::FileTooLarge {
            too_large(path, limit)
        } else {
            read_failure(path, &error)
        }
    })
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
    if SPREADSHEET_FORMATS.contains(&ext.as_str()) {
        return Err(format!(
            "{} is a .{ext} spreadsheet: Sheets' `read-file` reads its cells",
            path.display()
        ));
    }
    let bytes = read_file(path)?;
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

    #[test]
    fn a_workbook_is_refused_by_the_name_of_what_reads_it_and_a_csv_file_reads_as_text() {
        let root = tempfile::tempdir().unwrap();
        let workbook = root.path().join("Budget.XLSX");
        std::fs::write(&workbook, b"PK\x03\x04").unwrap();
        let refused = read_text(&workbook).unwrap_err();
        assert!(refused.contains("Sheets' `read-file`"), "{refused}");
        let leads = root.path().join("leads.csv");
        std::fs::write(&leads, "Company,Value\nAcme,1200\n").unwrap();
        assert_eq!(
            read_text(&leads).unwrap().text,
            "Company,Value\nAcme,1200\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_no_device_delivered_reads_as_unavailable_not_as_an_io_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Report.pdf");
        std::fs::write(&path, b"").unwrap();
        let eio = std::io::Error::from_raw_os_error(5);
        assert!(
            read_failure(&path, &eio).starts_with("cannot read"),
            "a file with no availability resource keeps its error"
        );
        let availability = |last_error: Option<&str>| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2,
                "document_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "current_hash": "4242424242424242424242424242424242424242424242424242424242424242",
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

    tilo_test_env::scenario! {
        fn text_is_read_and_written_and_other_files_are_refused_with_a_reason;

        phase text_is_written_as_given_and_reads_back;
        {
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

        phase documents_are_left_to_writer_with_a_reason;
        {
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

        phase binary_files_and_folders_are_refused_with_a_reason;
        {
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

        phase bounded_reads_refuse_missing_oversized_and_growing_sources;
        {
            let root = tempfile::tempdir().unwrap();
            let missing = root.path().join("missing.txt");
            assert!(read_file(&missing).is_err());

            let exact = root.path().join("exact.txt");
            std::fs::write(&exact, b"12345678").unwrap();
            assert_eq!(read_file_with_limit(&exact, 8).unwrap(), b"12345678");

            let one_over = root.path().join("over.txt");
            std::fs::write(&one_over, b"123456789").unwrap();
            let refused = read_file_with_limit(&one_over, 8).unwrap_err();
            assert!(refused.contains("larger than"), "{refused}");

            #[cfg(unix)]
            {
                let zero = Path::new("/dev/zero");
                assert_eq!(std::fs::metadata(zero).unwrap().len(), 0);
                let refused = read_file_with_limit(zero, 16).unwrap_err();
                assert!(refused.contains("larger than"), "{refused}");
                let refused = read_file(zero).unwrap_err();
                assert!(refused.contains("larger than"), "{refused}");
            }
        }
    }
}
