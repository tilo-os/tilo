//! The shared find overlay replaces current or all matches through Notes actions.
use notes::{actions, read};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn fixture() -> (TestApp<notes::NotesShell>, notes::PageId) {
    let mut app = TestApp::<notes::NotesShell>::new().unwrap();
    let page = notes::PageId::new_v4();
    app.invoke(Invoke::apply(&actions::create_page_action(page, None, "Replace")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app.harness().type_text("Ä😀 alpha ALPHA");
    app.harness().key("Enter");
    app.harness().type_text("日本 alpha");
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("f");
    app.harness().set_modifiers(&[]);
    (app, page)
}

fn fields(app: &mut TestApp<notes::NotesShell>, query: &str, replacement: &str) {
    for (target, value) in [
        ("notes.find:query", query),
        ("notes.find:replacement", replacement),
    ] {
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some(target),
                &ui::a11y::Action::SetValue(value.into()),
            )
            .unwrap();
    }
}

fn replace(app: &mut TestApp<notes::NotesShell>) {
    app.harness()
        .perform_semantic_named(
            "block_editor",
            Some("notes.find:replace"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
}

#[test]
fn semantic_replace_changes_one_unicode_match_and_undo_restores_it() {
    let (mut app, page) = fixture();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    fields(&mut app, "alpha", "β😀");
    replace(&mut app);
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(after[0].text, "Ä😀 β😀 ALPHA");
    assert_eq!(after[1], before[1]);
    assert_eq!(after[0].id, before[0].id);
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    assert!(app.redo());
    assert_eq!(app.read_document(|doc| read::page_blocks(doc, page)), after);
}

#[test]
fn keyboard_and_pointer_replace_use_the_same_atomic_action() {
    for pointer in [false, true] {
        let (mut app, page) = fixture();
        fields(&mut app, "alpha", "");
        app.harness().key("Tab");
        app.harness().type_text("日本");
        let before = app.read_document(|doc| read::page_blocks(doc, page));
        if pointer {
            let targets = app.harness().targets();
            let rect = targets["block-editor-replace"];
            assert!(
                targets["block-editor-replacement"].x1 <= rect.x0,
                "replacement field and button must not overlap"
            );
            app.harness().click_at(rect.center().x, rect.center().y);
        } else {
            app.harness().key("Enter");
        }
        let after = app.read_document(|doc| read::page_blocks(doc, page));
        assert_eq!(after[0].text, "Ä😀 日本 ALPHA", "pointer={pointer}");
        assert_eq!(after[1], before[1]);
        assert!(app.undo());
        assert_eq!(
            app.read_document(|doc| read::page_blocks(doc, page)),
            before
        );
    }
}

#[test]
fn replacement_undo_preserves_concurrent_peer_text_and_stale_match_is_refused() {
    for stale in [false, true] {
        let (mut app, page) = fixture();
        fields(&mut app, "alpha", "β");
        let before = app.read_document(|doc| read::page_blocks(doc, page));
        let mut peer = app.peer_document();
        crate::component_support::edit_page_body(
            &mut peer,
            page,
            &tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block: before[0].id.clone(),
                at: 0,
                del: 0,
                ins: "Peer ".into(),
            },
        )
        .unwrap();
        peer.commit();
        if stale {
            app.merge_peer(&mut peer).unwrap();
            let expected = app.read_document(|doc| read::page_blocks(doc, page));
            assert!(
                app.harness()
                    .perform_semantic_named(
                        "block_editor",
                        Some("notes.find:replace"),
                        &ui::a11y::Action::Activate
                    )
                    .is_err()
            );
            assert_eq!(
                app.read_document(|doc| read::page_blocks(doc, page)),
                expected
            );
        } else {
            replace(&mut app);
            app.merge_peer(&mut peer).unwrap();
            assert_eq!(
                app.read_document(|doc| read::page_blocks(doc, page)),
                read::page_blocks(&peer, page)
            );
            assert!(app.undo());
            let after = app.read_document(|doc| read::page_blocks(doc, page));
            assert_eq!(after[0].text, "Peer Ä😀 alpha ALPHA");
            assert_eq!(after[1], before[1]);
            assert_eq!(after[0].id, before[0].id);
        }
    }
}

#[test]
fn empty_replacement_deletes_one_match_without_touching_other_marks() {
    let (mut app, page) = fixture();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    app.invoke(
        Invoke::apply(&actions::edit_body_action(
            page,
            tilo_document::RichTextAction::SetMark {
                region: tilo_document::action::RegionKind::Body,
                spans: vec![tilo_document::action::Span {
                    block: before[0].id.clone(),
                    start: 9,
                    end: 14,
                }],
                name: "link".into(),
                value: Some(tilo_document::action::MarkVal::Str(
                    "https://example.com".into(),
                )),
                also_clear: vec![],
            },
        ))
        .unwrap(),
    )
    .unwrap();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    fields(&mut app, "alpha", "");
    replace(&mut app);
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(after[0].text, "Ä😀  ALPHA");
    assert!(after[0].marks.iter().any(|m| m.mark
        == ui::widget::marks::Mark::ExternalLink("https://example.com".into())
        && m.range == (8..13)));
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
}

#[test]
fn replacement_refuses_viewers_and_waits_for_ime_commit() {
    let (mut app, page) = fixture();
    fields(&mut app, "alpha", "");
    app.harness().key("Tab");
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
    ));
    app.harness().key("Enter");
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.find:replace"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Commit("日本".into()),
    ));
    app.harness().key("Enter");
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page))[0].text,
        "Ä😀 日本 ALPHA"
    );
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    app.set_role(ui::permissions::Role::Viewer);
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.find:replace"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
}

fn assert_replacement_focus(app: &mut TestApp<notes::NotesShell>) {
    let query = ui::headless::test_support::find_semantic(app.harness(), |node| {
        node.action_target.as_deref() == Some("notes.find:query")
    })
    .unwrap();
    let replacement = ui::headless::test_support::find_semantic(app.harness(), |node| {
        node.action_target.as_deref() == Some("notes.find:replacement")
    })
    .unwrap();
    assert!(
        !query.state.focused,
        "query must not steal replacement focus"
    );
    assert!(replacement.state.focused, "replacement retains focus");
}

#[test]
fn navigating_or_replacing_final_match_keeps_exclusive_overlay_focus() {
    for final_match in [false, true] {
        let (mut app, page) = fixture();
        fields(&mut app, if final_match { "😀" } else { "alpha" }, "β");
        app.harness().key("Tab");
        app.harness().key("End");
        if final_match {
            app.harness().key("Enter");
        } else {
            app.harness().key("F3");
        }
        assert_replacement_focus(&mut app);
        let before = app.read_document(|doc| read::page_blocks(doc, page));
        app.harness().type_text("Z");
        app.harness().send(&ui::winit::event::WindowEvent::Ime(
            ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
        ));
        app.harness().key("Enter");
        app.harness().send(&ui::winit::event::WindowEvent::Ime(
            ui::winit::event::Ime::Commit("日本".into()),
        ));
        assert_eq!(
            app.read_document(|doc| read::page_blocks(doc, page)),
            before
        );
        assert_replacement_focus(&mut app);
        let replacement = ui::headless::test_support::find_semantic(app.harness(), |node| {
            node.action_target.as_deref() == Some("notes.find:replacement")
        })
        .unwrap();
        assert_eq!(replacement.value.as_deref(), Some("βZ日本"));
    }
}

#[test]
fn replace_all_is_one_action_preserving_every_block_identity() {
    let (mut app, page) = fixture();
    fields(&mut app, "alpha", "β😀");
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    app.harness()
        .perform_semantic_named(
            "block_editor",
            Some("notes.find:replace-all"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(
        after.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(),
        ["Ä😀 β😀 β😀", "日本 β😀"]
    );
    assert_eq!(
        after.iter().map(|b| &b.id).collect::<Vec<_>>(),
        before.iter().map(|b| &b.id).collect::<Vec<_>>()
    );
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    assert!(app.redo());
    assert_eq!(app.read_document(|doc| read::page_blocks(doc, page)), after);
}

fn replace_all(app: &mut TestApp<notes::NotesShell>) {
    app.harness()
        .perform_semantic_named(
            "block_editor",
            Some("notes.find:replace-all"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
}

#[test]
fn replace_all_undo_preserves_peer_text_and_refuses_stale_searches() {
    for stale in [false, true] {
        let (mut app, page) = fixture();
        fields(&mut app, "alpha", "β");
        let before = app.read_document(|doc| read::page_blocks(doc, page));
        let mut peer = app.peer_document();
        crate::component_support::edit_page_body(
            &mut peer,
            page,
            &tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block: before[0].id.clone(),
                at: 0,
                del: 0,
                ins: "Peer alpha ".into(),
            },
        )
        .unwrap();
        peer.commit();
        if stale {
            app.merge_peer(&mut peer).unwrap();
            let expected = app.read_document(|doc| read::page_blocks(doc, page));
            assert!(
                app.harness()
                    .perform_semantic_named(
                        "block_editor",
                        Some("notes.find:replace-all"),
                        &ui::a11y::Action::Activate
                    )
                    .is_err()
            );
            assert_eq!(
                app.read_document(|doc| read::page_blocks(doc, page)),
                expected
            );
        } else {
            replace_all(&mut app);
            app.merge_peer(&mut peer).unwrap();
            assert_eq!(
                app.read_document(|doc| read::page_blocks(doc, page)),
                read::page_blocks(&peer, page)
            );
            assert!(app.undo());
            let after = app.read_document(|doc| read::page_blocks(doc, page));
            assert_eq!(after[0].text, "Peer alpha Ä😀 alpha ALPHA");
            assert_eq!(after[1], before[1]);
        }
    }
}

#[test]
fn replace_all_pointer_deletion_preserves_prefix_marks_and_undo() {
    let (mut app, page) = fixture();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    app.invoke(
        Invoke::apply(&actions::edit_body_action(
            page,
            tilo_document::RichTextAction::SetMark {
                region: tilo_document::action::RegionKind::Body,
                spans: vec![tilo_document::action::Span {
                    block: before[0].id.clone(),
                    start: 0,
                    end: 2,
                }],
                name: "link".into(),
                value: Some(tilo_document::action::MarkVal::Str(
                    "https://example.com".into(),
                )),
                also_clear: vec![],
            },
        ))
        .unwrap(),
    )
    .unwrap();
    fields(&mut app, "alpha", "");
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    let targets = app.harness().targets();
    let rect = targets["block-editor-replace-all"];
    assert!(targets["block-editor-replacement"].y1 <= rect.y0);
    assert!(targets["block-editor-replace"].y1 <= rect.y0);
    app.harness().click_at(rect.center().x, rect.center().y);
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(after[0].text, "Ä😀  ");
    assert_eq!(after[1].text, "日本 ");
    assert_eq!(after[0].marks, before[0].marks);
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
}

#[test]
fn replace_all_refuses_ime_and_viewers_and_retains_input_focus() {
    let (mut app, page) = fixture();
    fields(&mut app, "alpha", "");
    app.harness().key("Tab");
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
    ));
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.find:replace-all"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    let rect = app.harness().targets()["block-editor-replace-all"];
    app.harness().click_at(rect.center().x, rect.center().y);
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Commit("日本".into()),
    ));
    replace_all(&mut app);
    assert_replacement_focus(&mut app);
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(after[0].text, "Ä😀 日本 日本");
    assert_eq!(after[1].text, "日本 日本");
    fields(&mut app, "日本", "x");
    app.set_role(ui::permissions::Role::Viewer);
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.find:replace-all"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    assert_eq!(app.read_document(|doc| read::page_blocks(doc, page)), after);
}

#[test]
fn replace_all_refuses_new_peer_blocks_until_the_search_is_refreshed() {
    let (mut app, page) = fixture();
    fields(&mut app, "alpha", "alphaβ");
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    let mut peer = app.peer_document();
    crate::component_support::edit_page_body(
        &mut peer,
        page,
        &tilo_document::RichTextAction::InsertParagraph {
            after: before[1].id.clone(),
            text: "Peer alpha".into(),
        },
    )
    .unwrap();
    peer.commit();
    app.merge_peer(&mut peer).unwrap();
    let expected = app.read_document(|doc| read::page_blocks(doc, page));
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.find:replace-all"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        expected
    );
    fields(&mut app, "alpha", "alphaβ");
    replace_all(&mut app);
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page))
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>(),
        ["Ä😀 alphaβ alphaβ", "日本 alphaβ", "Peer alphaβ"]
    );
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        expected
    );
}

#[test]
fn replace_all_keyboard_shortcut_uses_one_undo_step() {
    for modifier in ["shift", "ctrl", "super"] {
        let (mut app, page) = fixture();
        fields(&mut app, "alpha", "β😀");
        app.harness().key("Tab");
        let before = app.read_document(|doc| read::page_blocks(doc, page));
        app.harness().set_modifiers(&[modifier.into()]);
        app.harness().key("Enter");
        app.harness().set_modifiers(&[]);
        assert_eq!(
            app.read_document(|doc| read::page_blocks(doc, page))
                .iter()
                .map(|b| b.text.clone())
                .collect::<Vec<_>>(),
            ["Ä😀 β😀 β😀", "日本 β😀"],
            "modifier={modifier}"
        );
        assert_replacement_focus(&mut app);
        assert!(app.undo());
        assert_eq!(
            app.read_document(|doc| read::page_blocks(doc, page)),
            before
        );
    }
}
