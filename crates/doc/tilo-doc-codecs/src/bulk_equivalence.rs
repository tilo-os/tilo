//! Every built-in codec builds the same document through a bulk scope as it does op by op —
//! with marks applied as local operations (a host-owned document with history) and as committed
//! changes (an owned [`tilo_doc::Document`], or a host's document with no history yet) — and the
//! documents keep behaving the same under later edits.

use crate::export_html::{HtmlOptions, render_html};
use crate::export_rtf::render_rtf;
use crate::{DocxCodec, FormatProvider, HtmlCodec, MarkdownCodec, OdtCodec, RtfCodec};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ROOT};
use tilo_doc::model::{self, Containers, DocMut, DocRef, IdGen, Region, change_writes, op_by_op};
use tilo_doc::snapshot::DocSnapshot;

#[path = "../tests/support/synth.rs"]
mod synth;

const SAMPLE_DOCX: &[u8] = include_bytes!("../../fixtures/sample.docx");
const SAMPLE_ODT: &[u8] = include_bytes!("../../fixtures/sample.odt");

#[derive(Clone, Copy, Debug)]
enum Path {
    OpByOp,
    BulkLocalMarks,
    BulkChangeMarks,
    /// A host-owned document that has never been committed, as a host opening a file creates.
    BulkFreshHost,
}

const PATHS: [Path; 4] = [
    Path::OpByOp,
    Path::BulkLocalMarks,
    Path::BulkChangeMarks,
    Path::BulkFreshHost,
];

struct Replica {
    doc: AutoCommit,
    c: Containers,
    ids: IdGen,
}

impl Replica {
    /// A fresh document with a fixed actor, so every path mints the same block ids.
    fn new(path: Path) -> Self {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(b"bulk-equivalence".as_slice()));
        let c = model::init(&mut doc, &ROOT);
        let actor = doc.get_actor().to_hex_string();
        // A host editing a document seeds its generator from it; one creating a document starts
        // a new one. Both count from zero here, so every path mints the same ids.
        let mut ids = match path {
            Path::BulkLocalMarks => IdGen::with_counter(actor, 0),
            _ => IdGen::new(actor),
        };
        if matches!(path, Path::BulkChangeMarks) {
            ids.allow_commits();
        }
        if !matches!(path, Path::BulkFreshHost) {
            doc.commit();
        }
        Self { doc, c, ids }
    }

    fn dm(&mut self) -> DocMut<'_> {
        DocMut {
            doc: &mut self.doc,
            c: &self.c,
            ids: &mut self.ids,
        }
    }

    fn snapshot(&self) -> DocSnapshot {
        model::materialize(&self.doc, &self.c)
    }

    fn reloaded_snapshot(&mut self) -> DocSnapshot {
        let doc = AutoCommit::load(&self.doc.save()).unwrap();
        let c = model::open(&doc, &ROOT).unwrap();
        model::materialize(&doc, &c)
    }
}

fn import(codec: &dyn FormatProvider, bytes: &[u8], path: Path) -> Replica {
    let mut r = Replica::new(path);
    let writes = change_writes();
    match path {
        Path::OpByOp => {
            op_by_op(|| codec.import(bytes, &mut r.dm(), &mut tilo_content::ContentInputs::new()))
        }
        Path::BulkLocalMarks | Path::BulkChangeMarks | Path::BulkFreshHost => {
            codec.import(bytes, &mut r.dm(), &mut tilo_content::ContentInputs::new())
        }
    }
    .unwrap();
    let wrote_changes = change_writes() > writes;
    assert_eq!(
        wrote_changes,
        matches!(path, Path::BulkChangeMarks | Path::BulkFreshHost),
        "{path:?} took the wrong write path"
    );
    if matches!(path, Path::BulkFreshHost) {
        assert!(
            r.doc.pending_ops() > 0,
            "the host's own commit must still carry part of the import"
        );
    }
    r.doc.commit();
    r
}

/// Insert text on both sides of the first mark of every body block: at its end the mark expands
/// over the insert, at its start it does not. Both only hold if the mark anchors sit where a local
/// `mark` puts them.
fn edit_at_mark_edges(r: &mut Replica) {
    let snap = r.snapshot();
    let mut dm = r.dm();
    for b in &snap.blocks {
        if let Some(m) = b.marks.first() {
            dm.splice_text(&b.id, m.end, 0, "»");
            dm.splice_text(&b.id, m.start, 0, "«");
        }
    }
    r.doc.commit();
}

fn assert_paths_agree(name: &str, codec: &dyn FormatProvider, bytes: &[u8]) {
    let mut replicas: Vec<(Path, Replica)> = PATHS
        .iter()
        .map(|&p| (p, import(codec, bytes, p)))
        .collect();
    let (_, reference) = &replicas[0];
    let want = reference.snapshot();
    assert!(!want.blocks.is_empty(), "{name}: imported nothing");
    for (path, r) in &mut replicas {
        assert_eq!(r.snapshot(), want, "{name}: {path:?} differs from op-by-op");
        assert_eq!(
            r.reloaded_snapshot(),
            want,
            "{name}: {path:?} differs after save/load"
        );
    }
    let mut edited = Vec::new();
    for (path, r) in &mut replicas {
        edit_at_mark_edges(r);
        edited.push((*path, r.snapshot()));
    }
    let (_, want) = &edited[0];
    for (path, got) in &edited {
        assert_eq!(
            got, want,
            "{name}: {path:?} diverges after editing at mark edges"
        );
    }
}

/// A document exercising what Markdown can't carry: comments, tracked changes, bookmarks,
/// header/footer with a page-number field, footnotes.
fn rich_document() -> Replica {
    let mut r = Replica::new(Path::OpByOp);
    let mut dm = r.dm();
    let mut prev: Option<String> = None;
    for i in 0..12 {
        let b = dm.insert_block(
            prev.as_deref(),
            if i % 4 == 0 { "heading" } else { "paragraph" },
        );
        if i % 4 == 0 {
            dm.set_attr(&b, "level", 2_i64);
        }
        dm.splice_text(
            &b,
            0,
            0,
            &format!("Paragraph {i} with some words to mark up."),
        );
        dm.add_mark(&b, 0, 9, "strong", true);
        dm.add_mark(&b, 5, 20, "em", true);
        dm.add_mark(&b, 22, 26, "color", "#ff0000");
        if i % 3 == 0 {
            dm.add_comment(&b, 10, 14, "Ada", "Check this.");
        }
        if i % 5 == 1 {
            dm.track_insert(&b, 3, "new ", "Grace", "2026-01-01T00:00:00Z");
            dm.track_delete(&b, 20, 24, "Grace", "2026-01-01T00:00:00Z");
        }
        if i == 2 {
            dm.add_bookmark(&b, 0, 4, "anchor");
            dm.insert_footnote(&b, 6);
        }
        prev = Some(b);
    }
    let h = dm.insert_block_in(Region::Header, None, "paragraph");
    dm.splice_text_in(Region::Header, &h, 0, 0, "Page ");
    dm.insert_page_number_field(Region::Header, &h, 5);
    let f = dm.insert_block_in(Region::Footer, None, "paragraph");
    dm.splice_text_in(Region::Footer, &f, 0, 0, "Footer text");
    r.doc.commit();
    r
}

#[test]
fn bulk_and_incremental_imports_agree() {
    // Phase `markdown_imports_agree`.
    {
        let mut md = b"---\ntitle: Scaling\nauthor: Ada\n---\n\n".to_vec();
        md.extend(synth::markdown(8_000));
        assert_paths_agree("md", &MarkdownCodec, &md);
    }
    // Phase `exported_formats_import_alike`.
    {
        let md = synth::markdown(8_000);
        let source = import(&MarkdownCodec, &md, Path::OpByOp);
        let snap = source.snapshot();
        let doc = DocRef {
            doc: &source.doc,
            c: &source.c,
        };
        assert_paths_agree(
            "docx",
            &DocxCodec,
            &DocxCodec.export(doc, &tilo_content::NoContent).unwrap(),
        );
        let doc = DocRef {
            doc: &source.doc,
            c: &source.c,
        };
        assert_paths_agree(
            "odt",
            &OdtCodec,
            &OdtCodec.export(doc, &tilo_content::NoContent).unwrap(),
        );
        let html = render_html(&snap, HtmlOptions::default(), &tilo_content::NoContent);
        assert_paths_agree("html", &HtmlCodec, html.as_bytes());
        assert_paths_agree("rtf", &RtfCodec, render_rtf(&snap).as_bytes());
    }
    // Phase `office_features_import_alike`.
    {
        let source = rich_document();
        let doc = DocRef {
            doc: &source.doc,
            c: &source.c,
        };
        assert_paths_agree(
            "rich docx",
            &DocxCodec,
            &DocxCodec.export(doc, &tilo_content::NoContent).unwrap(),
        );
        let doc = DocRef {
            doc: &source.doc,
            c: &source.c,
        };
        assert_paths_agree(
            "rich odt",
            &OdtCodec,
            &OdtCodec.export(doc, &tilo_content::NoContent).unwrap(),
        );
    }
    // Phase `fixture_documents_import_alike`.
    {
        assert_paths_agree("sample.docx", &DocxCodec, SAMPLE_DOCX);
        assert_paths_agree("sample.odt", &OdtCodec, SAMPLE_ODT);
        // LibreOffice-written documents full of content kept verbatim, merged cells and styles.
        for (name, docx, odt) in [
            (
                "lo-structure",
                include_bytes!("../../fixtures/lo-structure.docx").as_slice(),
                include_bytes!("../../fixtures/lo-structure.odt").as_slice(),
            ),
            (
                "lo-annotations",
                include_bytes!("../../fixtures/lo-annotations.docx").as_slice(),
                include_bytes!("../../fixtures/lo-annotations.odt").as_slice(),
            ),
            (
                "lo-fields",
                include_bytes!("../../fixtures/lo-fields.docx").as_slice(),
                include_bytes!("../../fixtures/lo-fields.odt").as_slice(),
            ),
            (
                "lo-drawings",
                include_bytes!("../../fixtures/lo-drawings.docx").as_slice(),
                include_bytes!("../../fixtures/lo-drawings.odt").as_slice(),
            ),
        ] {
            assert_paths_agree(&format!("{name}.docx"), &DocxCodec, docx);
            assert_paths_agree(&format!("{name}.odt"), &OdtCodec, odt);
        }
    }
}

/// The one-walk span reader agrees with automerge's own `text` and `marks` on every text object
/// of real documents, including marks removed again and marks merged across runs.
#[test]
fn span_reader_matches_automerge_text_and_marks() {
    use automerge::ReadDoc;
    let md = synth::markdown(4_000);
    let mut r = import(&MarkdownCodec, &md, Path::BulkChangeMarks);
    let snap = r.snapshot();
    {
        let mut dm = r.dm();
        for b in snap.blocks.iter().take(20) {
            let n = b.text.chars().count();
            if n > 6 {
                dm.remove_mark(&b.id, 1, 4, "strong");
                dm.add_mark(&b.id, 2, 6, "em", true);
                dm.add_mark(&b.id, 0, n, "em", true);
            }
        }
    }
    r.doc.commit();
    let mut checked = 0;
    for b in r.snapshot().blocks {
        let Some((_, obj)) = r.doc.get(&r.c.blocks, b.id.as_str()).unwrap() else {
            continue;
        };
        let Some((_, text)) = r.doc.get(&obj, tilo_doc::schema::BLOCK_TEXT).unwrap() else {
            continue;
        };
        assert_eq!(b.text, r.doc.text(&text).unwrap());
        let mut want: Vec<tilo_doc::MarkSpan> = r
            .doc
            .marks(&text)
            .unwrap()
            .into_iter()
            .map(|m| tilo_doc::MarkSpan {
                start: m.start,
                end: m.end,
                name: m.name().to_string(),
                value: m.value().into(),
            })
            .collect();
        want.sort_by(|a, b| {
            (a.start, a.end, &a.name, &a.value).cmp(&(b.start, b.end, &b.name, &b.value))
        });
        assert_eq!(b.marks, want, "block {}", b.id);
        checked += 1;
    }
    assert!(checked > 20);
}
