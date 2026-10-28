use notes::{actions, read};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use ui::window_toolbar::SearchOpening;

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
    (app, page)
}

fn blocks(app: &TestApp<notes::NotesShell>, page: notes::PageId) -> Vec<ui::widget::BlockContent> {
    app.read_document(|doc| read::page_blocks(doc, page))
}

fn texts(app: &TestApp<notes::NotesShell>, page: notes::PageId) -> Vec<String> {
    blocks(app, page).into_iter().map(|b| b.text).collect()
}

fn search(app: &mut TestApp<notes::NotesShell>, query: &str) -> Option<(usize, usize)> {
    app.harness()
        .search_in_window(SearchOpening::Replace, query)
}

fn link(
    app: &mut TestApp<notes::NotesShell>,
    page: notes::PageId,
    block: &str,
    start: usize,
    end: usize,
) {
    let invoke = Invoke::apply(&actions::edit_body_action(
        page,
        tilo_document::RichTextAction::SetMark {
            region: tilo_document::action::RegionKind::Body,
            spans: vec![tilo_document::action::Span {
                block: block.into(),
                start,
                end,
            }],
            name: "link".into(),
            value: Some(tilo_document::action::MarkVal::Str(
                "https://example.com".into(),
            )),
            also_clear: vec![],
        },
    ))
    .unwrap();
    app.invoke(invoke).unwrap();
}

fn peer_splice(
    app: &TestApp<notes::NotesShell>,
    page: notes::PageId,
    block: &str,
    ins: &str,
) -> ui::tilo_resource::Doc {
    let mut peer = app.peer_document();
    crate::component_support::edit_page_body(
        &mut peer,
        page,
        &tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block: block.into(),
            at: 0,
            del: 0,
            ins: ins.into(),
        },
    )
    .unwrap();
    peer.commit();
    peer
}

#[test]
fn the_window_search_replaces_one_unicode_match_and_undo_restores_it() {
    let (mut app, page) = fixture();
    let before = blocks(&app, page);
    assert_eq!(search(&mut app, "alpha"), Some((1, 3)));
    assert!(app.harness().window_search_replaces(), "a note can replace");
    assert!(app.harness().replace_in_window("β😀", false));
    let after = blocks(&app, page);
    assert_eq!(after[0].text, "Ä😀 β😀 ALPHA");
    assert_eq!(after[1], before[1]);
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(
        search(&mut app, "alpha"),
        Some((1, 2)),
        "found again in the edited note"
    );
    assert!(app.undo());
    assert_eq!(blocks(&app, page), before);
    assert!(app.redo());
    assert_eq!(blocks(&app, page), after);
}

#[test]
fn replacement_undo_preserves_concurrent_peer_text() {
    let (mut app, page) = fixture();
    search(&mut app, "alpha");
    let before = blocks(&app, page);
    let mut peer = peer_splice(&app, page, &before[0].id, "Peer ");
    app.harness().replace_in_window("β", false);
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(blocks(&app, page), read::page_blocks(&peer, page));
    assert!(app.undo());
    let after = blocks(&app, page);
    assert_eq!(after[0].text, "Peer Ä😀 alpha ALPHA");
    assert_eq!(after[1], before[1]);
    assert_eq!(after[0].id, before[0].id);
}

#[test]
fn a_peer_edit_under_the_search_replaces_the_match_where_it_now_is() {
    let (mut app, page) = fixture();
    search(&mut app, "alpha");
    let before = blocks(&app, page);
    let mut peer = peer_splice(&app, page, &before[0].id, "Peer ");
    app.merge_peer(&mut peer).unwrap();
    assert!(app.harness().replace_in_window("β", false));
    assert_eq!(
        texts(&app, page),
        ["Peer Ä😀 β ALPHA", "日本 alpha"],
        "the match moved with the peer's text"
    );
}

#[test]
fn empty_replacement_deletes_one_match_without_touching_other_marks() {
    let (mut app, page) = fixture();
    let first = blocks(&app, page)[0].id.clone();
    link(&mut app, page, &first, 9, 14);
    let before = blocks(&app, page);
    search(&mut app, "alpha");
    app.harness().replace_in_window("", false);
    let after = blocks(&app, page);
    assert_eq!(after[0].text, "Ä😀  ALPHA");
    assert!(after[0].marks.iter().any(|m| m.mark
        == ui::widget::marks::Mark::ExternalLink("https://example.com".into())
        && m.range == (8..13)));
    assert!(app.undo());
    assert_eq!(blocks(&app, page), before);
}

#[test]
fn replacement_waits_for_the_notes_ime_commit_and_refuses_viewers() {
    let (mut app, page) = fixture();
    search(&mut app, "alpha");
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
    ));
    let before = blocks(&app, page);
    app.harness().replace_in_window("β", false);
    assert_eq!(blocks(&app, page), before, "not while composing");
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Commit("日本".into()),
    ));
    app.harness().replace_in_window("β", false);
    assert_eq!(blocks(&app, page)[0].text, "Ä😀 β ALPHA");

    let before = blocks(&app, page);
    app.set_role(ui::permissions::Role::Viewer);
    app.harness().replace_in_window("x", false);
    app.harness().replace_in_window("x", true);
    assert_eq!(blocks(&app, page), before, "a viewer replaces nothing");
}

#[test]
fn replace_all_is_one_action_preserving_every_block_identity() {
    let (mut app, page) = fixture();
    search(&mut app, "alpha");
    let before = blocks(&app, page);
    assert!(app.harness().replace_in_window("β😀", true));
    let after = blocks(&app, page);
    assert_eq!(
        after.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(),
        ["Ä😀 β😀 β😀", "日本 β😀"]
    );
    assert_eq!(
        after.iter().map(|b| &b.id).collect::<Vec<_>>(),
        before.iter().map(|b| &b.id).collect::<Vec<_>>()
    );
    assert_eq!(search(&mut app, "alpha"), None, "nothing left to find");
    assert!(app.undo());
    assert_eq!(blocks(&app, page), before);
    assert!(app.redo());
    assert_eq!(blocks(&app, page), after);
}

#[test]
fn replace_all_undo_preserves_peer_text() {
    let (mut app, page) = fixture();
    search(&mut app, "alpha");
    let before = blocks(&app, page);
    let mut peer = peer_splice(&app, page, &before[0].id, "Peer alpha ");
    app.harness().replace_in_window("β", true);
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(blocks(&app, page), read::page_blocks(&peer, page));
    assert!(app.undo());
    let after = blocks(&app, page);
    assert_eq!(after[0].text, "Peer alpha Ä😀 alpha ALPHA");
    assert_eq!(after[1], before[1]);
}

#[test]
fn replace_all_reaches_a_block_a_peer_added_under_the_search() {
    let (mut app, page) = fixture();
    search(&mut app, "alpha");
    let before = blocks(&app, page);
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
    let expected = blocks(&app, page);
    app.harness().replace_in_window("alphaβ", true);
    assert_eq!(
        texts(&app, page),
        ["Ä😀 alphaβ alphaβ", "日本 alphaβ", "Peer alphaβ"]
    );
    assert!(app.undo());
    assert_eq!(blocks(&app, page), expected);
}

#[test]
fn replace_all_deleting_matches_preserves_prefix_marks_and_undo() {
    let (mut app, page) = fixture();
    let first = blocks(&app, page)[0].id.clone();
    link(&mut app, page, &first, 0, 2);
    let before = blocks(&app, page);
    search(&mut app, "alpha");
    app.harness().replace_in_window("", true);
    let after = blocks(&app, page);
    assert_eq!(after[0].text, "Ä😀  ");
    assert_eq!(after[1].text, "日本 ");
    assert_eq!(after[0].marks, before[0].marks);
    assert!(app.undo());
    assert_eq!(blocks(&app, page), before);
}
