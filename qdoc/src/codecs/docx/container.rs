//! OOXML (`.docx`) container: a `.docx` is an ordinary ZIP (Open Packaging
//! Conventions) whose parts include `word/document.xml` (the body),
//! `word/styles.xml`, `[Content_Types].xml`, `_rels/.rels`, etc. Unlike ODF
//! there is no special first/stored entry, so every part is deflated. This reads
//! every part into memory preserving entry order and re-writes byte-faithfully;
//! it is the lossless backbone — parts the codec doesn't model are carried
//! verbatim through the preservation layer.

use crate::format::CodecError;
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub struct DocxContainer {
    /// Parts in original zip order: `(name, bytes)`.
    pub parts: Vec<(String, Vec<u8>)>,
}

impl DocxContainer {
    pub fn read(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut ar = ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| CodecError::Parse(format!("zip open: {e}")))?;
        let mut parts = Vec::with_capacity(ar.len());
        for i in 0..ar.len() {
            let mut f = ar
                .by_index(i)
                .map_err(|e| CodecError::Parse(format!("zip entry {i}: {e}")))?;
            let name = f.name().to_string();
            if name.ends_with('/') {
                continue; // directory entry
            }
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)
                .map_err(|e| CodecError::Parse(format!("zip read {name}: {e}")))?;
            parts.push((name, buf));
        }
        Ok(Self { parts })
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.parts.iter().find(|(n, _)| n == name).map(|(_, b)| b.as_slice())
    }

    /// Replace an existing part (preserving its position) or append a new one.
    pub fn set(&mut self, name: &str, bytes: Vec<u8>) {
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == name) {
            p.1 = bytes;
        } else {
            self.parts.push((name.to_string(), bytes));
        }
    }

    pub fn write(&self) -> Result<Vec<u8>, CodecError> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        let werr = |e: zip::result::ZipError| CodecError::Export(format!("zip write: {e}"));
        let ierr = |e: std::io::Error| CodecError::Export(format!("zip io: {e}"));

        for (name, bytes) in &self.parts {
            zw.start_file(name.clone(), deflated).map_err(werr)?;
            zw.write_all(bytes).map_err(ierr)?;
        }
        let cur = zw.finish().map_err(werr)?;
        Ok(cur.into_inner())
    }
}

// ---- in-test fixtures ------------------------------------------------------

/// Wrap a `w:body` inner XML into a complete `word/document.xml`.
#[cfg(test)]
pub(crate) fn document_xml(body_inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body_inner}</w:body></w:document>"#
    )
}

/// Build a minimal valid `.docx` with the given `w:body` inner XML.
#[cfg(test)]
pub(crate) fn minimal_docx(body_inner: &str) -> Vec<u8> {
    build_docx("", "", body_inner)
}

/// As [`minimal_docx`], with explicit `word/styles.xml` inner content (the
/// `<w:style>` elements that go inside `<w:styles>`).
#[cfg(test)]
pub(crate) fn minimal_docx_styled(styles_inner: &str, body_inner: &str) -> Vec<u8> {
    build_docx(styles_inner, "", body_inner)
}

/// As [`minimal_docx_styled`], plus `word/numbering.xml` inner content (the
/// `<w:abstractNum>`/`<w:num>` elements that go inside `<w:numbering>`).
#[cfg(test)]
pub(crate) fn minimal_docx_numbered(styles_inner: &str, numbering_inner: &str, body_inner: &str) -> Vec<u8> {
    build_docx(styles_inner, numbering_inner, body_inner)
}

#[cfg(test)]
fn build_docx(styles_inner: &str, numbering_inner: &str, body_inner: &str) -> Vec<u8> {
    let has_num = !numbering_inner.is_empty();
    let content_types = {
        let num_ovr = if has_num {
            r#"<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#
        } else {
            ""
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>{num_ovr}</Types>"#
        )
    };
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = {
        let num_rel = if has_num {
            r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#
        } else {
            ""
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>{num_rel}</Relationships>"#
        )
    };
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{styles_inner}</w:styles>"#
    );

    let mut c = DocxContainer { parts: vec![] };
    c.set("[Content_Types].xml", content_types.into_bytes());
    c.set("_rels/.rels", root_rels.as_bytes().to_vec());
    c.set("word/_rels/document.xml.rels", doc_rels.into_bytes());
    c.set("word/document.xml", document_xml(body_inner).into_bytes());
    c.set("word/styles.xml", styles.into_bytes());
    if has_num {
        let numbering = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{numbering_inner}</w:numbering>"#
        );
        c.set("word/numbering.xml", numbering.into_bytes());
    }
    c.write().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_roundtrips_parts() {
        let docx = minimal_docx("<w:p><w:r><w:t>hi</w:t></w:r></w:p>");
        let c = DocxContainer::read(&docx).unwrap();
        assert_eq!(c.parts[0].0, "[Content_Types].xml", "content-types is the first entry");
        assert!(c.get("word/document.xml").is_some());
        assert!(c.get("_rels/.rels").is_some());

        // Re-write and re-read → identical part names + content.
        let out = c.write().unwrap();
        let c2 = DocxContainer::read(&out).unwrap();
        let names1: Vec<_> = c.parts.iter().map(|(n, _)| n.clone()).collect();
        let names2: Vec<_> = c2.parts.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names1, names2, "part set + order preserved");
        assert_eq!(c2.get("word/document.xml"), c.get("word/document.xml"), "body preserved verbatim");
    }

    #[test]
    fn set_replaces_in_place_and_appends() {
        let mut c = DocxContainer { parts: vec![] };
        c.set("a", b"1".to_vec());
        c.set("b", b"2".to_vec());
        c.set("a", b"3".to_vec()); // replace in place
        assert_eq!(c.parts.len(), 2);
        assert_eq!(c.get("a").unwrap(), b"3");
        assert_eq!(c.parts[0].0, "a"); // position preserved
    }
}
