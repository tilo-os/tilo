//! OOXML (`.docx`) container: an ordinary ZIP (Open Packaging Conventions) whose parts
//! include `word/document.xml`, `word/styles.xml`, `[Content_Types].xml`, `_rels/.rels`,
//! etc. Unlike ODF there is no special first/stored entry, so every part is deflated.
//! This reads every part preserving entry order and re-writes byte-faithfully — the
//! lossless backbone for parts the codec doesn't model, carried verbatim as preservation.

use crate::format::CodecError;
use crate::safety::{ZipLimits, read_zip_parts};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub struct DocxContainer {
    /// Parts in original zip order: `(name, bytes)`.
    pub parts: Vec<(String, Vec<u8>)>,
}

impl DocxContainer {
    /// Read an untrusted `.docx` package, bounded against zip bombs and rejecting
    /// DTD-bearing XML parts (XXE) via [`read_zip_parts`].
    ///
    /// # Errors
    /// Returns [`CodecError`] if `bytes` are not a readable zip archive, if the
    /// archive exceeds [`ZipLimits`], or if an XML part declares a DTD.
    pub fn read(bytes: &[u8]) -> Result<Self, CodecError> {
        let parts = read_zip_parts(bytes, &ZipLimits::default())?;
        Ok(Self { parts })
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    }

    /// Replace an existing part (preserving its position) or append a new one.
    pub fn set(&mut self, name: &str, bytes: Vec<u8>) {
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == name) {
            p.1 = bytes;
        } else {
            self.parts.push((name.to_string(), bytes));
        }
    }

    /// Remove a part, if present.
    pub fn remove(&mut self, name: &str) {
        self.parts.retain(|(n, _)| n != name);
    }

    /// Serialise the parts back into a package.
    ///
    /// # Errors
    /// Returns [`CodecError`] if the zip writer rejects a part name or fails to
    /// write its bytes.
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

/// As [`minimal_docx`], with explicit `word/styles.xml` inner content.
#[cfg(test)]
pub(crate) fn minimal_docx_styled(styles_inner: &str, body_inner: &str) -> Vec<u8> {
    build_docx(styles_inner, "", body_inner)
}

/// As [`minimal_docx_styled`], plus `word/numbering.xml` inner content.
#[cfg(test)]
pub(crate) fn minimal_docx_numbered(
    styles_inner: &str,
    numbering_inner: &str,
    body_inner: &str,
) -> Vec<u8> {
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
    fn container_parts_roundtrip_and_replace_in_place() {
        // Phase `container_roundtrips_parts`.
        {
            let docx = minimal_docx("<w:p><w:r><w:t>hi</w:t></w:r></w:p>");
            let c = DocxContainer::read(&docx).unwrap();
            assert_eq!(
                c.parts[0].0, "[Content_Types].xml",
                "content-types is the first entry"
            );
            assert!(c.get("word/document.xml").is_some());
            assert!(c.get("_rels/.rels").is_some());

            // Re-write and re-read → identical part names + content.
            let out = c.write().unwrap();
            let c2 = DocxContainer::read(&out).unwrap();
            let names1: Vec<_> = c.parts.iter().map(|(n, _)| n.clone()).collect();
            let names2: Vec<_> = c2.parts.iter().map(|(n, _)| n.clone()).collect();
            assert_eq!(names1, names2, "part set + order preserved");
            assert_eq!(
                c2.get("word/document.xml"),
                c.get("word/document.xml"),
                "body preserved verbatim"
            );
        }
        // Phase `set_replaces_in_place_and_appends`.
        {
            let mut c = DocxContainer { parts: vec![] };
            c.set("a", b"1".to_vec());
            c.set("b", b"2".to_vec());
            c.set("a", b"3".to_vec()); // replace in place
            assert_eq!(c.parts.len(), 2);
            assert_eq!(c.get("a").unwrap(), b"3");
            assert_eq!(c.parts[0].0, "a"); // position preserved
        }
    }
}
