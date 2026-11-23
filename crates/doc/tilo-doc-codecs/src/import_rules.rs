//! What each reader makes of a package an editor wrote by hand: every rule the
//! `.odt` and `.docx` importers hold, asserted once per format over the same
//! minimal packages (a body, optional styles and numbering, and an image part
//! where a picture is involved).

use crate::package::ZipPackage;
use crate::{DocxCodec, FormatProvider, OdtCodec};
use tilo_content::{ContentInputs, NoContent};
use tilo_doc::document::MarkValue;
use tilo_doc::schema::FIELD_PLACEHOLDER;
use tilo_doc::{AttrVal, Document};

use crate::docx::container::{minimal_docx, minimal_docx_numbered, minimal_docx_styled};
use crate::odt::container::{minimal_odt, minimal_odt_styled};
use crate::preserve::{MARK_DOCX_XML, MARK_ODT_XML};

const TINY_PNG: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xf7, 0x03, 0x41, 0x43, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

fn read_odt(bytes: &[u8]) -> Document {
    let mut document = Document::new();
    OdtCodec
        .import(bytes, &mut document.as_mut(), &mut ContentInputs::new())
        .unwrap();
    document
}

fn read_docx(bytes: &[u8]) -> Document {
    let mut document = Document::new();
    DocxCodec
        .import(bytes, &mut document.as_mut(), &mut ContentInputs::new())
        .unwrap();
    document
}

fn block_with<'a>(
    snapshot: &'a tilo_doc::DocSnapshot,
    text: &str,
) -> &'a tilo_doc::snapshot::BlockView {
    snapshot
        .blocks
        .iter()
        .find(|b| b.text == text)
        .unwrap_or_else(|| panic!("no block {text:?} in {snapshot:?}"))
}

fn content_xml(bytes: &[u8], part: &str) -> String {
    String::from_utf8(ZipPackage::read(bytes).unwrap().get(part).unwrap().to_vec()).unwrap()
}

#[allow(
    clippy::too_many_lines,
    reason = "one format's import rules over the packages an editor writes"
)]
#[test]
fn a_hand_built_odt_reads_into_the_model_by_its_own_rules() {
    // A heading by outline level, a plain paragraph and an unstyled (bulleted) list.
    let plain = read_odt(&minimal_odt(concat!(
        r#"<text:h text:outline-level="2">Title</text:h>"#,
        r#"<text:p>Hello</text:p>"#,
        r#"<text:list><text:list-item><text:p>one</text:p></text:list-item>"#,
        r#"<text:list-item><text:p>two</text:p></text:list-item></text:list>"#,
    )));
    let snapshot = plain.materialize();
    assert_eq!(snapshot.blocks[0].kind, "heading");
    assert_eq!(snapshot.blocks[0].text, "Title");
    assert_eq!(
        snapshot.blocks[0].attrs.get("level"),
        Some(&AttrVal::Int(2))
    );
    assert_eq!(snapshot.blocks[1].kind, "paragraph");
    assert_eq!(snapshot.blocks[1].text, "Hello");
    assert_eq!(
        snapshot
            .blocks
            .iter()
            .filter(|b| b.kind == "list_item")
            .count(),
        2
    );
    assert_eq!(snapshot.blocks[2].text, "one");
    assert_eq!(snapshot.blocks[3].text, "two");

    // Named paragraph styles, their inherited parent styles, the line-break, tab and
    // `text:s` forms, an ordered list style, character styles, a link (also around a
    // character style), a colour, and a run's language.
    let styles_xml = concat!(
        r#"<style:style style:name="Heading_20_1" style:display-name="Heading 1" style:family="paragraph" style:default-outline-level="1"/>"#,
        r#"<style:style style:name="MySub" style:family="paragraph" style:parent-style-name="Heading_20_1"/>"#,
        r#"<text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style>"#,
        r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#,
        r#"<style:style style:name="T2" style:family="text"><style:text-properties fo:font-style="italic"/></style:style>"#,
        r#"<style:style style:name="P2" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>"#,
        r#"<style:style style:name="T3" style:family="text"><style:text-properties fo:font-size="20pt" style:font-name="Liberation Serif"/></style:style>"#,
        r##"<style:style style:name="C1" style:family="text"><style:text-properties fo:color="#FF0000"/></style:style>"##,
        r#"<style:style style:name="Lf" style:family="text"><style:text-properties fo:language="fr" fo:country="FR"/></style:style>"#,
    );
    let named = read_odt(&minimal_odt_styled(
        styles_xml,
        concat!(
            r#"<text:p text:style-name="Heading_20_1">My Title</text:p>"#,
            r#"<text:p text:style-name="MySub">A Subtitle</text:p>"#,
            r#"<text:p>Line one<text:line-break/>line two<text:tab/>after a tab and<text:s text:c="3"/>spaces.</text:p>"#,
            r#"<text:list text:style-name="L1"><text:list-item><text:p>first</text:p></text:list-item><text:list-item><text:p>second</text:p></text:list-item></text:list>"#,
            r#"<text:p>a <text:span text:style-name="T1">bold</text:span> and <text:span text:style-name="T2">em</text:span></text:p>"#,
            r#"<text:p text:style-name="P2"><text:span text:style-name="T3">big</text:span></text:p>"#,
            r#"<text:p>see <text:a xlink:href="https://tilo.so/x">here</text:a>!</text:p>"#,
            r#"<text:p><text:a xlink:href="http://x/"><text:span text:style-name="T1">bold</text:span></text:a></text:p>"#,
            r#"<text:p>a <text:span text:style-name="C1">red</text:span></text:p>"#,
            r#"<text:p>a <text:span text:style-name="Lf">bonjour</text:span></text:p>"#,
        ),
    ));
    let snapshot = named.materialize();
    assert_eq!(
        snapshot.blocks[0].kind, "heading",
        "named paragraph style → heading: {snapshot:?}"
    );
    assert_eq!(snapshot.blocks[0].text, "My Title");
    assert_eq!(
        snapshot.blocks[0].attrs.get("level"),
        Some(&AttrVal::Int(1))
    );
    assert_eq!(
        snapshot.blocks[1].kind, "heading",
        "heading via inherited parent style"
    );
    let broken = block_with(&snapshot, "Line one\nline two\tafter a tab and   spaces.");
    assert_eq!(
        broken.kind, "paragraph",
        "line-break, tab and text:s: {snapshot:?}"
    );
    let items: Vec<_> = snapshot
        .blocks
        .iter()
        .filter(|b| ["first", "second"].contains(&b.text.as_str()))
        .collect();
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0].attrs.get("ordered"),
        Some(&AttrVal::Bool(true)),
        "numbered list-style → ordered"
    );
    let runs = block_with(&snapshot, "a bold and em");
    assert!(
        runs.marks
            .iter()
            .any(|m| m.name == "strong" && m.start == 2 && m.end == 6),
        "strong over 'bold', got {:?}",
        runs.marks
    );
    assert!(
        runs.marks
            .iter()
            .any(|m| m.name == "em" && m.start == 11 && m.end == 13),
        "em over 'em', got {:?}",
        runs.marks
    );
    let big = block_with(&snapshot, "big");
    assert_eq!(
        big.attrs.get("align"),
        Some(&AttrVal::Str("center".into())),
        "{:?}",
        big.attrs
    );
    assert!(
        big.marks
            .iter()
            .any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)),
        "{:?}",
        big.marks
    );
    assert!(
        big.marks
            .iter()
            .any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())),
        "{:?}",
        big.marks
    );
    let linked = block_with(&snapshot, "see here!");
    let link = linked
        .marks
        .iter()
        .find(|m| m.name == "link")
        .expect("link mark present");
    assert_eq!((link.start, link.end), (4, 8), "{:?}", linked.marks);
    assert_eq!(link.value, MarkValue::Str("https://tilo.so/x".into()));
    let nested = block_with(&snapshot, "bold");
    assert!(
        nested
            .marks
            .iter()
            .any(|m| m.name == "link" && m.value == MarkValue::Str("http://x/".into())),
        "{:?}",
        nested.marks
    );
    assert!(
        nested.marks.iter().any(|m| m.name == "strong"),
        "{:?}",
        nested.marks
    );
    let coloured = block_with(&snapshot, "a red");
    assert!(
        coloured.marks.iter().any(|m| m.name == "color"
            && m.start == 2
            && m.end == 5
            && m.value == MarkValue::Str("#ff0000".into())),
        "colour normalized to lower case and ranged: {:?}",
        coloured.marks
    );
    let french = block_with(&snapshot, "a bonjour");
    assert!(
        french.marks.iter().any(|m| m.name == "lang"
            && m.start == 2
            && m.end == 9
            && m.value == MarkValue::Str("fr-FR".into())),
        "lang reassembled from fo:language and fo:country: {:?}",
        french.marks
    );

    // Nested lists keep their depth with no indentation whitespace in the text, and
    // tables read as cells in order, with a paragraph style's alignment on its cell.
    let tables = read_odt(&minimal_odt_styled(
        r#"<style:style style:name="Pc" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>"#,
        concat!(
            "<text:list>\n  <text:list-item>\n    <text:p>a</text:p>\n  </text:list-item>\n",
            "  <text:list-item>\n    <text:p>b</text:p>\n",
            "<text:list><text:list-item><text:p>nested</text:p></text:list-item></text:list>\n",
            "  </text:list-item>\n</text:list>",
            r#"<table:table table:name="T1">"#,
            r#"<table:table-row><table:table-cell><text:p>c</text:p></table:table-cell>"#,
            r#"<table:table-cell><text:p>d</text:p></table:table-cell></table:table-row>"#,
            r#"<table:table-row><table:table-cell><text:p>e</text:p></table:table-cell>"#,
            r#"<table:table-cell><text:p>f</text:p></table:table-cell></table:table-row>"#,
            r#"</table:table>"#,
            r#"<table:table table:name="T2"><table:table-row>"#,
            r#"<table:table-cell><text:p text:style-name="Pc">mid</text:p></table:table-cell>"#,
            r#"<table:table-cell><text:p>plain</text:p></table:table-cell>"#,
            r#"</table:table-row></table:table>"#,
        ),
    ));
    let snapshot = tables.materialize();
    let level = |b: &tilo_doc::snapshot::BlockView| match b.attrs.get("level") {
        Some(AttrVal::Int(n)) => *n,
        _ => 0,
    };
    let items: Vec<_> = snapshot
        .blocks
        .iter()
        .filter(|b| ["a", "b", "nested"].contains(&b.text.as_str()))
        .collect();
    assert_eq!(
        items
            .iter()
            .map(|b| (b.text.clone(), level(b)))
            .collect::<Vec<_>>(),
        vec![
            ("a".to_string(), 0i64),
            ("b".to_string(), 0),
            ("nested".to_string(), 1)
        ],
        "nesting depth captured, no indentation whitespace: {items:?}"
    );
    assert_eq!(
        items[0].attrs.get("level"),
        None,
        "a top-level item has no level attr"
    );
    let cells: Vec<_> = snapshot
        .blocks
        .iter()
        .filter(|b| ["c", "d", "e", "f"].contains(&b.text.as_str()))
        .collect();
    assert_eq!(
        cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(),
        vec!["c", "d", "e", "f"],
        "2x2 cells in order"
    );
    assert_eq!(
        cells[0].attrs.get("table"),
        Some(&AttrVal::Str("T1".into()))
    );
    assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
    assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));
    assert_eq!(
        block_with(&snapshot, "mid").attrs.get("align"),
        Some(&AttrVal::Str("center".into())),
        "a cell takes its paragraph style's alignment"
    );
    assert_eq!(
        block_with(&snapshot, "plain").attrs.get("align"),
        None,
        "a plain cell has no align"
    );
    let exported = OdtCodec.export(tables.as_ref(), &NoContent).unwrap();
    let reread = read_odt(&exported);
    let cells_again: Vec<_> = reread
        .materialize()
        .blocks
        .iter()
        .filter(|b| b.kind == "cell")
        .map(|b| (b.text.clone(), b.attrs.get("align").cloned()))
        .collect();
    assert!(
        cells_again.contains(&("c".to_string(), None))
            && cells_again.contains(&("mid".to_string(), Some(AttrVal::Str("center".into())))),
        "cells and their alignment survive a save: {cells_again:?}"
    );

    // Inline pictures: an image-only wrapper paragraph goes, the frame's point width
    // becomes pixels, the title is the alt text with its references resolved, and
    // text before a frame stays on its own paragraph.
    let mut package = ZipPackage::read(&minimal_odt_styled(
        "",
        concat!(
            r#"<text:p>before</text:p>"#,
            r#"<text:p text:style-name="Fig">"#,
            r#"<draw:frame svg:width="200pt" svg:height="110pt"><draw:image xlink:href="Pictures/0.png"/></draw:frame>"#,
            r#"</text:p>"#,
            r#"<text:p>after</text:p>"#,
            r#"<text:p><draw:frame><draw:image xlink:href="Pictures/0.png"/>"#,
            "<svg:title>R&amp;D &lt;chart&gt; &#x1F600;</svg:title></draw:frame></text:p>",
            r#"<text:p>see <draw:frame><draw:image xlink:href="Pictures/0.png"/></draw:frame></text:p>"#,
        ),
    ))
    .unwrap();
    package.set("Pictures/0.png", TINY_PNG.to_vec());
    let mut pictures = Document::new();
    let mut inputs = ContentInputs::new();
    OdtCodec
        .import(
            &package.write_odf().unwrap(),
            &mut pictures.as_mut(),
            &mut inputs,
        )
        .unwrap();
    let snapshot = pictures.materialize();
    assert_eq!(
        snapshot
            .blocks
            .iter()
            .map(|b| b.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "paragraph",
            "image",
            "paragraph",
            "image",
            "paragraph",
            "image"
        ],
        "an image-only wrapper paragraph is dropped: {snapshot:?}"
    );
    assert_eq!(snapshot.blocks[0].text, "before");
    assert_eq!(snapshot.blocks[2].text, "after");
    let hash = match snapshot.blocks[1].attrs.get("blob") {
        Some(AttrVal::Str(h)) => h.clone(),
        other => panic!("image missing blob attr: {other:?}"),
    };
    assert_eq!(
        inputs.get(&hash),
        Some(&TINY_PNG[..]),
        "image bytes handed to the caller"
    );
    assert_eq!(
        snapshot.blocks[1].attrs.get("width"),
        Some(&AttrVal::Int(267)),
        "200pt → 267px"
    );
    assert_eq!(
        snapshot.blocks[3].attrs.get("alt"),
        Some(&AttrVal::Str("R&D <chart> \u{1F600}".into())),
        "the title is the alt text with references resolved"
    );
    assert_eq!(
        snapshot.blocks[4].text, "see ",
        "leading text stays on its paragraph: {snapshot:?}"
    );

    // A drawing the model has no shape for is kept verbatim, and comes back verbatim.
    let fidelity = read_odt(&minimal_odt(concat!(
        r#"<text:p>before</text:p>"#,
        r#"<draw:frame draw:name="F"><draw:image xlink:href="pic.png"/></draw:frame>"#,
        r#"<text:p>after</text:p>"#,
    )));
    let snapshot = fidelity.materialize();
    assert_eq!(
        snapshot
            .blocks
            .iter()
            .map(|b| b.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["paragraph", "fidelity", "paragraph"],
        "{snapshot:?}"
    );
    let raw = match snapshot.blocks[1].attrs.get("xml") {
        Some(AttrVal::Str(x)) => x.clone(),
        other => panic!("fidelity atom missing xml attr: {other:?}"),
    };
    assert!(
        raw.starts_with("<draw:frame") && raw.contains("pic.png") && raw.ends_with("</draw:frame>"),
        "captured raw: {raw}"
    );
    let exported = OdtCodec.export(fidelity.as_ref(), &NoContent).unwrap();
    let again = read_odt(&exported).materialize();
    let raw_again = match again.blocks[1].attrs.get("xml") {
        Some(AttrVal::Str(x)) => x.clone(),
        other => panic!("fidelity atom missing after round-trip: {other:?}"),
    };
    assert_eq!(raw, raw_again, "fidelity atom round-trips verbatim");

    // One paragraph holding a field, a text box, a shape and the spacing forms the
    // model keeps inline: the kept elements read as placeholders in place, and an
    // edit to one of them writes text where it stood.
    let field = r#"<text:page-number text:select-page="current">3</text:page-number>"#;
    let shape = r#"<draw:custom-shape text:anchor-type="paragraph" svg:width="1cm" svg:height="1cm"><draw:enhanced-geometry draw:type="star5"/></draw:custom-shape>"#;
    let text_box = r#"<draw:frame text:anchor-type="paragraph" svg:width="3cm"><draw:text-box><text:p>boxed</text:p></draw:text-box></draw:frame>"#;
    let mut mixed = read_odt(&minimal_odt(&format!(
        r#"<text:p>{shape}Page {field} of<text:s text:c="2"/>two{text_box} after<text:tab/>tab</text:p>"#
    )));
    let snapshot = mixed.materialize();
    assert_eq!(snapshot.blocks.len(), 1, "{snapshot:?}");
    let paragraph = &snapshot.blocks[0];
    assert_eq!(
        paragraph.text,
        format!("{FIELD_PLACEHOLDER}Page 3 of  two{FIELD_PLACEHOLDER} after\ttab")
    );
    assert_eq!(
        paragraph
            .marks
            .iter()
            .filter(|m| m.name == MARK_ODT_XML)
            .count(),
        3
    );
    let id = paragraph.id.clone();
    mixed.splice_text(&id, paragraph.text.chars().count(), 0, "!");
    let out = OdtCodec.export(mixed.as_ref(), &NoContent).unwrap();
    let xml = content_xml(&out, "content.xml");
    let at = |needle: &str| {
        xml.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {xml}"))
    };
    assert!(at(shape) < at("Page ") && at("Page ") < at(field) && at(field) < at(text_box));
    assert!(xml.contains("of <text:s/>two"), "{xml}");
    assert!(xml.contains("after<text:tab/>tab!"), "{xml}");
    assert_eq!(
        read_odt(&out).materialize().blocks[0].text,
        format!("{FIELD_PLACEHOLDER}Page 3 of  two{FIELD_PLACEHOLDER} after\ttab!")
    );
    let after_field = "_Page 3".chars().count();
    mixed.splice_text(&id, after_field, 0, "x");
    let xml = content_xml(
        &OdtCodec.export(mixed.as_ref(), &NoContent).unwrap(),
        "content.xml",
    );
    assert!(
        xml.contains(&format!("{field}x of")),
        "text typed right after the field follows it: {xml}"
    );
    mixed.splice_text(&id, after_field.saturating_sub(1), 2, "4");
    let xml = content_xml(
        &OdtCodec.export(mixed.as_ref(), &NoContent).unwrap(),
        "content.xml",
    );
    assert!(
        !xml.contains("text:page-number") && xml.contains("Page 4 of"),
        "replacing the field's text writes the new text instead of the field: {xml}"
    );

    // A document's default language and a run's own language both survive a save.
    let mut language = Document::new();
    language.set_language("de-DE");
    let id = language.insert_block(None, "paragraph");
    language.splice_text(&id, 0, 0, "bonjour monde");
    language.add_mark(&id, 0, 7, "lang", "fr");
    let exported = OdtCodec.export(language.as_ref(), &NoContent).unwrap();
    let snapshot = read_odt(&exported).materialize();
    assert_eq!(
        snapshot.language.as_deref(),
        Some("de-DE"),
        "document default language survives: {:?}",
        snapshot.language
    );
    assert!(
        snapshot.blocks[0].marks.iter().any(|m| m.name == "lang"
            && m.start == 0
            && m.end == 7
            && m.value == MarkValue::Str("fr".into())),
        "run lang mark `fr` survives over 'bonjour': {:?}",
        snapshot.blocks[0].marks
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "one format's import rules over the packages an editor writes"
)]
#[test]
fn a_hand_built_docx_reads_into_the_model_by_its_own_rules() {
    // A paragraph style that names a heading, one that inherits it, a numbered and a
    // bulleted list, and a table.
    let styles_xml = concat!(
        r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/><w:jc w:val="center"/></w:pPr></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="MyTitle"><w:basedOn w:val="Heading1"/></w:style>"#,
    );
    let numbering = concat!(
        r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>"#,
        r#"<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>"#,
        r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        r#"<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#,
    );
    let structured = read_docx(&minimal_docx_numbered(
        styles_xml,
        numbering,
        concat!(
            r#"<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:pStyle w:val="MyTitle"/></w:pPr><w:r><w:t>My Title</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>first</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>bee</w:t></w:r></w:p>"#,
            r#"<w:tbl>"#,
            r#"<w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>"#,
            r#"<w:tr><w:tc><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>d</w:t></w:r></w:p></w:tc></w:tr>"#,
            r#"</w:tbl>"#,
        ),
    ));
    let snapshot = structured.materialize();
    let heading = block_with(&snapshot, "Title");
    assert_eq!(heading.kind, "heading");
    assert_eq!(heading.attrs.get("level"), Some(&AttrVal::Int(2)));
    assert_eq!(block_with(&snapshot, "Hello").kind, "paragraph");
    let inherited = block_with(&snapshot, "My Title");
    assert_eq!(
        inherited.kind, "heading",
        "named paragraph style → heading: {snapshot:?}"
    );
    assert_eq!(
        inherited.attrs.get("level"),
        Some(&AttrVal::Int(1)),
        "level inherited via basedOn"
    );
    assert_eq!(
        inherited.attrs.get("align"),
        Some(&AttrVal::Str("center".into())),
        "alignment from the style"
    );
    let items: Vec<_> = snapshot
        .blocks
        .iter()
        .filter(|b| ["first", "bee"].contains(&b.text.as_str()))
        .collect();
    assert_eq!(items.len(), 2, "{snapshot:?}");
    assert_eq!(
        items[0].attrs.get("ordered"),
        Some(&AttrVal::Bool(true)),
        "decimal numId → ordered"
    );
    assert_eq!(
        items[1].attrs.get("ordered"),
        Some(&AttrVal::Bool(false)),
        "bullet numId → unordered"
    );
    let cells: Vec<_> = snapshot
        .blocks
        .iter()
        .filter(|b| ["a", "b", "c", "d"].contains(&b.text.as_str()))
        .collect();
    assert_eq!(
        cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(),
        vec!["a", "b", "c", "d"],
        "2x2 cells in order"
    );
    assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
    assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));

    // Run formatting, paragraph alignment and size, a named character style, and the
    // break and tab forms.
    let runs = read_docx(&minimal_docx_styled(
        r#"<w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>"#,
        concat!(
            r#"<w:p>"#,
            r#"<w:r><w:t xml:space="preserve">a </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r>"#,
            r#"<w:r><w:t xml:space="preserve"> and </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:i/><w:u w:val="single"/><w:strike/></w:rPr><w:t>more</w:t></w:r>"#,
            r#"</w:p>"#,
            r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr>"#,
            r#"<w:r><w:rPr><w:sz w:val="40"/><w:rFonts w:ascii="Times New Roman"/></w:rPr><w:t>big</w:t></w:r>"#,
            r#"</w:p>"#,
            r#"<w:p><w:r><w:t>x</w:t></w:r>"#,
            r#"<w:r><w:rPr><w:rStyle w:val="Strong"/></w:rPr><w:t>y</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>one</w:t><w:br/><w:t>two</w:t><w:tab/><w:t>three</w:t></w:r></w:p>"#,
        ),
    ));
    let snapshot = runs.materialize();
    let marked = block_with(&snapshot, "a bold and more");
    for (name, start, end) in [
        ("strong", 2, 6),
        ("em", 11, 15),
        ("underline", 11, 15),
        ("strike", 11, 15),
    ] {
        assert!(
            marked
                .marks
                .iter()
                .any(|m| m.name == name && m.start == start && m.end == end),
            "{name} {start}..{end}: {:?}",
            marked.marks
        );
    }
    let big = block_with(&snapshot, "big");
    assert_eq!(
        big.attrs.get("align"),
        Some(&AttrVal::Str("center".into())),
        "{:?}",
        big.attrs
    );
    assert!(
        big.marks
            .iter()
            .any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)),
        "{:?}",
        big.marks
    );
    assert!(
        big.marks
            .iter()
            .any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())),
        "{:?}",
        big.marks
    );
    let named = block_with(&snapshot, "xy");
    assert!(
        named
            .marks
            .iter()
            .any(|m| m.name == "strong" && m.start == 1 && m.end == 2),
        "named character style → mark: {:?}",
        named.marks
    );
    assert_eq!(
        block_with(&snapshot, "one\ntwo\tthree").text,
        "one\ntwo\tthree",
        "w:br → newline and w:tab → tab"
    );

    // A paragraph style is lifted into the document's styles, not flattened onto the
    // block: the block points at it, and every property converts to the model's unit.
    let lifted = read_docx(&minimal_docx_styled(
        concat!(
            r##"<w:style w:type="paragraph" w:styleId="Fancy"><w:name w:val="Fancy"/>"##,
            r##"<w:pPr><w:jc w:val="center"/><w:spacing w:after="180"/><w:ind w:left="360"/></w:pPr>"##,
            r##"<w:rPr><w:rFonts w:ascii="Times New Roman"/><w:b/><w:i/><w:color w:val="FF0000"/><w:sz w:val="40"/></w:rPr></w:style>"##,
        ),
        r#"<w:p><w:pPr><w:pStyle w:val="Fancy"/></w:pPr><w:r><w:t>hi</w:t></w:r></w:p>"#,
    ));
    let snapshot = lifted.materialize();
    let fancy = block_with(&snapshot, "hi");
    assert_eq!(fancy.kind, "paragraph");
    assert_eq!(
        fancy.attrs.get("style"),
        Some(&AttrVal::Str("Fancy".into())),
        "block points at the styleId: {:?}",
        fancy.attrs
    );
    assert_eq!(
        fancy.attrs.get("align"),
        None,
        "alignment comes from the style, not a direct attr: {:?}",
        fancy.attrs
    );
    assert!(
        fancy.marks.is_empty(),
        "style formatting not lifted as inline marks: {:?}",
        fancy.marks
    );
    let lifted_style = snapshot
        .styles
        .get("Fancy")
        .expect("style lifted into the document");
    assert_eq!(
        lifted_style.get("name"),
        Some(&AttrVal::Str("Fancy".into()))
    );
    assert_eq!(lifted_style.get("bold"), Some(&AttrVal::Bool(true)));
    assert_eq!(lifted_style.get("italic"), Some(&AttrVal::Bool(true)));
    assert_eq!(
        lifted_style.get("color"),
        Some(&AttrVal::Str("#ff0000".into())),
        "w:color FF0000 → #ff0000"
    );
    assert_eq!(
        lifted_style.get("align"),
        Some(&AttrVal::Str("center".into()))
    );
    assert_eq!(
        lifted_style.get("size"),
        Some(&AttrVal::Float(20.0)),
        "w:sz 40 half-points → 20pt"
    );
    assert_eq!(
        lifted_style.get("font"),
        Some(&AttrVal::Str("serif".into())),
        "Times New Roman → serif"
    );
    assert_eq!(
        lifted_style.get("space_after"),
        Some(&AttrVal::Float(12.0)),
        "180 twips → 12px"
    );
    assert_eq!(
        lifted_style.get("indent"),
        Some(&AttrVal::Float(24.0)),
        "360 twips → 24px"
    );

    // A run's language, and the document default from `docDefaults`, which a style's
    // own language must not override.
    let language = read_docx(&minimal_docx(concat!(
        r#"<w:p>"#,
        r#"<w:r><w:t xml:space="preserve">a </w:t></w:r>"#,
        r#"<w:r><w:rPr><w:lang w:val="fr"/></w:rPr><w:t>bonjour</w:t></w:r>"#,
        r#"</w:p>"#,
    )));
    assert!(
        language.materialize().blocks[0]
            .marks
            .iter()
            .any(|m| m.name == "lang"
                && m.start == 2
                && m.end == 9
                && m.value == MarkValue::Str("fr".into())),
        "lang mark over 'bonjour': {:?}",
        language.materialize().blocks[0].marks
    );
    let defaults = read_docx(&minimal_docx_styled(
        concat!(
            r#"<w:docDefaults><w:rPrDefault><w:rPr><w:lang w:val="de-DE"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
            r#"<w:style w:type="paragraph" w:styleId="Body"><w:rPr><w:lang w:val="en-US"/></w:rPr></w:style>"#,
        ),
        r"<w:p><w:r><w:t>hi</w:t></w:r></w:p>",
    ));
    assert_eq!(
        defaults.materialize().language.as_deref(),
        Some("de-DE"),
        "document default language from docDefaults"
    );
    let mut marked = Document::new();
    marked.set_language("de-DE");
    let id = marked.insert_block(None, "paragraph");
    marked.splice_text(&id, 0, 0, "bonjour monde");
    marked.add_mark(&id, 0, 7, "lang", "fr");
    let exported = DocxCodec.export(marked.as_ref(), &NoContent).unwrap();
    let snapshot = read_docx(&exported).materialize();
    assert_eq!(
        snapshot.language.as_deref(),
        Some("de-DE"),
        "document default language survives: {:?}",
        snapshot.language
    );
    assert!(
        snapshot.blocks[0].marks.iter().any(|m| m.name == "lang"
            && m.start == 0
            && m.end == 7
            && m.value == MarkValue::Str("fr".into())),
        "run lang mark `fr` survives over 'bonjour': {:?}",
        snapshot.blocks[0].marks
    );

    // A content control between paragraphs keeps its place as a fidelity block.
    let control = read_docx(&minimal_docx(concat!(
        r#"<w:p><w:r><w:t>before</w:t></w:r></w:p>"#,
        r#"<w:sdt><w:sdtPr><w:tag w:val="t"/></w:sdtPr><w:sdtContent><w:r><w:t>boxed</w:t></w:r></w:sdtContent></w:sdt>"#,
        r#"<w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
    )));
    let snapshot = control.materialize();
    assert_eq!(
        snapshot
            .blocks
            .iter()
            .map(|b| b.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["paragraph", "fidelity", "paragraph"],
        "{snapshot:?}"
    );
    assert_eq!(snapshot.blocks[0].text, "before");
    assert_eq!(snapshot.blocks[2].text, "after");
    let raw = match snapshot.blocks[1].attrs.get("xml") {
        Some(AttrVal::Str(x)) => x.clone(),
        other => panic!("fidelity atom missing xml attr: {other:?}"),
    };
    assert!(
        raw.starts_with("<w:sdt") && raw.contains("boxed") && raw.ends_with("</w:sdt>"),
        "captured raw: {raw}"
    );

    // One paragraph holding a complex field, a content control and a page break: the
    // kept elements read as placeholders in place, keep their order on export, and an
    // edit to one of them writes text where it stood.
    let field = concat!(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
        r#"<w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>"#,
        r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>7</w:t></w:r>"#,
        r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
    );
    let content_control = r#"<w:sdt><w:sdtPr><w:tag w:val="t"/></w:sdtPr><w:sdtContent><w:r><w:t>ctl</w:t></w:r></w:sdtContent></w:sdt>"#;
    let page_break = r#"<w:r><w:rPr><w:b/></w:rPr><w:br w:type="page"/></w:r>"#;
    let mut mixed = read_docx(&minimal_docx(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">page </w:t></w:r>{field}<w:r><w:t xml:space="preserve"> and </w:t></w:r>{content_control}{page_break}<w:r><w:t>end</w:t></w:r></w:p>"#
    )));
    let snapshot = mixed.materialize();
    let paragraph = &snapshot.blocks[0];
    assert_eq!(
        paragraph.text,
        format!("page 7 and ctl{FIELD_PLACEHOLDER}end")
    );
    assert_eq!(
        paragraph
            .marks
            .iter()
            .filter(|m| m.name == MARK_DOCX_XML)
            .count(),
        3,
        "{:?}",
        paragraph.marks
    );
    let id = paragraph.id.clone();
    mixed.splice_text(&id, 0, 0, "The ");
    let out = DocxCodec.export(mixed.as_ref(), &NoContent).unwrap();
    let xml = content_xml(&out, "word/document.xml");
    let at = |needle: &str| {
        xml.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {xml}"))
    };
    assert!(
        at("The page") < at(field)
            && at(field) < at(content_control)
            && at(content_control) < at(page_break)
    );
    assert!(at(page_break) < at(">end</w:t>"));
    assert_eq!(
        read_docx(&out).materialize().blocks[0].text,
        format!("The page 7 and ctl{FIELD_PLACEHOLDER}end")
    );
    let at_control = "The page 7 and ".chars().count();
    mixed.splice_text(&id, at_control, 3, "CTL");
    let xml = content_xml(
        &DocxCodec.export(mixed.as_ref(), &NoContent).unwrap(),
        "word/document.xml",
    );
    assert!(
        !xml.contains("<w:sdt>") && xml.contains(" and CTL</w:t>"),
        "editing a kept element's text writes the text instead of the element: {xml}"
    );
    assert!(xml.contains(field), "the unedited field stays: {xml}");

    // Text after an inline picture continues after it on the same paragraph.
    let picture = read_docx(&minimal_docx(concat!(
        r#"<w:p><w:r><w:t>before</w:t></w:r>"#,
        r#"<w:r><w:drawing><wp:inline><wp:extent cx="9525" cy="9525"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rIdMissing"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#,
        r#"<w:r><w:t>after</w:t></w:r></w:p>"#,
    )));
    let snapshot = picture.materialize();
    assert_eq!(snapshot.blocks.len(), 1, "{snapshot:?}");
    assert_eq!(
        snapshot.blocks[0].text,
        format!("before{FIELD_PLACEHOLDER}after"),
        "text after an inline picture continues after it"
    );
}
