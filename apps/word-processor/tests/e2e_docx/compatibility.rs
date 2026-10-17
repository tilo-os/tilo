//! Compatibility through the actual Writer resource app. Opaque XML retention
//! is deliberately tested separately from editable/rendered document structure.

use std::collections::BTreeMap;
use std::io::Read as _;

use quick_xml::events::Event;
use tilo_doc::DocSnapshot;
use word_processor::actions::{RegionKind, RichTextAction};

use crate::support::{apply, launched, save_as};

// Comments live as framework resources after import, not in the text body's
// legacy comment container. Read the same combined view that Save As uses.
fn snapshot(app: &crate::support::App) -> DocSnapshot {
    app.read_document(|doc| {
        word_processor::model::export_copy(doc)
            .unwrap()
            .materialize()
    })
}

const SUFFIX: &str = " — edited in Writer 東京";
const RETAINED: &[&str] = &[
    "w:tbl",
    "w:gridSpan",
    "w:vMerge",
    "w:ilvl",
    "w:pStyle",
    "w:rStyle",
    "w:sectPr",
    "w:bookmarkStart",
    "w:hyperlink",
    "w:sdt",
    "w:fldChar",
    "w:instrText",
    "m:oMath",
    "w:drawing",
    "wp:anchor",
    "wp:inline",
    "w:txbxContent",
    "w:footnoteReference",
    "w:endnoteReference",
    "w:commentRangeStart",
    "w:ins",
    "w:del",
    "c:chart",
];

fn package(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut parts = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        if !entry.is_dir() {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            parts.insert(entry.name().unwrap().into_owned(), bytes);
        }
    }
    parts
}

fn census(xml: &[u8]) -> BTreeMap<String, usize> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut counts = BTreeMap::new();
    loop {
        match reader.read_event().expect("exported XML is well formed") {
            Event::Start(element) | Event::Empty(element) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                // Word's private navigation cursor is intentionally not a
                // document bookmark; the importer explicitly discards it.
                let navigation_cursor = name == "w:bookmarkStart"
                    && element.attributes().any(|attr| {
                        let attr = attr.unwrap();
                        attr.key.as_ref() == b"w:name" && attr.value.as_ref() == b"_GoBack"
                    });
                if RETAINED.contains(&name.as_str()) && !navigation_cursor {
                    let count = counts.entry(name).or_insert(0_usize);
                    *count = count.saturating_add(1);
                }
            }
            Event::Eof => return counts,
            _ => {}
        }
    }
}

fn assert_package_retained(original: &[u8], exported: &[u8]) {
    let original = package(original);
    let exported = package(exported);
    for (name, bytes) in &original {
        assert!(exported.contains_key(name), "lost package part {name}");
        if name.starts_with("word/media/") || name.starts_with("word/embeddings/") {
            assert_eq!(&exported[name], bytes, "changed opaque payload {name}");
        }
        // Side-part paragraphs currently retain text, not arbitrary run or
        // paragraph formatting. Body rich constructs have the opaque carrier.
        if name == "word/document.xml" {
            let before = census(bytes);
            let after = census(&exported[name]);
            for (kind, count) in before {
                assert!(
                    after.get(&kind).copied().unwrap_or(0) >= count,
                    "{name}: lost {kind}, originally {count}, now {after:?}"
                );
            }
        }
    }
}

/// Non-codec evidence: a named production editor exists, and its inspected
/// widget tree actually exposes imported text to assistive technology.
fn assert_editor_shows(app: &mut crate::support::App, text: &str) {
    app.harness().relayout();
    assert!(app.harness().targets().contains_key("word_processor"));
    let inspection = serde_json::to_string(&app.harness().inspection()).unwrap();
    assert!(
        inspection.contains(text),
        "editor did not expose {text}: {inspection}"
    );
}

fn content(blocks: &[tilo_doc::snapshot::BlockView]) -> serde_json::Value {
    serde_json::Value::Array(
        blocks
            .iter()
            .map(|block| {
                let attrs: BTreeMap<_, _> = block
                    .attrs
                    .iter()
                    .filter(|(key, _)| {
                        matches!(
                            key.as_str(),
                            "level" | "ordered" | "style" | "align" | "rowspan" | "colspan"
                        )
                    })
                    .collect();
                let mut marks: Vec<_> = block
                    .marks
                    .iter()
                    // Local CRDT attribution is not a Word formatting property.
                    .filter(|mark| mark.name != "author")
                    .map(|mark| {
                        let value =
                            if matches!(mark.name.as_str(), "note-ref" | "comment" | "ins" | "del")
                            {
                                String::new() // IDs are minted afresh on import.
                            } else {
                                format!("{:?}", mark.value)
                            };
                        (mark.start, mark.end, &mark.name, value)
                    })
                    .collect();
                marks.sort();
                marks.dedup();
                serde_json::json!([
                    block.kind,
                    block.text,
                    attrs,
                    marks,
                    content(&block.children)
                ])
            })
            .collect(),
    )
}

fn round_trip(bytes: &[u8], needle: &str, required: &[&str], verify: fn(&DocSnapshot)) {
    let parts = package(bytes);
    let counts = census(&parts["word/document.xml"]);
    for kind in required {
        assert!(
            counts.get(*kind).copied().unwrap_or(0) > 0,
            "source fixture does not exercise {kind}"
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("import.docx");
    let saved = directory.path().join("edited.docx");
    std::fs::write(&source, bytes).unwrap();
    let mut app = launched(&source);
    let before = snapshot(&app);
    verify(&before);
    let paragraph = before.blocks.iter().find(|b| b.text == needle).unwrap();
    assert_editor_shows(&mut app, needle);
    // This is an Invoke::edit through WordProcessorApp's typed resource door,
    // not a mutation of an imported codec document or a model-only helper.
    apply(
        &mut app,
        &RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: paragraph.id.clone(),
            at: paragraph.text.chars().count(),
            del: 0,
            ins: SUFFIX.into(),
        },
    );
    let edited = format!("{needle}{SUFFIX}");
    assert_editor_shows(&mut app, &edited);
    let expected = snapshot(&app);
    save_as(&mut app, &saved);
    assert_eq!(
        std::fs::read(&source).unwrap(),
        bytes,
        "Save As changed source"
    );
    drop(app);
    let mut reopened = launched(&saved);
    assert_editor_shows(&mut reopened, &edited);
    let actual = snapshot(&reopened);
    assert_eq!(content(&actual.blocks), content(&expected.blocks));
    assert_eq!(content(&actual.header), content(&expected.header));
    assert_eq!(content(&actual.footer), content(&expected.footer));
    // Resource enumeration order is not a document property. Compare every
    // note/comment's kind or author and full content, including multiplicity.
    let notes = |doc: &DocSnapshot| {
        let mut values: Vec<_> = doc
            .notes
            .iter()
            .map(|note| serde_json::json!([note.kind, content(&note.blocks)]).to_string())
            .collect();
        values.sort();
        values
    };
    let comments = |doc: &DocSnapshot| {
        let mut values: Vec<_> = doc
            .comments
            .iter()
            .map(|comment| {
                serde_json::json!([comment.author, content(&comment.blocks)]).to_string()
            })
            .collect();
        values.sort();
        values
    };
    assert_eq!(notes(&actual), notes(&expected));
    assert_eq!(comments(&actual), comments(&expected));
    // A default single-column w:cols is normalized away on export, so compare
    // the modeled layout value rather than requiring that redundant element.
    assert_eq!(actual.columns, before.columns);
    assert_eq!(actual.notes.len(), before.notes.len());
    assert_eq!(actual.comments.len(), before.comments.len());
    assert_eq!(actual.changes.len(), before.changes.len());
    assert_eq!(actual.image_content(), before.image_content());
    assert_package_retained(bytes, &std::fs::read(saved).unwrap());
}

#[test]
fn libreoffice_structure_import_edit_save_reopen() {
    round_trip(
        include_bytes!(concat!(env!("OUT_DIR"), "/lo-structure.docx")),
        "Structure",
        &[
            "w:tbl",
            "w:gridSpan",
            "w:vMerge",
            "w:ilvl",
            "w:pStyle",
            "w:rStyle",
            "w:sectPr",
            "w:bookmarkStart",
            "w:hyperlink",
        ],
        |doc| {
            assert!(
                doc.blocks
                    .iter()
                    .any(|b| b.kind == "heading" && b.text == "Structure")
            );
            assert!(
                doc.blocks
                    .iter()
                    .any(|b| b.kind == "cell" && b.text == "Wide header")
            );
            assert!(
                doc.blocks
                    .iter()
                    .any(|b| b.kind == "list_item" && b.text == "Nested a")
            );
            assert!(!doc.styles.is_empty());
            assert!(
                doc.header
                    .iter()
                    .any(|b| b.text.contains("Structure header"))
            );
            assert!(!doc.footer.is_empty());
        },
    );
}

#[test]
fn libreoffice_annotations_import_edit_save_reopen() {
    round_trip(
        include_bytes!(concat!(env!("OUT_DIR"), "/lo-annotations.docx")),
        "Notes and comments",
        &[
            "w:footnoteReference",
            "w:endnoteReference",
            "w:commentRangeStart",
            "w:ins",
            "w:del",
        ],
        |doc| {
            assert_eq!(doc.notes.len(), 2, "footnote and endnote");
            assert_eq!(doc.comments.len(), 1);
            assert!(!doc.changes.is_empty());
        },
    );
}

#[test]
fn libreoffice_fields_import_edit_save_reopen() {
    round_trip(
        include_bytes!(concat!(env!("OUT_DIR"), "/lo-fields.docx")),
        "Fields",
        &["w:sdt", "w:fldChar", "w:instrText"],
        |doc| {
            assert!(doc.blocks.iter().any(|b| b.text.contains("ACME Corp")));
            assert!(doc.blocks.iter().any(|b| b.text.contains("2024-05-06")));
        },
    );
}

#[test]
fn libreoffice_drawings_import_edit_save_reopen() {
    round_trip(
        include_bytes!(concat!(env!("OUT_DIR"), "/lo-drawings.docx")),
        "Drawings",
        &[
            "m:oMath",
            "w:drawing",
            "wp:anchor",
            "wp:inline",
            "w:txbxContent",
            "c:chart",
        ],
        |doc| {
            assert!(!doc.image_content().is_empty(), "inline image is modeled");
            assert!(doc.blocks.iter().any(|b| b.text.contains("Einstein wrote")));
        },
    );
}

#[test]
fn upstream_python_docx_merged_tables_import_edit_save_reopen() {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/python-docx-merged-cells.docx"));
    let parts = package(bytes);
    let counts = census(&parts["word/document.xml"]);
    assert_eq!(counts["w:tbl"], 4);
    assert_eq!(counts["w:gridSpan"], 3);
    assert_eq!(counts["w:vMerge"], 4);
    round_trip(bytes, "1", &["w:tbl", "w:gridSpan", "w:vMerge"], |doc| {
        assert!(doc.blocks.iter().filter(|b| b.kind == "cell").count() >= 20);
        assert!(doc.blocks.iter().any(|b| b.kind == "cell" && b.text == "9"));
    });
}
