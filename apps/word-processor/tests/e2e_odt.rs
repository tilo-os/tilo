//! End-to-end: author a `.odt`, open it in the word processor, edit it, save,
//! reopen, and verify the edits persisted through the `.odt` codec + file I/O.
//! This exercises the whole stack — file bytes ↔ ODF codec ↔ canonical CRDT ↔
//! the app's actions — the way the integrated OS app does.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;

use word_processor::actions::{AttrVal, RegionKind, RichTextAction};
use word_processor::{StyleProps, WordProcessor};

use crate::support::{App, apply, last_block, launched, open, save_as, snapshot, texts};

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wp-e2e-{}-{}", std::process::id(), name));
    p
}

/// Append `text` to the end of the paragraph `block`.
fn append_to(app: &mut App, block: &str, text: &str) {
    let at = snapshot(app)
        .blocks
        .into_iter()
        .find(|candidate| candidate.id == block)
        .unwrap()
        .text
        .chars()
        .count();
    apply(
        app,
        &RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: block.to_owned(),
            at,
            del: 0,
            ins: text.to_owned(),
        },
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept as a scoped block"
)]
fn odt_and_markdown_edits_survive_save_and_reload() {
    // Phase: open_edit_save_reload_odt.
    {
        let path = temp_path("doc.odt");
        let mut author = open();
        apply(
            &mut author,
            &RichTextAction::AppendParagraph {
                text: "alpha".into(),
            },
        );
        save_as(&mut author, &path);

        let mut app = launched(&path);
        assert_eq!(texts(&app), ["alpha"]);

        // Edit: append " beta" to the paragraph, then a new paragraph "gamma".
        let first = last_block(&app);
        append_to(&mut app, &first, " beta");
        apply(
            &mut app,
            &RichTextAction::InsertParagraph {
                after: first,
                text: "gamma".into(),
            },
        );

        save_as(&mut app, &path);
        assert_eq!(texts(&launched(&path)), ["alpha beta", "gamma"]);

        std::fs::remove_file(&path).ok();
    }

    // Phase: open_edit_save_reload_markdown.
    {
        let path = temp_path("doc.md");
        std::fs::write(&path, b"# Title\n\nbody\n").unwrap();

        let mut app = launched(&path);
        assert_eq!(texts(&app), ["Title", "body"]);

        let body = last_block(&app);
        append_to(&mut app, &body, " text");
        save_as(&mut app, &path);
        assert_eq!(texts(&launched(&path)), ["Title", "body text"]);

        std::fs::remove_file(&path).ok();
    }

    // Phase: table_created_in_editor_survives_odt_save_reload.
    {
        let path = temp_path("table.odt");
        let mut app = open();
        apply(
            &mut app,
            &RichTextAction::AppendParagraph {
                text: "Title".into(),
            },
        );
        let title = last_block(&app);
        apply(
            &mut app,
            &RichTextAction::InsertFilledTable {
                after: title,
                cells: vec![vec!["X".into(), String::new()], vec![String::new(); 2]],
            },
        );
        save_as(&mut app, &path);

        // Title paragraph + a 2x2 table whose first cell kept "X".
        assert_eq!(
            texts(&launched(&path)),
            ["Title", "X", "", "", ""],
            "table + cell text round-tripped through .odt",
        );
        std::fs::remove_file(&path).ok();
    }

    // Phase: custom_named_style_survives_odt_save_reload.
    {
        let path = temp_path("styled.odt");

        // Author: a paragraph carrying a user-defined named style.
        let mut app = open();
        apply(
            &mut app,
            &RichTextAction::AppendParagraph {
                text: "Heading text".into(),
            },
        );
        let callout = StyleProps {
            name: "Callout".into(),
            font: "serif".into(),
            size: 22.0,
            bold: true,
            italic: true,
            color: [0xc0, 0x20, 0x20],
            align: "center".into(),
            space_before: 0.0,
            space_after: 10.0,
            indent: 0.0,
        };
        for (key, value) in callout.to_attrs() {
            let value = match value {
                tilo_doc::AttrVal::Bool(value) => AttrVal::Bool(value),
                tilo_doc::AttrVal::Int(value) => AttrVal::Int(value),
                tilo_doc::AttrVal::Float(value) => AttrVal::Float(value),
                tilo_doc::AttrVal::Str(value) => AttrVal::Str(value),
            };
            apply(
                &mut app,
                &word_processor::actions::set_style_property_action("callout", &key, value),
            );
        }
        let block = last_block(&app);
        apply(
            &mut app,
            &RichTextAction::ApplyNamedStyle {
                region: RegionKind::Body,
                blocks: vec![block],
                style: "callout".into(),
                kind: None,
                level: None,
                ordered: None,
            },
        );
        save_as(&mut app, &path);

        // Reopen: the style is back in the stylesheet, still applied to the block.
        let reopened = launched(&path);
        assert_eq!(texts(&reopened), ["Heading text"]);
        let sheet = reopened.read_document(|doc| {
            let mut model = WordProcessor::new();
            model.bind_view_only(doc, &automerge::ROOT);
            model.stylesheet(doc)
        });
        let cs = sheet.get("callout");
        assert_eq!(cs.name, "Callout", "custom style present after reload");
        assert_eq!(cs.size, 22.0);
        assert!(cs.bold && cs.italic);
        assert_eq!(cs.color, [0xc0, 0x20, 0x20], "color round-tripped");
        assert_eq!(cs.align, "center");
        assert_eq!(cs.space_after, 10.0);
        // Built-ins must still resolve (the import only writes referenced named styles).
        assert_eq!(
            sheet.get("body").name,
            "Body",
            "built-in body still available after import"
        );

        let snap = snapshot(&reopened);
        assert_eq!(
            snap.blocks[0].attrs.get("style"),
            Some(&tilo_doc::AttrVal::Str("callout".into())),
            "block keeps its named-style ref: {:?}",
            snap.blocks[0].attrs
        );
        assert!(
            snap.styles.contains_key("callout"),
            "styles container holds the custom style after reload"
        );

        std::fs::remove_file(&path).ok();
    }
}
