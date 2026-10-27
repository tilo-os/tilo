//! End-to-end: author a rich document (heading, bold run, bullet list, and a
//! 2x2 table) in the word processor, save it as `.docx`, reopen, and verify the
//! structure (block kinds, text, inline marks) round-tripped through the `tilo-doc`
//! `DocxCodec` + the editor's file I/O — exactly as the integrated app does.
//! Mirrors `e2e_odt.rs` for the Microsoft Word format. Frozen external and
//! LibreOffice-authored inputs below also exercise the production framework host,
//! typed resource edits, Save As and a fresh application launch (not just codecs).

use std::path::{Path, PathBuf};

use tilo_doc::AttrVal;
use word_processor::WpFormat;
use word_processor::actions::{MarkVal, RegionKind, RichTextAction, Span};

use crate::support::{apply, last_block, launched, open, save_as, snapshot};

#[path = "e2e_docx/compatibility.rs"]
mod compatibility;

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wp-docx-e2e-{}-{}", std::process::id(), name));
    p
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept as a scoped block"
)]
fn docx_files_are_recognised_and_rich_documents_round_trip() {
    // Phase: from_path_maps_docx_extension.
    {
        assert_eq!(
            WpFormat::from_path(Path::new("/tmp/report.docx")),
            WpFormat::Docx
        );
        assert_eq!(
            WpFormat::from_path(Path::new("/tmp/REPORT.DOCX")),
            WpFormat::Docx,
            "case-insensitive"
        );
    }

    // Phase: rich_document_round_trips_through_docx.
    {
        let path = temp_path("Bericht – 日本語.docx");

        // --- Author the document through the app's own actions. ---
        let mut app = open();
        // 1. A heading.
        apply(
            &mut app,
            &RichTextAction::AppendParagraph {
                text: "Heading".into(),
            },
        );
        let heading = last_block(&app);
        apply(
            &mut app,
            &RichTextAction::SetParagraphStyle {
                region: RegionKind::Body,
                blocks: vec![heading.clone()],
                kind: "heading".into(),
                level: Some(1),
                ordered: None,
            },
        );
        // 2. A paragraph with a bold run.
        apply(
            &mut app,
            &RichTextAction::InsertParagraph {
                after: heading,
                text: "bold".into(),
            },
        );
        let bold = last_block(&app);
        apply(
            &mut app,
            &RichTextAction::SetMark {
                region: RegionKind::Body,
                spans: vec![Span {
                    block: bold.clone(),
                    start: 0,
                    end: 4,
                }],
                name: "strong".into(),
                value: Some(MarkVal::Bool(true)),
                also_clear: Vec::new(),
            },
        );
        // 3. A bullet (unordered) list item.
        apply(
            &mut app,
            &RichTextAction::InsertParagraph {
                after: bold,
                text: "Bullet".into(),
            },
        );
        let bullet = last_block(&app);
        apply(
            &mut app,
            &RichTextAction::SetParagraphStyle {
                region: RegionKind::Body,
                blocks: vec![bullet.clone()],
                kind: "list_item".into(),
                level: None,
                ordered: Some(false),
            },
        );
        // 4. A 2x2 table whose first cell holds "X".
        apply(
            &mut app,
            &RichTextAction::InsertFilledTable {
                after: bullet,
                cells: vec![vec!["X".into(), String::new()], vec![String::new(); 2]],
            },
        );

        // --- Save as `.docx` with the app's Save As, then open the file. ---
        save_as(&mut app, &path);
        let reopened = launched(&path);
        let snap = snapshot(&reopened);

        let heading = snap
            .blocks
            .iter()
            .find(|b| b.kind == "heading")
            .expect("heading survived");
        assert_eq!(heading.text, "Heading");
        assert_eq!(
            heading.attrs.get("level"),
            Some(&AttrVal::Int(1)),
            "heading level 1"
        );

        // Bold run: a paragraph whose `bold` text carries a `strong` mark over it.
        let para = snap
            .blocks
            .iter()
            .find(|b| b.kind == "paragraph" && b.text == "bold")
            .expect("bold paragraph survived");
        let strong = para
            .marks
            .iter()
            .find(|m| m.name == "strong")
            .expect("strong mark survived");
        assert_eq!(
            (strong.start, strong.end),
            (0, 4),
            "strong covers the whole run"
        );

        let item = snap
            .blocks
            .iter()
            .find(|b| b.kind == "list_item")
            .expect("list item survived");
        assert_eq!(item.text, "Bullet");
        assert_eq!(
            item.attrs.get("ordered"),
            Some(&AttrVal::Bool(false)),
            "bullet (unordered)"
        );

        // 2x2 table: four `cell` blocks, the first holding "X".
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 4, "2x2 = four cells: {snap:?}");
        assert_eq!(cells[0].text, "X", "first cell kept its text");
        assert!(
            cells[1..].iter().all(|cell| cell.text.is_empty()),
            "remaining cells empty"
        );

        std::fs::remove_file(&path).ok();
    }
}
