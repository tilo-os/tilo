//! Entity and character references in `.docx`/`.odt` text survive import and
//! a re-export. `quick-xml` reports each `&amp;`/`&#x2019;` as an event of its
//! own between the text around it, so a reader that only collects text events
//! loses exactly these characters: "R&D" comes back as "RD".

use std::io::{Cursor, Read, Write};
use tilo_doc::{DocSnapshot, Document};
use tilo_doc_codecs::{DocxCodec, FormatProvider, OdtCodec};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Every predefined entity, decimal and hex character references (one outside
/// the BMP) and references directly next to each other and to text.
const RAW: &str =
    "R&amp;D &lt;b&gt; &quot;q&quot; &apos;s&apos; &#8217;&#x2019;&#x1F600;&#65;&amp;&amp;";
const TEXT: &str = "R&D <b> \"q\" 's' \u{2019}\u{2019}\u{1F600}A&&";

/// Unmodeled metadata is carried as XML; its references must stay escaped.
const RAW_UNMODELED: &str = "R&amp;D &lt;x&gt; &#x1F600;";

fn zip_of(parts: &[(&str, String)]) -> Vec<u8> {
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, body) in parts {
        let method = if *name == "mimetype" {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        zw.start_file(*name, opts.compression_method(method))
            .unwrap();
        zw.write_all(body.as_bytes()).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

fn part(bytes: &[u8], name: &str) -> String {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    out
}

fn import(codec: &dyn FormatProvider, bytes: &[u8]) -> Document {
    let mut d = Document::new();
    codec
        .import(
            bytes,
            &mut d.as_mut(),
            &mut tilo_content::ContentInputs::new(),
        )
        .unwrap();
    d
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

fn docx_with_references() -> Vec<u8> {
    let para =
        |text: &str| format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#);
    let body = format!(
        concat!(
            "{p}",
            r#"<w:p><w:r><w:t>anchor</w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t>x</w:t></w:r><w:commentRangeEnd w:id="0"/>"#,
            r#"<w:r><w:commentReference w:id="0"/></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#,
            "<w:tbl><w:tr><w:tc>{p}</w:tc></w:tr></w:tbl>",
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rIdQdocHdr"/><w:footerReference w:type="default" r:id="rIdQdocFtr"/></w:sectPr>"#,
        ),
        p = para(RAW),
    );
    let decl = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
    zip_of(&[
        (
            "[Content_Types].xml",
            format!(
                r#"{decl}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#
            ),
        ),
        (
            "_rels/.rels",
            format!(
                r#"{decl}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#
            ),
        ),
        (
            "word/_rels/document.xml.rels",
            format!(
                r#"{decl}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdQdocHdr" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rIdQdocFtr" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#
            ),
        ),
        (
            "word/document.xml",
            format!("{decl}<w:document {W}><w:body>{body}</w:body></w:document>"),
        ),
        (
            "word/header1.xml",
            format!("{decl}<w:hdr {W}>{}</w:hdr>", para(RAW)),
        ),
        (
            "word/footer1.xml",
            format!("{decl}<w:ftr {W}>{}</w:ftr>", para(RAW)),
        ),
        (
            "word/footnotes.xml",
            format!(
                r#"{decl}<w:footnotes {W}><w:footnote w:id="1">{}</w:footnote></w:footnotes>"#,
                para(RAW)
            ),
        ),
        (
            "word/comments.xml",
            format!(
                r#"{decl}<w:comments {W}><w:comment w:id="0" w:author="{RAW}" w:date="2024-01-01T00:00:00Z">{}</w:comment></w:comments>"#,
                para(RAW)
            ),
        ),
        (
            "docProps/core.xml",
            format!(
                r#"{decl}<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{RAW}</dc:title><dc:creator>{RAW}</dc:creator><dc:subject>{RAW}</dc:subject><cp:lastModifiedBy>{RAW_UNMODELED}</cp:lastModifiedBy></cp:coreProperties>"#
            ),
        ),
        (
            "docProps/app.xml",
            format!(
                r#"{decl}<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>{RAW}</Application></Properties>"#
            ),
        ),
    ])
}

const ODF_NS: &str = concat!(
    r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" "#,
    r#"xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" "#,
    r#"xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" "#,
    r#"xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" "#,
    r#"xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" "#,
    r#"xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" "#,
    r#"xmlns:dc="http://purl.org/dc/elements/1.1/""#,
);

fn odt_with_references() -> Vec<u8> {
    let content = format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {ns} office:version="1.3"><office:body><office:text>"#,
            r#"<text:tracked-changes><text:changed-region xml:id="ct1"><text:insertion><office:change-info><dc:creator>{raw}</dc:creator><dc:date>2024-01-01T00:00:00</dc:date></office:change-info></text:insertion></text:changed-region></text:tracked-changes>"#,
            "<text:p>{raw}</text:p>",
            r#"<text:p>anchor<office:annotation office:name="c1"><dc:creator>{raw}</dc:creator><dc:date>2024-01-01T00:00:00</dc:date><text:p>{raw}</text:p></office:annotation>x<office:annotation-end office:name="c1"/>"#,
            r#"<text:note text:id="n1" text:note-class="footnote"><text:note-citation>1</text:note-citation><text:note-body><text:p>{raw}</text:p></text:note-body></text:note></text:p>"#,
            r#"<text:p><text:change-start text:change-id="ct1"/>ins<text:change-end text:change-id="ct1"/></text:p>"#,
            r#"<table:table table:name="T"><table:table-column/><table:table-row><table:table-cell><text:p>{raw}</text:p></table:table-cell></table:table-row></table:table>"#,
            "</office:text></office:body></office:document-content>",
        ),
        ns = ODF_NS,
        raw = RAW,
    );
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-styles {ODF_NS} office:version="1.3"><office:master-styles><style:master-page style:name="Standard"><style:header><text:p>{RAW}</text:p></style:header><style:footer><text:p>{RAW}</text:p></style:footer></style:master-page></office:master-styles></office:document-styles>"#
    );
    let meta = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta {ODF_NS} office:version="1.3"><office:meta><dc:title>{RAW}</dc:title><dc:creator>{RAW}</dc:creator><meta:generator>{RAW}</meta:generator><meta:user-defined meta:name="note">{RAW_UNMODELED}</meta:user-defined></office:meta></office:document-meta>"#
    );
    let manifest = r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/></manifest:manifest>"#;
    zip_of(&[
        ("mimetype", "application/vnd.oasis.opendocument.text".into()),
        ("content.xml", content),
        ("styles.xml", styles),
        ("meta.xml", meta),
        ("META-INF/manifest.xml", manifest.into()),
    ])
}

/// The body paragraph and table cell, header, footer, footnote, comment and
/// metadata each hold exactly [`TEXT`].
fn assert_references_resolved(snap: &DocSnapshot, format: &str) {
    let texts = |kind: &str| -> Vec<&str> {
        snap.blocks
            .iter()
            .filter(|b| b.kind == kind)
            .map(|b| b.text.as_str())
            .collect()
    };
    assert_eq!(
        texts("paragraph").first(),
        Some(&TEXT),
        "{format}: body paragraph"
    );
    assert_eq!(texts("cell"), [TEXT], "{format}: table cell");
    let region = |blocks: &[tilo_doc::BlockView]| -> Vec<String> {
        blocks.iter().map(|b| b.text.clone()).collect()
    };
    assert_eq!(region(&snap.header), [TEXT], "{format}: header");
    assert_eq!(region(&snap.footer), [TEXT], "{format}: footer");
    assert_eq!(snap.notes.len(), 1, "{format}: one footnote");
    assert_eq!(region(&snap.notes[0].blocks), [TEXT], "{format}: footnote");
    assert_eq!(snap.comments.len(), 1, "{format}: one comment");
    assert_eq!(
        region(&snap.comments[0].blocks),
        [TEXT],
        "{format}: comment"
    );
    assert_eq!(snap.comments[0].author, TEXT, "{format}: comment author");
    for key in ["title", "author", "generator"] {
        assert_eq!(
            snap.properties.get(key).map(String::as_str),
            Some(TEXT),
            "{format}: {key} property"
        );
    }
}

#[test]
fn office_text_keeps_entity_references_and_drops_undeclared_ones() {
    // Phase `docx_keeps_entity_and_character_references_in_text`.
    {
        let d = import(&DocxCodec, &docx_with_references());
        let snap = d.materialize();
        assert_references_resolved(&snap, "docx");
        assert_eq!(
            snap.properties.get("subject").map(String::as_str),
            Some(TEXT)
        );

        let exported = DocxCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        assert!(
            part(&exported, "docProps/core.xml").contains(&format!(
                "<cp:lastModifiedBy>{RAW_UNMODELED}</cp:lastModifiedBy>"
            )),
            "unmodeled core property keeps its references: {}",
            part(&exported, "docProps/core.xml")
        );
        let reopened = import(&DocxCodec, &exported).materialize();
        assert_references_resolved(&reopened, "docx re-export");
    }
    // Phase `odt_keeps_entity_and_character_references_in_text`.
    {
        let d = import(&OdtCodec, &odt_with_references());
        let snap = d.materialize();
        assert_references_resolved(&snap, "odt");
        assert_eq!(snap.changes.len(), 1, "one tracked change");
        assert_eq!(snap.changes[0].author, TEXT, "tracked-change author");

        let exported = OdtCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        assert!(
            part(&exported, "meta.xml").contains(&format!(
                r#"<meta:user-defined meta:name="note">{RAW_UNMODELED}</meta:user-defined>"#
            )),
            "unmodeled meta field keeps its references: {}",
            part(&exported, "meta.xml")
        );
        let reopened = import(&OdtCodec, &exported).materialize();
        assert_references_resolved(&reopened, "odt re-export");
        assert_eq!(reopened.changes[0].author, TEXT, "tracked-change author");
    }
    // Phase `undeclared_named_entities_are_dropped_without_losing_neighbours`.
    // A named entity no DTD declares cannot occur in a well-formed part (a DTD is
    // refused before parsing), so it stands for no character: the readers drop
    // the reference and keep the text around it, as the spreadsheet readers do.
    {
        let docx = zip_of(&[(
            "word/document.xml",
            format!(
                "<w:document {W}><w:body><w:p><w:r><w:t>a&bogus;b&amp;c</w:t></w:r></w:p></w:body></w:document>"
            ),
        )]);
        let snap = import(&DocxCodec, &docx).materialize();
        assert_eq!(snap.blocks[0].text, "ab&c");

        let odt = zip_of(&[
            ("mimetype", "application/vnd.oasis.opendocument.text".into()),
            (
                "content.xml",
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {ODF_NS}><office:body><office:text><text:p>a&bogus;b&amp;c</text:p></office:text></office:body></office:document-content>"#
                ),
            ),
        ]);
        let snap = import(&OdtCodec, &odt).materialize();
        assert_eq!(snap.blocks[0].text, "ab&c");
    }
}
