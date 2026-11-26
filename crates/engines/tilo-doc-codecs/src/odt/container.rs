//! Minimal `.odt` packages for the codec's own tests, without a `LibreOffice`
//! dependency.

use crate::package::ZipPackage;

pub(crate) fn minimal_odt(body_xml: &str) -> Vec<u8> {
    minimal_odt_styled("", body_xml)
}

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
