//! Document files read as Markdown and written in another format, without opening
//! them. The conversions are [`tilo_doc_codecs::convert`], so every caller reads a
//! document the same way and the window's own document is untouched.

use std::path::{Path, PathBuf};

use tilo_doc_codecs::convert;

pub const CONVERSIONS: &[&str] = &["pdf", "docx", "odt", "md", "html", "rtf", "txt"];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, ui::tilo_resource::JsonSchema)]
pub struct ReadDocument {
    pub file: String,
    pub format: String,
    pub text: Option<String>,
    pub from: Option<usize>,
    pub next: Option<usize>,
    pub total: Option<usize>,
    pub picture: Option<String>,
    pub media_type: Option<String>,
    pub bytes: Option<usize>,
    pub width: Option<usize>,
    pub height: Option<usize>,
}

impl ReadDocument {
    fn answered(self) -> Result<serde_json::Value, String> {
        serde_json::to_value(self).map_err(|error| error.to_string())
    }
}

pub const MAX_READ_CHARS: usize = 64 * 1024;

fn extension(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let (dir, name) = tilo_files::file_at(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    tilo_files::blocking::read_bounded(&dir, &name, convert::MAX_DOCUMENT_BYTES)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?
        .ok_or_else(|| {
            format!(
                "{} is larger than {}",
                path.display(),
                ui::format::memory(convert::MAX_DOCUMENT_BYTES as u64)
            )
        })
}

fn not_a_pdf(path: &Path) -> Result<(), String> {
    if extension(path) == "pdf" {
        return Err(format!(
            "{} is a PDF: Files' `read-text` reads its text, which converts from Markdown",
            path.display()
        ));
    }
    Ok(())
}

/// What the document `file` says, as Markdown, from character `from` on, at most
/// [`MAX_READ_CHARS`] of it.
///
/// # Errors
/// `path` cannot be read, is too large, is a PDF, or its format refuses it.
pub fn read_file(path: &Path, from: Option<usize>) -> Result<serde_json::Value, String> {
    let bytes = read_bounded(path)?;
    if let Some(format) = picture_format(&bytes) {
        return Err(format!(
            "{} is a picture ({}), not text",
            path.display(),
            format.mime
        ));
    }
    let ext = read_as(path, &bytes)?;
    let (format, text) = as_text(path, &ext, &bytes)?;
    page(path, format, &text, from.unwrap_or(0)).answered()
}

pub fn read_content(
    store: &tilo_content::ContentStore,
    (content, name): (&str, &str),
    from: Option<usize>,
) -> Result<serde_json::Value, String> {
    let path = Path::new(name.trim());
    let hash = ui::tilo_actions::content_hash(content.trim())
        .ok_or_else(|| format!("`{content}` is not a `tilo.content:<hash>` address"))?;
    let bytes = store
        .read(hash)
        .ok_or_else(|| format!("{name} is not on this device"))?;
    if let Some(format) = picture_format(&bytes) {
        return picture(name.trim(), hash, format, &bytes).answered();
    }
    if bytes.len() > convert::MAX_DOCUMENT_BYTES {
        return Err(format!(
            "{name} is larger than {}",
            ui::format::memory(convert::MAX_DOCUMENT_BYTES as u64)
        ));
    }
    let ext = read_as(path, &bytes)?;
    let (format, text) = as_text(path, &ext, &bytes)?;
    page(path, format, &text, from.unwrap_or(0)).answered()
}

fn read_as(path: &Path, bytes: &[u8]) -> Result<String, String> {
    match tilo_content::document_format(bytes) {
        Some(format) if format.extension == "pdf" => Err(format!(
            "{} is a PDF: pdf-viewer import-pdf-content opens it, and Files' `read-text` reads a \
             PDF file's text",
            path.display()
        )),
        Some(format) if convert::reading(format.extension) == convert::Reading::Document => {
            Ok(format.extension.to_owned())
        }
        Some(_) => Err(format!(
            "{} is not a text document: {}",
            path.display(),
            tilo_content::opened_by(bytes).unwrap_or_default()
        )),
        None => Ok(match extension(path).as_str() {
            "docx" | "odt" | "rtf" => "txt".to_owned(),
            ext => ext.to_owned(),
        }),
    }
}

fn picture_format(bytes: &[u8]) -> Option<tilo_content::ImageFormat> {
    tilo_content::image_format(bytes).filter(|format| format.extension != "svg")
}

fn picture(
    name: &str,
    hash: &str,
    format: tilo_content::ImageFormat,
    bytes: &[u8],
) -> ReadDocument {
    let size = tilo_content::image_size(bytes);
    ReadDocument {
        file: name.to_owned(),
        format: "picture".to_owned(),
        text: None,
        from: None,
        next: None,
        total: None,
        picture: Some(format!("tilo.content:{hash}")),
        media_type: Some(format.mime.to_owned()),
        bytes: Some(bytes.len()),
        width: size.map(|(_, width, _)| usize::try_from(width).unwrap_or_default()),
        height: size.map(|(_, _, height)| usize::try_from(height).unwrap_or_default()),
    }
}

fn as_text(path: &Path, ext: &str, bytes: &[u8]) -> Result<(&'static str, String), String> {
    let format = match convert::reading(ext) {
        convert::Reading::Markdown | convert::Reading::Document => "markdown",
        convert::Reading::Text => "text",
    };
    let text = convert::markdown_of(ext, bytes)
        .map_err(|error| format!("{} cannot be read as text: {error}", path.display()))?;
    Ok((format, text))
}

fn page(path: &Path, format: &str, text: &str, from: usize) -> ReadDocument {
    let total = text.chars().count();
    let from = from.min(total);
    let to = from.saturating_add(MAX_READ_CHARS).min(total);
    let byte = |n: usize| text.char_indices().nth(n).map_or(text.len(), |(at, _)| at);
    ReadDocument {
        file: path.display().to_string(),
        format: format.to_owned(),
        text: Some(
            text.get(byte(from)..byte(to))
                .unwrap_or_default()
                .to_owned(),
        ),
        from: Some(from),
        next: (to < total).then_some(to),
        total: Some(total),
        picture: None,
        media_type: None,
        bytes: None,
        width: None,
        height: None,
    }
}

fn converted(path: &Path, to: &str) -> Result<Vec<u8>, String> {
    not_a_pdf(path)?;
    let refused = |error: tilo_doc_codecs::CodecError| {
        format!("{} cannot be converted: {error}", path.display())
    };
    let (document, inputs) =
        convert::import(&extension(path), &read_bounded(path)?).map_err(refused)?;
    if to == "pdf" {
        return tilo_doc_pdf::render_pdf(&document.as_ref().materialize(), &inputs)
            .map_err(|error| format!("{} cannot be converted: {error:?}", path.display()));
    }
    convert::export(&document, &inputs, to).map_err(refused)
}

/// Write the document file `file` again as format `to` beside it, under a
/// free name (`Letter.pdf`, then `Letter (2).pdf`, …); the new file's path.
///
/// # Errors
///
/// `to` is not one of [`CONVERSIONS`], `path` cannot be read as a
/// document, or the new file cannot be written.
pub fn convert_file(path: &Path, to: &str) -> Result<PathBuf, String> {
    let to = to.trim().trim_start_matches('.').to_ascii_lowercase();
    if !CONVERSIONS.contains(&to.as_str()) {
        return Err(format!(
            "files convert to {}, not .{to}",
            CONVERSIONS.join(", ")
        ));
    }
    let bytes = converted(path, &to)?;
    let stem = path.file_stem().map_or_else(
        || "Converted".into(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    for n in 1..=999 {
        let name = if n == 1 {
            format!("{stem}.{to}")
        } else {
            format!("{stem} ({n}).{to}")
        };
        let target = path.with_file_name(&name);
        let (dir, vname) = tilo_files::file_at(&target)
            .map_err(|error| format!("cannot write {}: {error}", target.display()))?;
        if tilo_files::blocking::create_new(&dir, &vname, &bytes)
            .map_err(|error| format!("cannot write {}: {error}", target.display()))?
        {
            return Ok(target);
        }
    }
    Err(format!(
        "no free name for {stem}.{to} beside {}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_converts_beside_itself_and_reads_back_as_markdown() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Letter.md");
        std::fs::write(&source, "# Dear reader\n\nThank you for writing.").unwrap();

        let docx = convert_file(&source, ".DOCX").unwrap();
        assert!(docx.ends_with("Letter.docx"));
        let read = read_file(&docx, None).unwrap();
        assert_eq!(read["format"], "markdown");
        assert!(
            read["text"].as_str().unwrap().contains("# Dear reader"),
            "{read}"
        );
        assert_eq!(read["next"], serde_json::Value::Null);

        let again = convert_file(&source, "docx").unwrap();
        assert!(again.ends_with("Letter (2).docx"), "never over a file");
        let pdf = convert_file(&source, "pdf").unwrap();
        assert!(std::fs::read(pdf).unwrap().starts_with(b"%PDF"));

        assert!(
            convert_file(&source, "exe")
                .unwrap_err()
                .contains("not .exe")
        );
        assert!(read_file(&root.path().join("x.pdf"), None).is_err());
    }

    #[test]
    fn a_document_kept_as_content_reads_as_markdown() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Letter.md");
        std::fs::write(&source, "# Dear reader\n\nThank you for writing.").unwrap();
        let docx = std::fs::read(convert_file(&source, "docx").unwrap()).unwrap();
        let store = tilo_content::ContentStore::memory();
        let address = format!("tilo.content:{}", store.put(&docx).unwrap());
        let read = read_content(&store, (&address, "Letter.docx"), None).unwrap();
        assert_eq!(
            (read["file"].as_str(), read["format"].as_str()),
            (Some("Letter.docx"), Some("markdown"))
        );
        assert!(
            read["text"].as_str().unwrap().contains("# Dear reader"),
            "{read}"
        );
        for name in ["Sofia details", "Letter.pdf", "Letter.md"] {
            let read = read_content(&store, (&address, name), None).unwrap();
            assert_eq!(read["format"], "markdown", "{name}: {read}");
            assert!(
                read["text"].as_str().unwrap().contains("# Dear reader"),
                "{name}: {read}"
            );
        }
        assert!(read_content(&store, ("Letter.docx", "Letter.docx"), None).is_err());
        let gone = format!("tilo.content:{}", "0".repeat(64));
        assert_eq!(
            read_content(&store, (&gone, "Letter.docx"), None).unwrap_err(),
            "Letter.docx is not on this device"
        );
    }

    #[test]
    fn content_writer_does_not_read_names_what_does() {
        let store = tilo_content::ContentStore::memory();
        let pdf = format!(
            "tilo.content:{}",
            store.put(b"%PDF-1.7\n%\xe2\xe3").unwrap()
        );
        let refused = read_content(&store, (&pdf, "Sofia details.docx"), None).unwrap_err();
        assert!(
            refused.contains("is a PDF: pdf-viewer import-pdf-content opens it"),
            "{refused}"
        );
        let text = format!("tilo.content:{}", store.put(b"Plain notes").unwrap());
        let read = read_content(&store, (&text, "notes.docx"), None).unwrap();
        assert_eq!(
            (read["format"].as_str(), read["text"].as_str()),
            (Some("text"), Some("Plain notes"))
        );
    }

    /// A scan kept as content answers as a picture, whatever the name it is
    /// read under says; a picture file is refused as not text.
    #[test]
    fn a_picture_kept_as_content_answers_as_a_picture() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(640_u32.to_be_bytes());
        png.extend(480_u32.to_be_bytes());
        png.extend([8, 2, 0, 0, 0]);
        let store = tilo_content::ContentStore::memory();
        let hash = store.put(&png).unwrap();
        let address = format!("tilo.content:{hash}");
        for name in ["passport.png", "passport.pdf", "scan of the passport"] {
            let read = read_content(&store, (&address, name), None).unwrap();
            assert_eq!(read["format"], "picture", "{read}");
            assert_eq!(read["picture"], address.as_str());
            assert_eq!(read["file"], name);
            assert_eq!(read["media_type"], "image/png");
            assert_eq!(
                (read["width"].as_u64(), read["height"].as_u64()),
                (Some(640), Some(480))
            );
            assert_eq!(read["bytes"].as_u64(), Some(png.len() as u64));
        }

        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("passport.png");
        std::fs::write(&file, &png).unwrap();
        let refused = read_file(&file, None).unwrap_err();
        assert!(
            refused.ends_with("is a picture (image/png), not text"),
            "{refused}"
        );
    }

    #[test]
    fn a_long_text_reads_in_parts_that_join_up() {
        let text: String = "ab\u{e9}".repeat(MAX_READ_CHARS);
        let path = Path::new("/tmp/book.txt");
        let mut joined = String::new();
        let mut from = Some(0);
        while let Some(at) = from {
            let part = page(path, "text", &text, at);
            joined.push_str(part.text.as_deref().unwrap());
            from = part.next;
        }
        assert_eq!(joined, text);
    }
}
