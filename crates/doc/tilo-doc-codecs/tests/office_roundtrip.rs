//! Real-path office round-trip, version-diff, security, and performance
//! tests over **genuine** `.odt`/`.docx` fixtures (complete ODF/OOXML
//! packages LibreOffice/Word open normally). Office bytes are parsed
//! into the `qdoc` CRDT model, edited as CRDT operations, serialized
//! back, and reopened — proving the model is the source of truth, not
//! an opaque blob.

use std::fmt::Write as _;
use tilo_doc::diff::{BlockOp, RunOp, diff_snapshots};
use tilo_doc::{BlockView, DocSnapshot, Document, MarkSpan};
use tilo_doc_codecs::{DocxCodec, FormatProvider, OdtCodec};

const SAMPLE_ODT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sample.odt"));
const SAMPLE_DOCX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sample.docx"));
const ZIPBOMB_DOCX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/zipbomb.docx"));
const XXE_DOCX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/xxe.docx"));
const XXE_ODT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/xxe.odt"));

#[allow(
    clippy::expect_used,
    reason = "test fixture helper: a failure here is a broken fixture, and the message says so"
)]
fn import(codec: &dyn FormatProvider, bytes: &[u8]) -> Document {
    let mut d = Document::new();
    codec
        .import(
            bytes,
            &mut d.as_mut(),
            &mut tilo_content::ContentInputs::new(),
        )
        .expect("import into CRDT model");
    d
}

/// The characters a mark of `name` covers in a block (its rendered
/// substring), so tests can assert *what* is bold/italic without
/// pinning exact offsets.
fn covered(block: &BlockView, name: &str) -> Vec<String> {
    let chars: Vec<char> = block.text.chars().collect();
    block
        .marks
        .iter()
        .filter(|m: &&MarkSpan| m.name == name)
        .map(|m| {
            chars
                .get(m.start.min(chars.len())..m.end.min(chars.len()))
                .unwrap_or_default()
                .iter()
                .collect()
        })
        .collect()
}

fn kinds<'a>(snap: &'a DocSnapshot, kind: &str) -> Vec<&'a BlockView> {
    snap.blocks.iter().filter(|b| b.kind == kind).collect()
}

fn texts(blocks: &[&BlockView]) -> Vec<String> {
    blocks.iter().map(|b| b.text.clone()).collect()
}

/// The invariant structure both fixtures share: an H1, a paragraph with
/// a bold and an italic run, two bullet items, a 2x2 table, and
/// title/author metadata.
#[allow(
    clippy::panic,
    reason = "test assertion helper: a panic here means a fixture bug, and the message is the fixture's context"
)]
fn assert_sample_structure(snap: &DocSnapshot, format: &str) {
    let headings = kinds(snap, "heading");
    assert!(
        headings.iter().any(|h| h.text == "Quarterly Report"),
        "{format}: expected an H1 'Quarterly Report', got {:?}",
        snap.blocks
            .iter()
            .map(|b| (b.kind.as_str(), b.text.as_str()))
            .collect::<Vec<_>>()
    );

    let para = snap
        .blocks
        .iter()
        .find(|b| b.kind == "paragraph" && b.text.contains("bold") && b.text.contains("italic"))
        .unwrap_or_else(|| panic!("{format}: mixed-run paragraph missing"));
    assert!(
        covered(para, "strong").iter().any(|s| s == "bold"),
        "{format}: 'bold' run must carry a strong mark; marks={:?}",
        para.marks
    );
    assert!(
        covered(para, "em").iter().any(|s| s == "italic"),
        "{format}: 'italic' run must carry an em mark; marks={:?}",
        para.marks
    );

    let items = kinds(snap, "list_item");
    assert_eq!(
        texts(&items),
        vec!["First item", "Second item"],
        "{format}: two bullet items"
    );

    let cells = kinds(snap, "cell");
    assert_eq!(
        texts(&cells),
        vec!["Region", "Total", "North", "1200"],
        "{format}: 2x2 table cells in order"
    );

    assert_eq!(
        snap.properties.get("title").map(String::as_str),
        Some("Quarterly Report"),
        "{format}: title metadata"
    );
    assert_eq!(
        snap.properties.get("author").map(String::as_str),
        Some("Ada Lovelace"),
        "{format}: author metadata"
    );
}

#[test]
fn real_office_files_parse_edit_and_diff_as_crdt() {
    // Phase `odt_parses_real_file_into_crdt_model`.
    {
        let d = import(&OdtCodec, SAMPLE_ODT);
        assert_sample_structure(&d.materialize(), "odt");
    }
    // Phase `docx_parses_real_file_into_crdt_model`.
    {
        let d = import(&DocxCodec, SAMPLE_DOCX);
        assert_sample_structure(&d.materialize(), "docx");
    }
    // Phase `odt_edit_as_crdt_op_survives_reopen`.
    {
        edit_and_reopen(&OdtCodec, SAMPLE_ODT, "odt");
    }
    // Phase `docx_edit_as_crdt_op_survives_reopen`.
    {
        edit_and_reopen(&DocxCodec, SAMPLE_DOCX, "docx");
    }
    // Phase `version_diff_reports_structural_changes_on_real_docx`.
    {
        let mut d = import(&DocxCodec, SAMPLE_DOCX);
        let v1 = d.materialize();

        let heading_id = d
            .blocks_in_order()
            .into_iter()
            .find(|id| d.block_kind(id).as_deref() == Some("heading"))
            .unwrap();
        let hlen = d
            .block_text(&heading_id)
            .unwrap_or_default()
            .chars()
            .count();
        d.splice_text(&heading_id, hlen, 0, " (DRAFT)");
        let last = d.blocks_in_order().pop().unwrap();
        let np = d.insert_block(Some(&last), "paragraph");
        d.splice_text(&np, 0, 0, "New section");
        let v2 = d.materialize();

        let diff = diff_snapshots(&v1, &v2);
        assert_eq!(diff.summary.added, 1, "one paragraph added");
        assert_eq!(diff.summary.removed, 0, "nothing removed");
        assert_eq!(diff.summary.changed, 1, "the heading changed");

        let ch = diff
            .blocks
            .iter()
            .find(|b| b.op == BlockOp::Changed)
            .unwrap();
        assert_eq!(ch.id, heading_id);
        assert_eq!(ch.old_text.as_deref(), Some("Quarterly Report"));
        assert_eq!(ch.new_text.as_deref(), Some("Quarterly Report (DRAFT)"));
        assert!(
            ch.runs
                .iter()
                .any(|r| r.op == RunOp::Insert && r.text.contains("DRAFT")),
            "run diff must show the inserted text"
        );
        assert!(
            ch.runs
                .iter()
                .any(|r| r.op == RunOp::Equal && r.text.contains("Quarterly Report")),
            "run diff must keep the shared prefix"
        );

        let added = diff.blocks.iter().find(|b| b.op == BlockOp::Added).unwrap();
        assert_eq!(added.new_text.as_deref(), Some("New section"));
        assert!(added.old_text.is_none());

        let json = serde_json::to_string(&diff).expect("diff serializes to JSON");
        assert!(json.contains("New section") && json.contains("DRAFT"));
    }
}

/// Apply two edits as real CRDT operations (a text splice into the
/// heading, a newly inserted paragraph), export to `format`, reopen,
/// and prove both edits + the original structure survive as replayable
/// CRDT change history (an intermediate version shows the first edit
/// but not the second).
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertion helper: a failure here means a fixture/test bug, and each message says so"
)]
fn edit_and_reopen(codec: &dyn FormatProvider, bytes: &[u8], format: &str) {
    let mut d = import(codec, bytes);

    let heading_id = d
        .blocks_in_order()
        .into_iter()
        .find(|id| d.block_kind(id).as_deref() == Some("heading"))
        .expect("heading block");
    let hlen = d
        .block_text(&heading_id)
        .unwrap_or_default()
        .chars()
        .count();
    let versions_before = d.versions().len();
    d.splice_text(&heading_id, hlen, 0, " (DRAFT)");

    let last = d.blocks_in_order().pop().expect("a last block");
    let new_para = d.insert_block(Some(&last), "paragraph");
    d.splice_text(&new_para, 0, 0, "Appended by CRDT edit");
    let versions_after = d.versions().len();
    assert!(
        versions_after > versions_before,
        "{format}: edits must extend the CRDT change history ({versions_before} -> {versions_after})"
    );

    let saw_intermediate = (0..d.versions().len()).any(|v| {
        let s = d.materialize_at(v).unwrap();
        let has_draft = s
            .blocks
            .iter()
            .any(|b| b.kind == "heading" && b.text.contains("(DRAFT)"));
        let has_appended = s.blocks.iter().any(|b| b.text == "Appended by CRDT edit");
        has_draft && !has_appended
    });
    assert!(
        saw_intermediate,
        "{format}: expected an intermediate version with only the first edit"
    );

    let out = codec
        .export(d.as_ref(), &tilo_content::NoContent)
        .unwrap_or_else(|e| panic!("{format}: export failed: {e}"));
    let reopened = import(codec, &out);
    let snap = reopened.materialize();

    assert!(
        snap.blocks
            .iter()
            .any(|b| b.kind == "heading" && b.text == "Quarterly Report (DRAFT)"),
        "{format}: heading edit lost on reopen: {:?}",
        snap.blocks
            .iter()
            .filter(|b| b.kind == "heading")
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        snap.blocks
            .iter()
            .any(|b| b.text == "Appended by CRDT edit"),
        "{format}: appended paragraph lost on reopen"
    );

    let para = snap
        .blocks
        .iter()
        .find(|b| b.text.contains("bold") && b.text.contains("italic"))
        .expect("styled paragraph");
    assert!(
        covered(para, "strong").iter().any(|s| s == "bold"),
        "{format}: bold lost on reopen"
    );
    assert!(
        covered(para, "em").iter().any(|s| s == "italic"),
        "{format}: italic lost on reopen"
    );
    assert_eq!(
        texts(&kinds(&snap, "list_item")),
        vec!["First item", "Second item"],
        "{format}: list lost on reopen"
    );
    assert_eq!(
        texts(&kinds(&snap, "cell")),
        vec!["Region", "Total", "North", "1200"],
        "{format}: table lost on reopen"
    );
    assert_eq!(
        snap.properties.get("title").map(String::as_str),
        Some("Quarterly Report"),
        "{format}: title lost on reopen"
    );
}

#[test]
fn hostile_office_inputs_are_rejected() {
    // Phase `zip_bomb_is_rejected_not_inflated`.
    {
        // The production codec path must reject a decompression bomb, fast,
        // without OOMing. The fixture is ~300 KB on disk but declares a
        // 300 MiB part — over the 256 MiB per-part cap.
        let mut d = Document::new();
        let err = DocxCodec
            .import(
                ZIPBOMB_DOCX,
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .expect_err("zip bomb must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("limit") || msg.contains("bomb"),
            "expected a bounded-size rejection, got: {msg}"
        );
    }
    // Phase `xxe_dtd_is_rejected_in_both_formats`.
    {
        // A `<!DOCTYPE ... <!ENTITY ...>>` in a modeled XML part is rejected
        // outright (defence in depth against XXE/entity-expansion), both codecs.
        let mut d = Document::new();
        let err = DocxCodec
            .import(
                XXE_DOCX,
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .expect_err("docx XXE must be rejected");
        assert!(
            format!("{err}").contains("DTD"),
            "docx: expected a DTD rejection, got: {err}"
        );

        let mut d2 = Document::new();
        let err2 = OdtCodec
            .import(
                XXE_ODT,
                &mut d2.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .expect_err("odt XXE must be rejected");
        assert!(
            format!("{err2}").contains("DTD"),
            "odt: expected a DTD rejection, got: {err2}"
        );
    }
    // Phase `garbage_input_is_rejected_cleanly`.
    {
        // Non-zip garbage must fail as a clean error, never a panic.
        let mut d = Document::new();
        assert!(
            DocxCodec
                .import(
                    b"not a zip at all",
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new()
                )
                .is_err()
        );
        assert!(
            OdtCodec
                .import(
                    &[0xff, 0x00, 0x13, 0x37],
                    &mut d.as_mut(),
                    &mut tilo_content::ContentInputs::new()
                )
                .is_err()
        );
    }
}

/// Build a large book-length document in the CRDT model, export +
/// reimport for both formats.
///
/// Guards the two O(n^2) traps the block-order fix closed — a full
/// container rescan per insert, and order keys that grow a byte per
/// insert — asserted **structurally** (a scan counter, order-key
/// lengths) so a regression fails in a plain debug `cargo test`. The
/// release build also round-trips the full book and reports its times;
/// `codec_scaling` is the wall-clock measurement.
#[test]
fn large_document_parse_and_serialize_does_constant_order_work() {
    use std::time::Instant;

    // The release round-trip runs the full book-length N; the structural
    // guards are scale-independent, so a *debug* build uses a smaller N
    // to stay fast (building N blocks is automerge-0.8's inherent O(n^2),
    // minutes at 4000 in debug). 128 still separates both regressions
    // clearly: a per-insert scan would count 128 scans against the bound of
    // 4, and a key growing a byte per insert would reach 128 bytes against
    // the bound of 64.
    const N: usize = if cfg!(debug_assertions) { 128 } else { 4_000 };
    let mut d = Document::new();
    let mut prev: Option<String> = None;

    // Guard #1 (mode-independent): the in-memory order index must keep
    // the whole build to a *constant* number of full container scans
    // (one, to seed it) — a per-insert scan is the O(n^2) this fix removed.
    let scans_before = tilo_doc::ordered_keys_scan_count();
    for i in 0..N {
        let kind = if i % 25 == 0 { "heading" } else { "paragraph" };
        let id = d.insert_block(prev.as_deref(), kind);
        let text = format!("Section {i}: the quick brown fox jumps over the lazy dog number {i}.");
        d.splice_text(&id, 0, 0, &text);
        // A bold run on the leading "Section {i}" label — exercises the mark path.
        let label_len = format!("Section {i}").chars().count();
        d.add_mark(&id, 0, label_len, "strong", true);
        prev = Some(id);
    }
    let build_scans = tilo_doc::ordered_keys_scan_count() - scans_before;
    assert!(
        build_scans <= 4,
        "building {N} blocks performed {build_scans} full container scans — the order index \
         must keep this ~constant; a per-insert scan is the O(n^2) regression this test guards \
         (holds in debug and release)"
    );

    let built = d.materialize();
    assert_eq!(built.blocks.len(), N, "built N blocks");

    // Guard #2 (mode-independent): appended order keys grow logarithmically
    // (three bytes, plus two salt bytes, cover millions of sequential appends)
    // instead of linearly, which would make comparison O(n) and
    // building/sorting O(n^2).
    let max_key_len = d.block_order_key_lengths().into_iter().max().unwrap_or(0);
    assert!(
        max_key_len <= 5,
        "max block order-key length is {max_key_len} bytes for {N} blocks — key_between must keep \
         it bounded; linear growth is O(n^2) (holds in debug and release)"
    );

    // The export + reimport round-trip runs release-only: each
    // reimport rebuilds all N blocks, and in debug that's automerge-0.8's
    // inherent O(n^2) object creation (minutes at N=4000, unrelated to
    // qdoc) — pathologically slow under CI for no added signal, since
    // the structural guards above already prove the fix in debug.
    let mut report = String::new();
    if cfg!(debug_assertions) {
        report.push_str("  (debug: the export/reimport round-trip runs in release; structural guards above cover debug)\n");
    } else {
        for (name, codec) in [
            ("odt", &OdtCodec as &dyn FormatProvider),
            ("docx", &DocxCodec as &dyn FormatProvider),
        ] {
            let t0 = Instant::now();
            let bytes = codec
                .export(d.as_ref(), &tilo_content::NoContent)
                .expect("export large doc");
            let export_ms = t0.elapsed().as_secs_f64() * 1000.0;

            let t1 = Instant::now();
            let round = import(codec, &bytes);
            let import_ms = t1.elapsed().as_secs_f64() * 1000.0;

            let snap = round.materialize();
            assert_eq!(
                snap.blocks.len(),
                N,
                "{name}: block count preserved through a large round-trip"
            );
            assert!(
                snap.blocks[0].text.starts_with("Section 0:"),
                "{name}: first block text preserved"
            );
            assert!(
                snap.blocks[N - 1]
                    .text
                    .contains(&format!("number {}", N - 1)),
                "{name}: last block text preserved"
            );
            assert!(
                snap.blocks
                    .iter()
                    .any(|b| b.marks.iter().any(|m| m.name == "strong")),
                "{name}: bold runs preserved"
            );

            #[allow(
                clippy::cast_precision_loss,
                reason = "display-only byte count; a test fixture is nowhere near f64's exact-integer range"
            )]
            let kib = bytes.len() as f64 / 1024.0;
            let _ = writeln!(
                report,
                "  {name:>4}: export {export_ms:8.1} ms, reimport {import_ms:8.1} ms, {kib:8.1} KiB"
            );
        }
    }
    let head = format!(
        "large_document_parse_and_serialize ({N} paragraphs; {build_scans} container scans, order-key <= {max_key_len}B)"
    );
    println!("\n{head}:\n{report}");
    eprintln!("{head}:\n{report}");
}
