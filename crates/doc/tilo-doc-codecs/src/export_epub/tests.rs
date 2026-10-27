//! Tests for the EPUB export engine.
//!
//! Snapshots are built directly (the `DocSnapshot`/`BlockView` structs are
//! public) so the tests exercise `render_epub` without coupling to qdoc's
//! mutation API. We assert on the **real** produced bytes: the archive is
//! unzipped in-memory with the `zip` crate and the structural EPUB invariants
//! are checked against the production output (mimetype-first/stored, container,
//! OPF metadata, nav TOC, image manifest items).

use super::*;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::io::Read;
use tilo_doc::snapshot::{AttrVal, BlockView, DocSnapshot};
use zip::CompressionMethod;
use zip::ZipArchive;

fn empty_snapshot() -> DocSnapshot {
    DocSnapshot {
        schema: "tilo.richtext".into(),
        schema_version: 1,
        ..Default::default()
    }
}

fn block(kind: &str, text: &str) -> BlockView {
    BlockView {
        id: format!("b-{kind}-{}", text.len()),
        kind: kind.into(),
        text: text.into(),
        marks: Vec::new(),
        attrs: BTreeMap::new(),
        children: Vec::new(),
    }
}

fn heading(level: i64, text: &str) -> BlockView {
    let mut b = block("heading", text);
    b.attrs.insert("level".into(), AttrVal::Int(level));
    b
}

fn image_block(blob_hash: &str, alt: &str) -> BlockView {
    let mut b = block("image", alt);
    b.attrs
        .insert("blob".into(), AttrVal::Str(blob_hash.into()));
    b
}

/// A 1x1 PNG (valid magic header; bytes beyond don't matter for our sniff/embed).
fn tiny_png() -> Vec<u8> {
    vec![
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
        0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D', b'R', // IHDR
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
    ]
}

/// Read every entry of the produced archive into `(name, bytes)`, preserving
/// the archive's stored order (so we can assert mimetype is first).
fn unzip(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut ar = ZipArchive::new(Cursor::new(bytes)).expect("output is a valid zip");
    let mut out = Vec::with_capacity(ar.len());
    for i in 0..ar.len() {
        let mut f = ar.by_index(i).expect("entry readable");
        let name = f.name().expect("entry name").into_owned();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).expect("entry bytes");
        out.push((name, buf));
    }
    out
}

fn part<'a>(parts: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    parts
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, b)| b.as_slice())
}

fn part_str(parts: &[(String, Vec<u8>)], name: &str) -> String {
    String::from_utf8(
        part(parts, name)
            .unwrap_or_else(|| panic!("missing part {name}"))
            .to_vec(),
    )
    .expect("utf-8 part")
}

/// The compression method of a named entry (to assert mimetype is Stored).
fn method_of(bytes: &[u8], name: &str) -> CompressionMethod {
    let mut ar = ZipArchive::new(Cursor::new(bytes)).unwrap();
    for i in 0..ar.len() {
        let f = ar.by_index(i).unwrap();
        if f.name().is_ok_and(|entry| entry == name) {
            return f.compression();
        }
    }
    panic!("entry {name} not found");
}

#[test]
fn epub_package_scaffolding_is_valid_even_when_empty() {
    // Phase `first_entry_is_mimetype_stored_with_exact_payload`.
    {
        let mut snap = empty_snapshot();
        snap.properties.insert("title".into(), "Hello".into());
        snap.blocks = vec![block("paragraph", "body")];

        let epub = render_epub(&snap, &tilo_content::NoContent).expect("render");
        let parts = unzip(&epub);

        // First entry MUST be `mimetype`.
        assert_eq!(
            parts[0].0, "mimetype",
            "first archive entry must be `mimetype`"
        );
        // Its payload MUST be exactly the EPUB media type.
        assert_eq!(
            parts[0].1, b"application/epub+zip",
            "mimetype payload must be the EPUB media type"
        );
        // And it MUST be stored (uncompressed).
        assert_eq!(
            method_of(&epub, "mimetype"),
            CompressionMethod::Stored,
            "mimetype must be stored uncompressed"
        );
    }
    // Phase `contains_container_pointing_at_opf`.
    {
        let mut snap = empty_snapshot();
        snap.properties.insert("title".into(), "Doc".into());

        let epub = render_epub(&snap, &tilo_content::NoContent).expect("render");
        let parts = unzip(&epub);

        let container = part_str(&parts, "META-INF/container.xml");
        assert!(
            container.contains("rootfile"),
            "container declares a rootfile"
        );
        assert!(
            container.contains("full-path=\"OEBPS/content.opf\""),
            "container points at the OPF package: {container}"
        );
        assert!(
            part(&parts, "OEBPS/content.opf").is_some(),
            "OPF part exists"
        );
    }
    // Phase `opf_carries_the_document_title_and_author`.
    {
        let mut snap = empty_snapshot();
        snap.properties
            .insert("title".into(), "The Grand Title".into());
        snap.properties
            .insert("author".into(), "Ada Lovelace".into());

        let epub = render_epub(&snap, &tilo_content::NoContent).expect("render");
        let parts = unzip(&epub);

        let opf = part_str(&parts, "OEBPS/content.opf");
        assert!(
            opf.contains("<dc:title>The Grand Title</dc:title>"),
            "OPF carries the document title: {opf}"
        );
        assert!(
            opf.contains("<dc:creator>Ada Lovelace</dc:creator>"),
            "OPF carries the author as dc:creator: {opf}"
        );
        // EPUB 3 requires a dcterms:modified meta and a unique identifier.
        assert!(opf.contains("dcterms:modified"), "OPF has dcterms:modified");
        assert!(
            opf.contains("unique-identifier=\"book-id\""),
            "OPF declares its unique-identifier"
        );
        assert!(
            opf.contains("properties=\"nav\""),
            "OPF manifest marks the nav item"
        );
    }
    // Phase `empty_document_still_produces_a_valid_epub`.
    {
        let snap = empty_snapshot();
        let epub = render_epub(&snap, &tilo_content::NoContent).expect("render empty");
        let parts = unzip(&epub);

        assert_eq!(parts[0].0, "mimetype");
        assert!(part(&parts, "META-INF/container.xml").is_some());
        assert!(part(&parts, "OEBPS/content.opf").is_some());
        assert!(part(&parts, "OEBPS/nav.xhtml").is_some());
        // At least one chapter so the spine is non-empty.
        assert!(part(&parts, "OEBPS/chapter-1.xhtml").is_some());
        // Untitled fallback title.
        let opf = part_str(&parts, "OEBPS/content.opf");
        assert!(
            opf.contains("<dc:title>Untitled</dc:title>"),
            "empty doc gets a fallback title"
        );
        // The nav must still have a toc target.
        let nav = part_str(&parts, "OEBPS/nav.xhtml");
        assert!(
            nav.contains("<a href="),
            "nav has at least one target even with no headings"
        );
    }
}

#[test]
fn epub_nav_mirrors_the_heading_tree() {
    // Phase `nav_has_a_toc_entry_per_heading`.
    {
        let mut snap = empty_snapshot();
        snap.properties.insert("title".into(), "Manual".into());
        snap.blocks = vec![
            heading(1, "Chapter One"),
            block("paragraph", "intro"),
            heading(2, "Section A"),
            block("paragraph", "details"),
            heading(1, "Chapter Two"),
            heading(2, "Section B"),
        ];

        let epub = render_epub(&snap, &tilo_content::NoContent).expect("render");
        let parts = unzip(&epub);

        let nav = part_str(&parts, "OEBPS/nav.xhtml");
        assert!(
            nav.contains("epub:type=\"toc\""),
            "nav declares the toc type"
        );

        // One TOC link per heading (4 headings here).
        let links = nav.matches("<a href=").count();
        assert_eq!(links, 4, "one TOC entry per heading\n{nav}");

        for label in ["Chapter One", "Section A", "Chapter Two", "Section B"] {
            assert!(nav.contains(label), "nav lists heading '{label}':\n{nav}");
        }

        // Nesting: the H2s must sit inside a nested <ol> deeper than the H1 level —
        // there must be more than one <ol> opened.
        assert!(
            nav.matches("<ol>").count() >= 2,
            "nested TOC has nested lists:\n{nav}"
        );

        // The nested nav must be well-formed XML (balanced <ol>/<li>).
        assert_well_formed_xml(&nav);
    }
    // Phase `deeply_nested_headings_yield_well_formed_nested_nav`.
    {
        // H1 → H2 → H3, plus a level-skip (H1 directly to H3) to exercise the tree
        // builder's "deeper heading nests under the nearest shallower one" rule.
        let mut snap = empty_snapshot();
        snap.properties.insert("title".into(), "Deep".into());
        snap.blocks = vec![
            heading(1, "One"),
            heading(2, "One.A"),
            heading(3, "One.A.i"),
            heading(1, "Two"),
            heading(3, "Two.skip"), // H1 → H3 with no intervening H2
        ];

        let epub = render_epub(&snap, &tilo_content::NoContent).expect("render");
        let parts = unzip(&epub);
        let nav = part_str(&parts, "OEBPS/nav.xhtml");

        // One link per heading.
        assert_eq!(
            nav.matches("<a href=").count(),
            5,
            "one TOC entry per heading\n{nav}"
        );
        assert!(
            nav.matches("<ol>").count() >= 3,
            "deep nesting opens several lists\n{nav}"
        );
        // Balanced <ol>/<li> — this is the invariant the original flat algorithm broke.
        assert_eq!(
            nav.matches("<li>").count(),
            nav.matches("</li>").count(),
            "every <li> is closed exactly once\n{nav}"
        );
        assert_eq!(
            nav.matches("<ol>").count(),
            nav.matches("</ol>").count(),
            "every <ol> is closed exactly once\n{nav}"
        );
        assert_well_formed_xml(&nav);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn epub_chapters_are_split_well_formed_escaped_and_carry_images() {
    // Phase `image_block_adds_an_image_manifest_item_and_packaged_resource`.
    {
        let mut snap = empty_snapshot();
        snap.properties
            .insert("title".into(), "Picture Book".into());
        let mut blobs = BTreeMap::new();
        blobs.insert("hash-abc".to_string(), tiny_png());
        snap.blocks = vec![
            block("paragraph", "before"),
            image_block("hash-abc", "a kitten"),
        ];

        let epub = render_epub(&snap, &blobs).expect("render");
        let parts = unzip(&epub);

        let img_part = parts.iter().find(|(n, _)| n.starts_with("OEBPS/images/"));
        assert!(
            img_part.is_some(),
            "image blob is packaged as a file resource"
        );
        let (img_name, img_bytes) = img_part.unwrap();
        assert!(
            std::path::Path::new(img_name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png")),
            "PNG blob packaged with .png extension: {img_name}"
        );
        assert_eq!(
            img_bytes,
            &tiny_png(),
            "packaged image bytes match the blob"
        );

        let opf = part_str(&parts, "OEBPS/content.opf");
        assert!(
            opf.contains("media-type=\"image/png\""),
            "OPF manifest has an image/png item:\n{opf}"
        );
        assert!(
            opf.contains("images/img-1.png"),
            "OPF manifest references the packaged image href"
        );

        // The chapter XHTML points its <img src> at the packaged file (not a data: URI).
        let chapter = part_str(&parts, "OEBPS/chapter-1.xhtml");
        assert!(
            chapter.contains("src=\"images/img-1.png\""),
            "chapter img is re-pointed to the packaged file:\n{chapter}"
        );
        assert!(
            !chapter.contains("data:image"),
            "no inline data: URI remains in the chapter"
        );
    }
    // Phase `chapters_are_well_formed_xml_with_self_closing_void_elements`.
    {
        let mut snap = empty_snapshot();
        snap.properties.insert("title".into(), "WF".into());
        snap.blocks = vec![
            heading(1, "Title <with> & ampersand"),
            block("paragraph", ""), // empty paragraph emits a <br>
            block("paragraph", "ok"),
        ];

        let epub = render_epub(&snap, &tilo_content::NoContent).expect("render");
        let parts = unzip(&epub);
        let chapter = part_str(&parts, "OEBPS/chapter-1.xhtml");

        // Void elements must be self-closed for XML well-formedness.
        assert!(
            chapter.contains("<br/>"),
            "empty paragraph yields a self-closed <br/>:\n{chapter}"
        );
        assert!(
            !chapter.contains("<br>"),
            "no bare HTML5 <br> remains:\n{chapter}"
        );
        assert!(
            chapter.starts_with("<?xml"),
            "chapter carries an XML prolog"
        );

        // The XML must parse (balanced tags, escaped specials). zip-roundtrip already
        // proved the bytes are intact; parse with a minimal well-formedness check.
        assert_well_formed_xml(&chapter);

        // All chapter/nav/opf parts must be well-formed XML.
        assert_well_formed_xml(&part_str(&parts, "OEBPS/nav.xhtml"));
        assert_well_formed_xml(&part_str(&parts, "OEBPS/content.opf"));
        assert_well_formed_xml(&part_str(&parts, "META-INF/container.xml"));
    }
    // Phase `body_splits_into_one_chapter_per_h1`.
    {
        let mut snap = empty_snapshot();
        snap.properties.insert("title".into(), "Multi".into());
        snap.blocks = vec![
            heading(1, "First"),
            block("paragraph", "a"),
            heading(1, "Second"),
            block("paragraph", "b"),
        ];

        let epub = render_epub(&snap, &tilo_content::NoContent).expect("render");
        let parts = unzip(&epub);

        assert!(
            part(&parts, "OEBPS/chapter-1.xhtml").is_some(),
            "chapter 1 exists"
        );
        assert!(
            part(&parts, "OEBPS/chapter-2.xhtml").is_some(),
            "chapter 2 exists"
        );

        // The spine lists both, in order.
        let opf = part_str(&parts, "OEBPS/content.opf");
        let i1 = opf
            .find("idref=\"chapter-1\"")
            .expect("spine has chapter-1");
        let i2 = opf
            .find("idref=\"chapter-2\"")
            .expect("spine has chapter-2");
        assert!(i1 < i2, "spine order is chapter-1 then chapter-2");
    }
    // Phase `text_specials_are_xml_escaped_not_injected`.
    {
        let mut snap = empty_snapshot();
        snap.properties
            .insert("title".into(), "Tom & Jerry <fight>".into());
        snap.blocks = vec![block("paragraph", "5 < 6 && \"quoted\"")];

        let epub = render_epub(&snap, &tilo_content::NoContent).expect("render");
        let parts = unzip(&epub);

        let opf = part_str(&parts, "OEBPS/content.opf");
        assert!(
            opf.contains("Tom &amp; Jerry &lt;fight&gt;"),
            "title is XML-escaped in OPF:\n{opf}"
        );
        assert_well_formed_xml(&opf);

        let chapter = part_str(&parts, "OEBPS/chapter-1.xhtml");
        assert!(
            chapter.contains("5 &lt; 6"),
            "body specials escaped:\n{chapter}"
        );
        assert_well_formed_xml(&chapter);
    }
}

/// A small XML well-formedness check: tags balance, prolog present, no raw `<`
/// or `&` that isn't part of a tag/entity. Not a full parser, but catches the
/// failure modes this crate could realistically introduce (unclosed void
/// elements, unescaped specials, mismatched tags).
fn assert_well_formed_xml(xml: &str) {
    let bytes = xml.as_bytes();
    let mut stack: Vec<String> = Vec::new();
    let mut i: usize = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => {
                let rel = xml[i..]
                    .find('>')
                    .unwrap_or_else(|| panic!("unterminated tag at {i}\n{xml}"));
                let end = i.saturating_add(rel);
                let tag = &xml[i.saturating_add(1)..end]; // inside the angle brackets

                if tag.starts_with('?') || tag.starts_with('!') {
                    // XML prolog / declaration / comment — skip.
                    i = end.saturating_add(1);
                    continue;
                }
                if let Some(close) = tag.strip_prefix('/') {
                    // Closing tag — must match the top of the stack.
                    let name = tag_name(close);
                    let open = stack
                        .pop()
                        .unwrap_or_else(|| panic!("close </{name}> with empty stack\n{xml}"));
                    assert_eq!(
                        open, name,
                        "mismatched close </{name}> (open was <{open}>)\n{xml}"
                    );
                } else if tag.ends_with('/') {
                    // Self-closing — no stack change.
                } else {
                    stack.push(tag_name(tag).clone());
                }
                i = end.saturating_add(1);
            }
            b'&' => {
                // Must be a well-formed entity: &name; or &#nnn;.
                let rest = &xml[i.saturating_add(1)..];
                let semi = rest
                    .find(';')
                    .unwrap_or_else(|| panic!("unescaped '&' at {i}\n{xml}"));
                let ent = &rest[..semi];
                assert!(
                    !ent.is_empty() && ent.chars().all(|c| c.is_ascii_alphanumeric() || c == '#'),
                    "malformed entity '&{ent};' at {i}\n{xml}"
                );
                i = i.saturating_add(1).saturating_add(semi).saturating_add(1);
            }
            _ => i = i.saturating_add(1),
        }
    }
    assert!(stack.is_empty(), "unclosed tags {stack:?}\n{xml}");
}

/// Extract the element name from a tag's inner text (`p class="x"` → `p`).
fn tag_name(inner: &str) -> String {
    inner
        .trim_start_matches('/')
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '/' && *c != '>')
        .collect()
}
