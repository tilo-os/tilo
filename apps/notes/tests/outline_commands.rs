//! Actual Notes input → canonical body → history and Markdown projection.
use notes::actions::create_page_action;
use notes::read;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn open() -> (TestApp<NotesShell>, PageId) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let page = PageId::new_v4();
    app.invoke(Invoke::apply(&create_page_action(page, None, "Outline")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    (app, page)
}

fn rows(app: &TestApp<NotesShell>, page: PageId) -> Vec<(String, String, u8)> {
    app.read_document(|doc| {
        read::page_blocks(doc, page)
            .into_iter()
            .map(|block| (block.id, block.text, block.nesting))
            .collect()
    })
}

#[test]
fn tab_nests_prose_and_shift_tab_restores_it_as_atomic_notes_actions() {
    let (mut app, page) = open();
    app.harness().type_text("Parent");
    app.harness().key("Enter");
    app.harness().type_text("é😀 child");
    let before = rows(&app, page);
    app.harness().key("Tab");
    let mut nested = before.clone();
    nested[1].2 = 1;
    assert_eq!(rows(&app, page), nested);
    assert!(app.undo());
    assert_eq!(rows(&app, page), before);
    assert!(app.redo());
    assert_eq!(rows(&app, page), nested);
    app.harness().set_modifiers(&["shift".into()]);
    app.harness().key("Tab");
    app.harness().set_modifiers(&[]);
    assert_eq!(rows(&app, page), before);
    app.harness().type_text("!");
    assert_eq!(rows(&app, page)[1].1, "é😀 child!");
}

#[test]
fn selected_prose_blocks_indent_together_without_replacing_text() {
    let (mut app, page) = open();
    app.harness().type_text("Parent");
    app.harness().key("Enter");
    app.harness().type_text("First child");
    app.harness().key("Enter");
    app.harness().type_text("Second child");
    let before = rows(&app, page);
    app.harness()
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: 7,
                focus: 31,
            },
        )
        .unwrap();
    app.harness().key("Tab");
    let mut nested = before.clone();
    nested[1].2 = 1;
    nested[2].2 = 1;
    assert_eq!(rows(&app, page), nested);
    assert!(app.undo());
    assert_eq!(rows(&app, page), before);
}

#[test]
fn backspace_outdents_before_joining_and_keeps_both_blocks_formatting() {
    let (mut app, page) = open();
    app.harness().type_text("Parent");
    app.harness().key("Enter");
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("b");
    app.harness().set_modifiers(&[]);
    app.harness().type_text("é😀 child");
    app.harness().key("Tab");
    let mut outdented = rows(&app, page);
    outdented[1].2 = 0;
    app.harness().key("Home");
    app.harness().key("Backspace");
    assert_eq!(rows(&app, page), outdented, "first Backspace only outdents");
    app.harness().key("Backspace");
    let joined = rows(&app, page);
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0].1, "Parenté😀 child");
    app.read_document(|doc| {
        let blocks = read::page_blocks(doc, page);
        assert!(
            blocks[0].marks.iter().any(|span| {
                span.mark == ui::widget::marks::Mark::Bold && span.range == (6..18)
            }),
            "merged child retains bold over its UTF-8 byte range: {:?}",
            blocks[0].marks
        );
    });
    assert!(app.undo());
    assert_eq!(rows(&app, page), outdented);
}

#[test]
fn nesting_survives_concurrent_text_and_markdown_roundtrip_and_refuses_viewers() {
    let (mut app, page) = open();
    app.harness().type_text("Parent");
    app.harness().key("Enter");
    app.harness().type_text("Child");
    let before = rows(&app, page);
    let mut peer = app.peer_document();
    crate::component_support::edit_page_body(
        &mut peer,
        page,
        &tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block: before[1].0.clone(),
            at: 0,
            del: 0,
            ins: "同僚 ".into(),
        },
    )
    .unwrap();
    peer.commit();
    app.harness().key("Tab");
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(
        rows(&app, page)[1],
        (before[1].0.clone(), "同僚 Child".into(), 1)
    );
    app.read_document(|doc| {
        assert_eq!(read::page_blocks(doc, page), read::page_blocks(&peer, page));
        let mut editor = ui::widget::BlockEditor::new();
        editor.present_blocks(&read::page_blocks(doc, page));
        let specs = notes::markdown::blocks_to_specs(editor.blocks());
        let source = notes::markdown::serialize(&specs);
        assert_eq!(notes::markdown::parse(&source), specs, "{source}");
    });
    assert!(app.undo());
    assert_eq!(
        rows(&app, page)[1],
        (before[1].0.clone(), "同僚 Child".into(), 0)
    );
    app.set_role(ui::permissions::Role::Viewer);
    let readonly = rows(&app, page);
    app.harness().key("Tab");
    app.harness().key("Backspace");
    assert_eq!(rows(&app, page), readonly);
}

#[test]
fn composition_keys_do_not_restructure_notes_before_the_ime_commit() {
    let (mut app, page) = open();
    app.harness().type_text("Parent");
    app.harness().key("Enter");
    app.harness().type_text("Child");
    let before = rows(&app, page);
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
    ));
    for key in ["Tab", "Enter", "Backspace", "Escape"] {
        app.harness().key(key);
        assert_eq!(rows(&app, page), before, "IME owns {key} while composing");
    }
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Commit("日本".into()),
    ));
    assert_eq!(rows(&app, page)[1].1, "Child日本");
    app.harness().key("Tab");
    assert_eq!(rows(&app, page)[1].2, 1);
}

#[test]
fn escape_then_tab_moves_host_focus_without_editing_prose_or_code() {
    for code in [false, true] {
        let (mut app, page) = open();
        app.harness().type_text("Parent");
        app.harness().key("Enter");
        if code {
            app.harness().type_text("```rust");
            app.harness().key("Enter");
        }
        app.harness().type_text("Child");
        let before = rows(&app, page);
        let focused = app.harness().focused();
        app.harness().key("Escape");
        app.harness().key("Tab");
        assert_ne!(
            app.harness().focused(),
            focused,
            "host focus must leave the editor"
        );
        assert_eq!(
            rows(&app, page),
            before,
            "focus traversal must not insert spaces or nesting"
        );
    }
}
