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

    {
        let path = temp_path("Bericht – 日本語.docx");

        let mut app = open();
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
        apply(
            &mut app,
            &RichTextAction::InsertFilledTable {
                after: bullet,
                cells: vec![vec!["X".into(), String::new()], vec![String::new(); 2]],
            },
        );

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
