//! Document files read as Markdown and converted to other formats, without
//! opening them.
//!
//! Writer links the document codecs, so it is where a file is read "as
//! text" or written again in another format for anyone who asks: a person
//! from the palette, an agent summarising a `.docx` or turning a Markdown
//! summary into a PDF. Neither touches the document open in the window. The
//! conversions are [`tilo_doc_codecs::convert`], so every caller reads a
//! document as Markdown and writes Markdown as a document the same way.

use std::path::{Path, PathBuf};

use tilo_doc_codecs::convert;

pub const CONVERSIONS: &[&str] = &["pdf", "docx", "odt", "md", "html", "rtf", "txt"];

/// The most characters one read answers with.
pub const MAX_READ_CHARS: usize = 64 * 1024;

fn extension(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn absolute(file: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(file.trim());
    if !path.is_absolute() {
        return Err(format!("`{file}` is not an absolute path"));
    }
    Ok(path)
}

/// The bytes of the file at `path`, at most
/// [`convert::MAX_DOCUMENT_BYTES`] of them.
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

/// Refuse a PDF as a source: Writer lays PDFs out but does not read them.
fn not_a_pdf(path: &Path) -> Result<(), String> {
    if extension(path) == "pdf" {
        return Err(format!(
            "{} is a PDF: Files' `read-text` reads its text, which converts from Markdown",
            path.display()
        ));
    }
    Ok(())
}

/// What the document `file` says, as Markdown (the text itself for plain
/// text), from character `from` on, at most [`MAX_READ_CHARS`] of it.
///
/// # Errors
///
/// `file` is not an absolute path, cannot be read, is too large, is a PDF,
/// or its format refuses it.
pub fn read_file(file: &str, from: Option<usize>) -> Result<serde_json::Value, String> {
    let path = absolute(file)?;
    not_a_pdf(&path)?;
    let bytes = read_bounded(&path)?;
    if let Some(format) = picture_format(&bytes) {
        return Err(format!(
            "{} is a picture ({}), not text",
            path.display(),
            format.mime
        ));
    }
    let (format, text) = as_text(&path, &bytes)?;
    Ok(page(&path, format, &text, from.unwrap_or(0)))
}

/// What the document kept as `content` in `store` says, read as
/// [`read_file`] reads a file named `name`. Content that is a picture,
/// whatever its name, answers as one ([`picture`]).
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
        return Ok(picture(name.trim(), hash, format, &bytes));
    }
    not_a_pdf(path)?;
    if bytes.len() > convert::MAX_DOCUMENT_BYTES {
        return Err(format!(
            "{name} is larger than {}",
            ui::format::memory(convert::MAX_DOCUMENT_BYTES as u64)
        ));
    }
    let (format, text) = as_text(path, &bytes)?;
    Ok(page(path, format, &text, from.unwrap_or(0)))
}

/// The raster picture format `bytes` start with: a scan or a photo, which
/// has no text to read. An SVG is text and is read as such.
fn picture_format(bytes: &[u8]) -> Option<tilo_content::ImageFormat> {
    tilo_content::image_format(bytes).filter(|format| format.extension != "svg")
}

/// The answer for content at `hash` that is a picture: `format` `picture`,
/// its address under `picture`, its type, size and weight. An agent's host
/// takes it to show the picture to a model that sees.
fn picture(
    name: &str,
    hash: &str,
    format: tilo_content::ImageFormat,
    bytes: &[u8],
) -> serde_json::Value {
    let size = tilo_content::image_size(bytes);
    serde_json::json!({
        "file": name,
        "format": "picture",
        "picture": format!("tilo.content:{hash}"),
        "media_type": format.mime,
        "bytes": bytes.len(),
        "width": size.map(|(_, width, _)| width),
        "height": size.map(|(_, _, height)| height),
    })
}

/// What `bytes` of the file `path` names say, and whether as Markdown or
/// as text.
fn as_text(path: &Path, bytes: &[u8]) -> Result<(&'static str, String), String> {
    let ext = extension(path);
    let format = match convert::reading(&ext) {
        convert::Reading::Markdown | convert::Reading::Document => "markdown",
        convert::Reading::Text => "text",
    };
    let text = convert::markdown_of(&ext, bytes)
        .map_err(|error| format!("{} cannot be read as text: {error}", path.display()))?;
    Ok((format, text))
}

/// The part of `text` from character `from` on.
fn page(path: &Path, format: &str, text: &str, from: usize) -> serde_json::Value {
    let total = text.chars().count();
    let from = from.min(total);
    let to = from.saturating_add(MAX_READ_CHARS).min(total);
    let byte = |n: usize| text.char_indices().nth(n).map_or(text.len(), |(at, _)| at);
    serde_json::json!({
        "file": path.display().to_string(),
        "format": format,
        "text": text.get(byte(from)..byte(to)).unwrap_or_default(),
        "from": from,
        "next": (to < total).then_some(to),
        "total": total,
    })
}

/// The document file at `path` written again as format `to` (an extension
/// from [`CONVERSIONS`]).
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
/// `to` is not one of [`CONVERSIONS`], `file` is not an absolute path or
/// cannot be read as a document, or the new file cannot be written.
pub fn convert_file(file: &str, to: &str) -> Result<PathBuf, String> {
    let to = to.trim().trim_start_matches('.').to_ascii_lowercase();
    if !CONVERSIONS.contains(&to.as_str()) {
        return Err(format!(
            "files convert to {}, not .{to}",
            CONVERSIONS.join(", ")
        ));
    }
    let path = absolute(file)?;
    let bytes = converted(&path, &to)?;
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
        let source = source.display().to_string();

        let docx = convert_file(&source, ".DOCX").unwrap();
        assert!(docx.ends_with("Letter.docx"));
        let read = read_file(&docx.display().to_string(), None).unwrap();
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
        assert!(
            convert_file("Letter.md", "pdf").is_err(),
            "a path, absolute"
        );
        assert!(read_file(&root.path().join("x.pdf").display().to_string(), None).is_err());
    }

    /// A document kept as content (an attachment) reads as its name's
    /// format says, as a file does.
    #[test]
    fn a_document_kept_as_content_reads_as_markdown() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Letter.md");
        std::fs::write(&source, "# Dear reader\n\nThank you for writing.").unwrap();
        let docx =
            std::fs::read(convert_file(&source.display().to_string(), "docx").unwrap()).unwrap();
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
        assert!(read_content(&store, (&address, "Letter.pdf"), None).is_err());
        assert!(read_content(&store, ("Letter.docx", "Letter.docx"), None).is_err());
        let gone = format!("tilo.content:{}", "0".repeat(64));
        assert_eq!(
            read_content(&store, (&gone, "Letter.docx"), None).unwrap_err(),
            "Letter.docx is not on this device"
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
        let refused = read_file(&file.display().to_string(), None).unwrap_err();
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
            joined.push_str(part["text"].as_str().unwrap());
            from = part["next"]
                .as_u64()
                .map(|next| usize::try_from(next).unwrap());
        }
        assert_eq!(joined, text);
    }
}
