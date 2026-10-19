use crate::component_support::*;
use notes::{markdown, read};
use ui::widget::{BlockEditor, BlockKind};

#[allow(
    clippy::too_many_lines,
    reason = "geometry regression test compares wrapping, marks, carets, and measurement in one fixture"
)]
#[test]
fn block_kind_changes_preserve_marks_text_and_shaped_geometry() {
    eprintln!("phase wrapped_marks_share_shaped_geometry_with_carets_and_measurement");
    {
        use taffy::{AvailableSpace, Size};
        use ui::widget::{
            RichTextLine,
            marks::{Mark, MarkSpan},
        };
        use ui::{
            Widget,
            richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign},
        };
        let text = "Böld and italic with code";
        for width in [400.0_f32, 130.0] {
            let theme = ui::Theme::dark();
            // The line pads its text 10 px across,
            // a control inset above and below.
            let pad_x = 10.0_f32;
            let pad_y = theme.tokens.spacing.s2;
            let mut renderer = renderer();
            let mut line = RichTextLine::new().with_text(text);
            // Settle the input's initial text/font measurement before checking
            // whether a formatting-only update schedules another measurement.
            line.measure(
                Size {
                    width: Some(width),
                    height: None,
                },
                Size {
                    width: AvailableSpace::Definite(width),
                    height: AvailableSpace::MaxContent,
                },
                &mut renderer,
            );
            line.apply_layout_external(vello::kurbo::Rect::new(
                40.0,
                60.0,
                40.0 + f64::from(width),
                100.0,
            ));
            line.set_marks(vec![
                MarkSpan::new(0..5, Mark::Bold),
                MarkSpan::new(10..16, Mark::Italic),
                MarkSpan::new(22..26, Mark::Code),
            ]);
            assert!(
                !line.layout_needs_measure(),
                "fixed-height text must not keep requesting measurement for marks"
            );
            line.set_wrap(true);
            line.build_layout(
                &mut taffy::TaffyTree::new(),
                &theme,
                &mut std::collections::HashMap::new(),
            );
            let size = line.measure(
                Size {
                    width: Some(width),
                    height: None,
                },
                Size {
                    width: AvailableSpace::Definite(width),
                    height: AvailableSpace::MaxContent,
                },
                &mut renderer,
            );
            assert!(
                !line.layout_needs_measure(),
                "measurement must settle the attributed layout"
            );
            line.apply_layout_external(vello::kurbo::Rect::new(
                40.0,
                60.0,
                40.0 + f64::from(width),
                60.0 + f64::from(size.height),
            ));
            line.draw(
                &mut vello::Scene::new(),
                vello::kurbo::Affine::IDENTITY,
                &mut renderer,
                &theme,
                &ui::presence::PresenceCtx::empty(),
            );
            let expected = RichTextLayout::shape_styled(
                &mut renderer.font_system,
                &[
                    RichSpan::new("Böld").bold(true),
                    RichSpan::new(" and "),
                    RichSpan::new("italic").italic(true),
                    RichSpan::new(" with "),
                    RichSpan {
                        family: Some(FontFamily::Mono),
                        ..RichSpan::new("code")
                    },
                ],
                &theme.text(ui::TextRole::Body).with_line_height(1.25),
                Some(width - 2.0 * pad_x),
                TextAlign::Left,
                Vec::new(),
                Vec::new(),
            );
            assert!(
                (size.height - (expected.height() + 2.0 * pad_y)).abs() < 0.1,
                "marked text must determine wrapping and height: measured {} vs shaped {} + 2*{pad_y}, width {width}",
                size.height,
                expected.height()
            );
            for byte in [0, 5, 10, 16, 22, 26] {
                let (x, y, _) = expected.caret_geometry(byte);
                let caret = line.caret_rect(&theme, byte);
                assert!(
                    (caret.x0 - (40.0 + f64::from(pad_x) + f64::from(x))).abs() < 0.1,
                    "marked caret x at {byte}, width {width}: {caret:?} vs {x}"
                );
                assert!(
                    (caret.y0 - (60.0 + f64::from(pad_y) + f64::from(y))).abs() < 0.1,
                    "marked caret row at {byte}, width {width}"
                );
                assert_eq!(
                    line.inner()
                        .byte_at_point(vello::kurbo::Point::new(caret.x0 + 0.1, caret.center().y)),
                    byte,
                    "pointer placement must use the same styled layout"
                );
            }
        }
    }

    eprintln!("phase changing_rich_block_kinds_preserves_marks_cursor_and_nesting");
    {
        let mut editor = BlockEditor::new();
        editor.set_blocks(markdown::specs_to_blocks(&markdown::parse(
            "- Parent\n\n  **Böld** and *italic* with [site](https://example.com)\n",
        )));
        let marks = editor.blocks()[1].line.marks().to_vec();
        assert_eq!(marks.len(), 3);
        editor.focus_block(1, ui::widget::CursorPlacement::Byte(6));
        for kind in [
            BlockKind::Heading(2),
            BlockKind::Bullet { indent: 0 },
            BlockKind::Numbered { indent: 0 },
            BlockKind::Quote,
            BlockKind::CodeBlock {
                lang: Some("python".into()),
            },
            BlockKind::Paragraph,
        ] {
            assert!(editor.try_convert_to(1, kind));
            let block = &editor.blocks()[1];
            assert_eq!(
                block.line.marks(),
                marks,
                "changing kind must retain authored formatting"
            );
            assert_eq!(
                block.line.cursor_pos(),
                6,
                "changing presentation must not move the caret"
            );
            assert_eq!(block.indent(), 1);
        }
        assert_eq!(
            markdown::blocks_to_specs(editor.blocks()),
            markdown::parse(
                "- Parent\n\n  **Böld** and *italic* with [site](https://example.com)\n",
            )
        );
    }

    eprintln!("phase slash_conversion_preserves_native_marks_text_identity_undo_and_reopen");
    {
        let f = NotebookFixture::rich();
        f.write_page(
            f.child,
            "- Parent\n\n  **Böld** and *italic* with [site](https://example.com)\n".as_bytes(),
        );
        let checkout = notebook(&f);
        let (id, original_marks) = f
            .records
            .read(&checkout.id, |doc| {
                (
                    read::page_block_snapshots(doc, f.child)[1].0.clone(),
                    read::page_block_marks(doc, f.child),
                )
            })
            .unwrap();
        let mut live = driver(&f, 95, &checkout);
        open_note(&mut live, 95, f.child, Some(&id));
        let rect = semantic_rect(&live, &format!("notes.block:{id}"));
        live.click_at(rect.x0 + 8.0, rect.center().y);
        live.key("Home");
        live.type_text("/number");
        let before_commit = revision(&f, &checkout);
        live.key("Enter");
        assert_eq!(
            revision(&f, &checkout),
            before_commit + 2,
            "the typed command closes, then the conversion"
        );
        let page = f.child;
        let verify = |doc: &automerge::AutoCommit| {
            notes::read::validate(doc).unwrap();
            assert_eq!(read::page_blocks(doc, page)[1].kind.stored(), "numbered:1");
            assert_eq!(
                read::page_block_snapshots(doc, page)[1],
                (id.clone(), "Böld and italic with site".into()),
                "the block keeps its identity"
            );
            assert_eq!(read::page_block_marks(doc, page), original_marks);
        };
        f.records.read(&checkout.id, verify).unwrap();
        live.set_modifiers(&["ctrl".into()]);
        live.key("z");
        live.set_modifiers(&["ctrl".into(), "shift".into()]);
        live.key("z");
        live.set_modifiers(&[]);
        f.records.read(&checkout.id, verify).unwrap();
        close_driver(&mut live);
        drop(live);
        f.records.try_flush().unwrap();
        let reopened_fixture = f.reopen();
        let mut reopened = driver(&reopened_fixture, 96, &checkout);
        reopened_fixture.records.read(&checkout.id, verify).unwrap();
        close_driver(&mut reopened);
    }

    eprintln!("phase typed_block_prefixes_preserve_existing_formatted_contents");
    {
        for (prefix, expected_kind) in [("## ", "heading2"), ("> ", "quote"), ("- ", "bullet:0")] {
            let f = NotebookFixture::rich();
            f.write_page(
                f.child,
                "**Böld** and *italic* with [site](https://example.com)\n".as_bytes(),
            );
            let checkout = notebook(&f);
            let (id, marks) = f
                .records
                .read(&checkout.id, |doc| {
                    (
                        read::page_block_snapshots(doc, f.child)[0].0.clone(),
                        read::page_block_marks(doc, f.child),
                    )
                })
                .unwrap();
            let mut live = driver(&f, 97, &checkout);
            open_note(&mut live, 97, f.child, Some(&id));
            let rect = semantic_rect(&live, &format!("notes.block:{id}"));
            live.click_at(rect.x0 + 8.0, rect.center().y);
            live.key("Home");
            live.type_text(prefix);
            f.records
                .read(&checkout.id, |doc| {
                    notes::read::validate(doc).unwrap();
                    assert_eq!(
                        read::page_blocks(doc, f.child)[0].kind.stored(),
                        expected_kind,
                        "{prefix}"
                    );
                    assert_eq!(
                        read::page_block_snapshots(doc, f.child)[0],
                        (id.clone(), "Böld and italic with site".into()),
                        "{prefix}"
                    );
                    assert_eq!(read::page_block_marks(doc, f.child), marks, "{prefix}");
                })
                .unwrap();
            let image = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/notes-review")
                .join(format!("formatted-{}.png", expected_kind.replace(':', "-")));
            live.render_png(&image);
            assert!(
                std::fs::read(&image)
                    .unwrap()
                    .starts_with(b"\x89PNG\r\n\x1a\n")
            );
            close_driver(&mut live);
        }
    }

    eprintln!("phase unknown_dates_show_nothing_and_known_dates_read_in_the_local_time_zone");
    {
        use chrono::{Local, TimeZone};
        use notes::metadata::NoteDates;
        notes::metadata::register_messages();
        let now = Local
            .with_ymd_and_hms(2026, 6, 30, 10, 42, 0)
            .single()
            .unwrap();
        assert!(
            NoteDates::default().labels_at(now).is_empty(),
            "a note with no known date shows no header placeholder"
        );
        assert_eq!(
            NoteDates::default().display_labels(false).len(),
            1,
            "the detailed resource display retains its unavailable-date label"
        );
        let created = Local
            .with_ymd_and_hms(2023, 11, 14, 10, 42, 0)
            .single()
            .unwrap();
        let edited = Local
            .with_ymd_and_hms(2023, 11, 17, 11, 40, 0)
            .single()
            .unwrap();
        let edited_only = NoteDates {
            created: None,
            edited: Some(edited.timestamp()),
        };
        let lines = edited_only.labels_at(now);
        assert_eq!(lines, vec!["Edited Nov 17, 2023".to_string()]);
        let both = NoteDates {
            created: Some(created.timestamp()),
            edited: edited_only.edited,
        };
        let detailed = both.display_labels(false);
        assert_eq!(detailed.len(), 2);
        assert!(
            detailed
                .iter()
                .all(|label| label.contains("Nov 14, 2023, ") || label.contains("Nov 17, 2023, "))
        );
        let lines = both.labels_at(now);
        assert_eq!(
            lines,
            vec!["Created Nov 14, 2023, edited Nov 17, 2023".to_string()],
            "created and edited share one header line in the reader's local time"
        );
        assert!(
            lines
                .iter()
                .chain(detailed.iter())
                .all(|line| !line.contains("UTC")),
            "dates are the reader's local time, never labelled UTC: {lines:?}, {detailed:?}"
        );
        let today = NoteDates {
            created: Some(now.timestamp()),
            edited: Some(now.timestamp()),
        };
        assert_eq!(
            today.labels_at(now),
            vec!["Created today at 10:42 AM".to_string()]
        );
    }
}
