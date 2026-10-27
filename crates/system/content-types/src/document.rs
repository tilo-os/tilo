//! What document a piece of content is, read from its bytes: content travels under whatever name
//! its sender gave it ("Sofia details", "payslips.pdf" for a Word file), so a reader that trusts
//! the name refuses what it could read, or hands it to the wrong app.

/// A document format, named by its file extension and media type, with what a person calls it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentFormat {
    pub extension: &'static str,
    pub mime: &'static str,
    /// What it is called in a sentence: "a Word document".
    pub kind: &'static str,
    /// The app actions that open it, as an agent addresses them; `None` when none does.
    pub opened_by: Option<&'static str>,
}

/// What reads a word-processing document.
const WRITER: &str = "word-processor import-docx-content or read-content";

const PDF: DocumentFormat = DocumentFormat {
    extension: "pdf",
    mime: "application/pdf",
    kind: "a PDF",
    opened_by: Some("pdf-viewer import-pdf-content"),
};
const DOCX: DocumentFormat = DocumentFormat {
    extension: "docx",
    mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    kind: "a Word document",
    opened_by: Some(WRITER),
};
const XLSX: DocumentFormat = DocumentFormat {
    extension: "xlsx",
    mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    kind: "an Excel workbook",
    opened_by: Some("sheets import-xlsx-content"),
};
const PPTX: DocumentFormat = DocumentFormat {
    extension: "pptx",
    mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    kind: "a PowerPoint deck",
    opened_by: Some("slides import-pptx-content"),
};
const ODT: DocumentFormat = DocumentFormat {
    extension: "odt",
    mime: "application/vnd.oasis.opendocument.text",
    kind: "an OpenDocument text",
    opened_by: Some(WRITER),
};
const ODS: DocumentFormat = DocumentFormat {
    extension: "ods",
    mime: "application/vnd.oasis.opendocument.spreadsheet",
    kind: "an OpenDocument spreadsheet",
    opened_by: None,
};
const ODP: DocumentFormat = DocumentFormat {
    extension: "odp",
    mime: "application/vnd.oasis.opendocument.presentation",
    kind: "an OpenDocument presentation",
    opened_by: None,
};
const EPUB: DocumentFormat = DocumentFormat {
    extension: "epub",
    mime: "application/epub+zip",
    kind: "an EPUB book",
    opened_by: None,
};
const RTF: DocumentFormat = DocumentFormat {
    extension: "rtf",
    mime: "application/rtf",
    kind: "an RTF document",
    opened_by: Some(WRITER),
};
const HTML: DocumentFormat = DocumentFormat {
    extension: "html",
    mime: "text/html",
    kind: "an HTML page",
    opened_by: Some(WRITER),
};

/// The document format `bytes` hold: a PDF by its `%PDF-` header, an Office or `OpenDocument`
/// package by the parts and media type its zip holds, RTF, or an HTML page. `None` for plain
/// text, Markdown and anything else, whose name is all that says what it is.
#[must_use]
pub fn document_format(bytes: &[u8]) -> Option<DocumentFormat> {
    if bytes.starts_with(b"PK\x03\x04") {
        return package_format(bytes);
    }
    let text = bytes
        .strip_prefix(b"\xef\xbb\xbf")
        .unwrap_or(bytes)
        .trim_ascii_start();
    if text.starts_with(b"%PDF-") {
        return Some(PDF);
    }
    if text.starts_with(b"{\\rtf") {
        return Some(RTF);
    }
    let start = text.get(..text.len().min(14)).unwrap_or_default();
    (start.eq_ignore_ascii_case(b"<!doctype html")
        || start
            .get(..5)
            .is_some_and(|tag| tag.eq_ignore_ascii_case(b"<html")))
    .then_some(HTML)
}

/// What an agent is told of content an action does not open: what it is and what opens it,
/// "this is a Word document; use word-processor import-docx-content or read-content". `None`
/// when the bytes say nothing (plain text, or a format not known here).
#[must_use]
pub fn opened_by(bytes: &[u8]) -> Option<String> {
    if let Some(picture) = crate::image_format(bytes).filter(|format| format.extension != "svg") {
        return Some(format!(
            "this is a picture ({}); use word-processor read-content, which shows it",
            picture.mime
        ));
    }
    let format = document_format(bytes)?;
    Some(match format.opened_by {
        Some(opener) => format!("this is {}; use {opener}", format.kind),
        None => format!("this is {}, which no app here opens", format.kind),
    })
}

/// The format of the zip package `bytes`: Office by its main part, `OpenDocument` and EPUB by the
/// media type kept uncompressed in their first entry, `mimetype`.
fn package_format(bytes: &[u8]) -> Option<DocumentFormat> {
    if bytes.get(30..38) == Some(b"mimetype") && read_u16le(bytes, 8) == Some(0) {
        let size = usize::try_from(read_u32le(bytes, 18)?).ok()?;
        let extra = usize::from(read_u16le(bytes, 28)?);
        let from = 38_usize.checked_add(extra)?;
        let mime = bytes.get(from..from.checked_add(size)?)?;
        return [ODT, ODS, ODP, EPUB]
            .into_iter()
            .find(|format| mime.trim_ascii() == format.mime.as_bytes());
    }
    let names = central_names(bytes)?;
    [
        (DOCX, "word/document.xml"),
        (XLSX, "xl/workbook.xml"),
        (PPTX, "ppt/presentation.xml"),
    ]
    .into_iter()
    .find(|(_, part)| names.contains(&part.as_bytes()))
    .map(|(format, _)| format)
}

/// The most entries of a package's directory read: a document has dozens.
const MOST_ENTRIES: usize = 4096;

/// The entry names in the central directory of the zip `bytes`, from its end record.
fn central_names(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    // The end record is 22 bytes, then a comment of at most 64 KiB.
    let floor = bytes.len().saturating_sub(22_usize.saturating_add(0xffff));
    let end = (floor..bytes.len().saturating_sub(21))
        .rev()
        .find(|&at| bytes.get(at..at.saturating_add(4)) == Some(b"PK\x05\x06"))?;
    let entries = usize::from(read_u16le(bytes, end.saturating_add(10))?).min(MOST_ENTRIES);
    let mut at = usize::try_from(read_u32le(bytes, end.saturating_add(16))?).ok()?;
    let mut names = Vec::with_capacity(entries);
    for _ in 0..entries {
        if bytes.get(at..at.saturating_add(4)) != Some(b"PK\x01\x02") {
            break;
        }
        let name = usize::from(read_u16le(bytes, at.saturating_add(28))?);
        let extra = usize::from(read_u16le(bytes, at.saturating_add(30))?);
        let comment = usize::from(read_u16le(bytes, at.saturating_add(32))?);
        let from = at.checked_add(46)?;
        names.push(bytes.get(from..from.checked_add(name)?)?);
        at = from
            .checked_add(name)?
            .checked_add(extra)?
            .checked_add(comment)?;
    }
    Some(names)
}

fn read_u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored (uncompressed) zip of `entries`, as a package is laid out.
    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut directory = Vec::new();
        for (name, data) in entries {
            let offset = u32::try_from(out.len()).unwrap();
            let name_len = u16::try_from(name.len()).unwrap();
            let size = u32::try_from(data.len()).unwrap();
            out.extend(b"PK\x03\x04");
            out.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend(size.to_le_bytes());
            out.extend(size.to_le_bytes());
            out.extend(name_len.to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(*data);
            directory.extend(b"PK\x01\x02");
            directory.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            directory.extend(size.to_le_bytes());
            directory.extend(size.to_le_bytes());
            directory.extend(name_len.to_le_bytes());
            directory.extend([0; 12]);
            directory.extend(offset.to_le_bytes());
            directory.extend(name.as_bytes());
        }
        let at = u32::try_from(out.len()).unwrap();
        let count = u16::try_from(entries.len()).unwrap();
        out.extend(&directory);
        out.extend(b"PK\x05\x06\0\0\0\0");
        out.extend(count.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(u32::try_from(directory.len()).unwrap().to_le_bytes());
        out.extend(at.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    #[test]
    fn a_document_is_known_by_its_bytes() {
        let docx = zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", b"<w:document/>"),
        ]);
        assert_eq!(document_format(&docx), Some(DOCX));
        let deck = zip(&[("ppt/presentation.xml", b"<p/>")]);
        assert_eq!(document_format(&deck), Some(PPTX));
        let workbook = zip(&[("xl/workbook.xml", b"<w/>")]);
        assert_eq!(document_format(&workbook), Some(XLSX));
        let odt = zip(&[
            ("mimetype", ODT.mime.as_bytes()),
            ("content.xml", b"<office/>"),
        ]);
        assert_eq!(document_format(&odt), Some(ODT));
        assert_eq!(document_format(b"%PDF-1.7\n%\xe2\xe3"), Some(PDF));
        assert_eq!(document_format(b"{\\rtf1\\ansi Hi}"), Some(RTF));
        assert_eq!(document_format(b"\n<!DOCTYPE html><p>Hi"), Some(HTML));
        for other in [
            b"# Notes\n\nPlain words" as &[u8],
            b"",
            b"PK\x03\x04 truncated",
            &zip(&[("notes.txt", b"hi")]),
        ] {
            assert_eq!(document_format(other), None);
        }
    }

    #[test]
    fn content_an_action_does_not_open_names_what_does() {
        let docx = zip(&[("word/document.xml", b"<w:document/>")]);
        assert_eq!(
            opened_by(&docx).as_deref(),
            Some("this is a Word document; use word-processor import-docx-content or read-content")
        );
        assert_eq!(
            opened_by(b"%PDF-1.4").as_deref(),
            Some("this is a PDF; use pdf-viewer import-pdf-content")
        );
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR";
        assert!(opened_by(png).is_some_and(|said| said.contains("picture (image/png)")));
        assert_eq!(opened_by(b"plain words"), None);
    }
}
