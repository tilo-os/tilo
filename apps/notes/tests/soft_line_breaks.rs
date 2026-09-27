use crate::component_support::*;
use notes::{actions, doc::NotesDoc, markdown};
use ui::widget::{BlockEditor, BlockKind};

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn soft_and_hard_line_breaks_round_trip_in_paragraphs_and_lists() {
    eprintln!("phase shift_enter_is_one_durable_text_block_in_paragraphs_and_lists");
    {
        for (source, expected_kind) in [
            ("Alpha", "paragraph"),
            ("- Alpha", "bullet:0"),
            ("1. Alpha", "numbered:0"),
            ("- [ ] Alpha", "checklist:0:0"),
        ] {
            let f = LegacyFixture::rich();
            f.write_page(f.child, source.as_bytes());
            let checkout = imported(&f);
            let block = f
                .records
                .read(&checkout.id, |doc| {
                    NotesDoc::page_block_snapshots(doc, f.child)[0].0.clone()
                })
                .unwrap();
            let mut live = driver(&f, 81, &checkout);
            invoke(
                &mut live,
                81,
                actions::block_resource_id(f.child, &block),
                "open",
                None,
            );
            let rect = semantic_rect(&live, &format!("notes.block:{block}"));
            live.click_at(rect.x0 + 8.0, rect.center().y);
            live.key("End");
            live.set_modifiers(&["shift".into()]);
            live.key("Enter");
            live.set_modifiers(&[]);
            live.type_text("Beta");
            live.relayout();
            close_driver(&mut live);
            drop(live);
            f.records.try_flush().unwrap();
            let f = f.reopen();
            // Mounting an existing record loads its persisted bytes into the new owner.
            let mut reopened = driver(&f, 82, &checkout);
            f.records
                .read(&checkout.id, |doc| {
                    assert_eq!(
                        NotesDoc::page_block_snapshots(doc, f.child),
                        vec![(block.clone(), "Alpha\nBeta".into())]
                    );
                    assert_eq!(kind_at(doc, f.child, 0), expected_kind);
                    let mut editor = BlockEditor::new();
                    editor.present_blocks(&NotesDoc::page_blocks(doc, f.child));
                    let specs = markdown::blocks_to_specs(editor.blocks());
                    let exported = markdown::serialize(&specs);
                    assert_eq!(markdown::parse(&exported), specs, "{exported:?}");
                })
                .unwrap();
            close_driver(&mut reopened);
        }
    }

    eprintln!("phase hard_breaks_round_trip_unicode_marks_and_list_continuations");
    {
        use ui::widget::marks::{Mark, MarkSpan};
        let text = markdown::InlineText::new("α\nβ\n", vec![MarkSpan::new(0..5, Mark::Bold)]);
        for block in [
            markdown::BlockSpec::Paragraph(text.clone()),
            markdown::BlockSpec::Quote(text.clone()),
            markdown::BlockSpec::Bullet {
                indent: 2,
                text: text.clone(),
            },
            markdown::BlockSpec::Numbered {
                indent: 1,
                text: text.clone(),
            },
            markdown::BlockSpec::Checklist {
                indent: 2,
                done: true,
                text,
            },
        ] {
            let expected = vec![block];
            let exported = markdown::serialize(&expected);
            assert_eq!(markdown::parse(&exported), expected, "{exported:?}");
        }
    }

    eprintln!("phase mixed_nested_export_resumes_outer_numbering");
    {
        let blocks = vec![
            ui::widget::Block::numbered(0, "First"),
            ui::widget::Block::bullet(1, "Child"),
            ui::widget::Block::checklist(2, false, "Task"),
            ui::widget::Block::numbered(0, "Second"),
        ];
        let exported = markdown::serialize(&markdown::blocks_to_specs(&blocks));
        assert!(exported.contains("2. Second"), "{exported}");
        let restored = markdown::specs_to_blocks(&markdown::parse(&exported));
        assert_eq!(restored[1].kind, BlockKind::Bullet { indent: 1 });
        assert_eq!(
            restored[2].kind,
            BlockKind::Checklist {
                indent: 2,
                done: false
            }
        );
        assert_eq!(
            ui::widget::block_editor::numbered_ordinals(&restored),
            vec![1, 0, 0, 2]
        );
    }
}
