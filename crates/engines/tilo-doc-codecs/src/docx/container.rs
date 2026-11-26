//! Minimal `.docx` packages for the codec's own tests: `[Content_Types].xml`,
//! the relationships, `word/document.xml`, `word/styles.xml` and optionally
//! `word/numbering.xml`.

use crate::package::ZipPackage;

pub(crate) fn document_xml(body_inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body_inner}</w:body></w:document>"#
    )
}

pub(crate) fn minimal_docx(body_inner: &str) -> Vec<u8> {
    build_docx("", "", body_inner)
}

pub(crate) fn minimal_docx_styled(styles_inner: &str, body_inner: &str) -> Vec<u8> {
    build_docx(styles_inner, "", body_inner)
}

pub(crate) fn minimal_docx_numbered(
    styles_inner: &str,
    numbering_inner: &str,
    body_inner: &str,
) -> Vec<u8> {
    build_docx(styles_inner, numbering_inner, body_inner)
}

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

    let mut c = ZipPackage { parts: vec![] };
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
    c.write_opc().unwrap()
}
