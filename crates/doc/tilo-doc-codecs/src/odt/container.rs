//! Minimal `.odt` packages for the codec's own tests, without a `LibreOffice`
//! dependency.

use crate::package::ZipPackage;

/// Build a minimal valid `.odt` body for self-contained tests (no `LibreOffice` dependency).
pub(crate) fn minimal_odt(body_xml: &str) -> Vec<u8> {
    minimal_odt_styled("", body_xml)
}

/// As [`minimal_odt`], with explicit `office:automatic-styles` content.
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
    let mut c = ZipPackage { parts: vec![] };
    c.set(
        "mimetype",
        b"application/vnd.oasis.opendocument.text".to_vec(),
    );
    c.set("content.xml", content.into_bytes());
    c.set("META-INF/manifest.xml", manifest.as_bytes().to_vec());
    c.write_odf().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_parts_roundtrip_and_replace_in_place() {
        // Phase `container_roundtrips_parts`.
        {
            let odt = minimal_odt("<text:p>hi</text:p>");
            let c = ZipPackage::read(&odt).unwrap();
            assert_eq!(c.parts[0].0, "mimetype", "mimetype must be the first entry");
            assert_eq!(
                c.get("mimetype").unwrap(),
                b"application/vnd.oasis.opendocument.text"
            );
            assert!(c.get("content.xml").is_some());

            let out = c.write_odf().unwrap();
            let c2 = ZipPackage::read(&out).unwrap();
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
            let mut c = ZipPackage { parts: vec![] };
            c.set("a", b"1".to_vec());
            c.set("b", b"2".to_vec());
            c.set("a", b"3".to_vec());
            assert_eq!(c.parts.len(), 2);
            assert_eq!(c.get("a").unwrap(), b"3");
            assert_eq!(c.parts[0].0, "a");
        }
    }
}
