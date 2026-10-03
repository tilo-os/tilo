//! ODF container: a `.odt` is a ZIP whose first entry must be an uncompressed
//! `mimetype`. This reads every part into memory preserving entry order, and
//! re-writes byte-faithfully (mimetype first + stored, the rest deflated). It is
//! the lossless backbone: parts the codec doesn't model are carried verbatim.

use crate::format::CodecError;
use crate::safety::{ZipLimits, read_zip_parts};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub struct OdtContainer {
    /// Parts in original zip order: `(name, bytes)`.
    pub parts: Vec<(String, Vec<u8>)>,
}

impl OdtContainer {
    /// Read an untrusted `.odt` package; bounded against zip bombs and rejects DTD-bearing XML (XXE) via [`read_zip_parts`].
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

    /// Serialise the parts back into a package.
    ///
    /// # Errors
    /// Returns [`CodecError`] if the zip writer rejects a part name or fails to
    /// write its bytes.
    pub fn write(&self) -> Result<Vec<u8>, CodecError> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        let werr = |e: zip::result::ZipError| CodecError::Export(format!("zip write: {e}"));
        let ierr = |e: std::io::Error| CodecError::Export(format!("zip io: {e}"));

        // ODF requires `mimetype` to be the first entry and stored uncompressed.
        if let Some(mt) = self.get("mimetype") {
            zw.start_file("mimetype", stored).map_err(werr)?;
            zw.write_all(mt).map_err(ierr)?;
        }
        for (name, bytes) in &self.parts {
            if name == "mimetype" {
                continue;
            }
            zw.start_file(name.clone(), deflated).map_err(werr)?;
            zw.write_all(bytes).map_err(ierr)?;
        }
        let cur = zw.finish().map_err(werr)?;
        Ok(cur.into_inner())
    }
}

/// Build a minimal valid `.odt` body for self-contained tests (no `LibreOffice` dependency).
#[cfg(test)]
pub(crate) fn minimal_odt(body_xml: &str) -> Vec<u8> {
    minimal_odt_styled("", body_xml)
}

/// As [`minimal_odt`], with explicit `office:automatic-styles` content.
#[cfg(test)]
pub(crate) fn minimal_odt_styled(styles_xml: &str, body_xml: &str) -> Vec<u8> {
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0">
<office:automatic-styles>{styles_xml}</office:automatic-styles>
<office:body><office:text>{body_xml}</office:text></office:body>
</office:document-content>"#
    );
    let manifest = r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
<manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>
<manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#;
    let mut c = OdtContainer { parts: vec![] };
    c.set(
        "mimetype",
        b"application/vnd.oasis.opendocument.text".to_vec(),
    );
    c.set("content.xml", content.into_bytes());
    c.set("META-INF/manifest.xml", manifest.as_bytes().to_vec());
    c.write().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_parts_roundtrip_and_replace_in_place() {
        // Phase `container_roundtrips_parts`.
        {
            let odt = minimal_odt("<text:p>hi</text:p>");
            let c = OdtContainer::read(&odt).unwrap();
            assert_eq!(c.parts[0].0, "mimetype", "mimetype must be the first entry");
            assert_eq!(
                c.get("mimetype").unwrap(),
                b"application/vnd.oasis.opendocument.text"
            );
            assert!(c.get("content.xml").is_some());

            let out = c.write().unwrap();
            let c2 = OdtContainer::read(&out).unwrap();
            let names1: Vec<_> = c.parts.iter().map(|(n, _)| n.clone()).collect();
            let names2: Vec<_> = c2.parts.iter().map(|(n, _)| n.clone()).collect();
            assert_eq!(names1, names2, "part set + order preserved");
            assert_eq!(
                c2.get("content.xml"),
                c.get("content.xml"),
                "content preserved verbatim"
            );
            assert_eq!(
                c2.parts[0].0, "mimetype",
                "mimetype still first after rewrite"
            );
        }
        // Phase `set_replaces_in_place_and_appends`.
        {
            let mut c = OdtContainer { parts: vec![] };
            c.set("a", b"1".to_vec());
            c.set("b", b"2".to_vec());
            c.set("a", b"3".to_vec());
            assert_eq!(c.parts.len(), 2);
            assert_eq!(c.get("a").unwrap(), b"3");
            assert_eq!(c.parts[0].0, "a");
        }
    }
}
