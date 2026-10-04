//! Explicit block selection through the real Notes field and canonical history.
use notes::{actions, doc::NotesDoc};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn app() -> (TestApp<notes::NotesShell>, notes::PageId) {
    let mut app = TestApp::<notes::NotesShell>::new().unwrap();
    let page = notes::PageId::new_v4();
    app.invoke(Invoke::apply(&actions::new_page_action(page, None, 0, "Select")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    for text in ["A", "B"] {
        app.harness().type_text(text);
        app.harness().key("Enter");
    }
    app.harness().type_text("C");
    (app, page)
}

fn caret(app: &mut TestApp<notes::NotesShell>, at: usize) {
    app.harness()
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: at,
                focus: at,
            },
        )
        .unwrap();
}

fn command(app: &mut TestApp<notes::NotesShell>, target: &str) {
    app.harness()
        .perform_semantic_named("block_editor", Some(target), &ui::a11y::Action::Activate)
        .unwrap();
}

fn menu(app: &mut TestApp<notes::NotesShell>, item: usize) {
    use ui::winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
    let rect = app.harness().targets()["block_editor"];
    app.harness().set_modifiers(&[]);
    app.harness().dispatch_to_root_with(
        &WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: ui::winit::dpi::PhysicalPosition::new(rect.x0 + 20.0, rect.y0 + 10.0),
        },
        &Modifiers::default(),
    );
    app.harness().dispatch_to_root_with(
        &WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Right,
        },
        &Modifiers::default(),
    );
    for _ in 0..=item {
        app.harness().key("ArrowDown");
    }
    app.harness().key("Enter");
}

#[test]
fn whole_group_deletion_preserves_nested_marks_fences_and_tables_through_undo_and_reopen() {
    use crate::component_support::*;
    for (source, semantic) in [
        ("- **Ä parent**\n  - *日本 child*\n- After\n", false),
        (
            "```rust\nlet 日本 = 1;\n\nprintln!(\"ok\");\n```\n\nAfter\n",
            true,
        ),
        (
            "| Name | Value |\n| --- | --- |\n| 日本 | **bold** |\n\nAfter\n",
            false,
        ),
    ] {
        let fixture = LegacyFixture::rich();
        fixture.write_page(fixture.child, source.as_bytes());
        let checkout = imported(&fixture);
        let before = fixture
            .records
            .read(&checkout.id, |doc| {
                NotesDoc::page_blocks(doc, fixture.child)
            })
            .unwrap();
        assert!(before.len() >= 2);
        let after = vec![before.last().unwrap().clone()];
        assert_eq!(after[0].text, "After");
        let mut live = driver(&fixture, 97, &checkout);
        invoke(
            &mut live,
            97,
            actions::block_resource_id(fixture.child, &before[0].id),
            "open",
            None,
        );
        if semantic {
            live.perform_semantic_named(
                "block_editor",
                Some(&format!("notes.block-delete:{}", before[0].id)),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        } else {
            live.perform_semantic_named(
                "block_editor",
                Some(&format!("notes.block-select:{}", before[0].id)),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
            live.key("Delete");
        }
        assert_eq!(
            fixture
                .records
                .read(&checkout.id, |doc| NotesDoc::page_blocks(
                    doc,
                    fixture.child
                ))
                .unwrap(),
            after
        );
        live.set_modifiers(&["ctrl".into()]);
        live.key("z");
        live.set_modifiers(&[]);
        assert_eq!(
            fixture
                .records
                .read(&checkout.id, |doc| NotesDoc::page_blocks(
                    doc,
                    fixture.child
                ))
                .unwrap(),
            before
        );
        live.set_modifiers(&["ctrl".into(), "shift".into()]);
        live.key("z");
        live.set_modifiers(&[]);
        close_driver(&mut live);
        let mut reopened = driver(&fixture, 98, &checkout);
        assert_eq!(
            fixture
                .records
                .read(&checkout.id, |doc| NotesDoc::page_blocks(
                    doc,
                    fixture.child
                ))
                .unwrap(),
            after
        );
        close_driver(&mut reopened);
    }
}

#[test]
fn deleting_every_block_ensures_one_persistent_paragraph_in_the_same_undo_step() {
    for keyboard in [true, false] {
        let (mut app, page) = app();
        let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
        command(&mut app, "notes.editor:select-all-blocks");
        if keyboard {
            app.harness().key("Backspace");
        } else {
            command(&mut app, "notes.editor:delete");
        }
        let empty = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].text, "");
        assert!(!before.iter().any(|block| block.id == empty[0].id));
        assert!(app.undo());
        assert_eq!(
            app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
            before
        );
        assert!(app.redo());
        assert_eq!(
            app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
            empty
        );
        app.harness().key("Enter");
        let split = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
        assert_eq!(
            split.len(),
            2,
            "Enter after delete-all persists without typing"
        );
        app.harness().type_text("日本");
        let typed = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
        assert_eq!(typed[1].text, "日本");
        assert!(app.undo());
        assert_eq!(
            app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
            split
        );
    }
}

#[test]
fn reversing_block_extension_shrinks_selection_and_enter_or_typing_preserves_other_blocks() {
    let (mut app, page) = app();
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    caret(&mut app, 3);
    app.harness().key("Escape");
    app.harness().set_modifiers(&["shift".into()]);
    app.harness().key("ArrowUp");
    app.harness().key("ArrowDown");
    app.harness().set_modifiers(&[]);
    app.harness().key("Delete");
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        vec![before[0].clone(), before[2].clone()]
    );
    assert!(app.undo());
    caret(&mut app, 1);
    app.harness().key("Escape");
    command(&mut app, "notes.editor:extend-down");
    app.harness().type_text("x");
    let typed = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert_eq!(
        typed
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        ["A", "Bx", "C"]
    );
    app.harness().key("Escape");
    app.harness().key("Enter");
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        typed,
        "Enter edits the selected block instead of replacing it"
    );
}

#[test]
fn deletion_refuses_viewers_and_composition_and_ime_exits_block_mode_safely() {
    let (mut app, page) = app();
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    caret(&mut app, 1);
    app.harness().key("Escape");
    command(&mut app, "notes.editor:extend-down");
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Preedit("日本".into(), Some((6, 6))),
    ));
    app.harness().key("Delete");
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.editor:delete"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
    app.harness().send(&ui::winit::event::WindowEvent::Ime(
        ui::winit::event::Ime::Commit("日本".into()),
    ));
    let typed = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert_eq!(
        typed
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        ["A", "B日本", "C"]
    );
    app.set_role(ui::permissions::Role::Viewer);
    command(&mut app, "notes.editor:select-all-blocks");
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.editor:delete"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    app.harness().key("Backspace");
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        typed
    );
}

#[test]
fn invalidated_block_selection_cannot_delete_interleaved_peer_content_or_fall_back_to_text() {
    let (mut app, page) = app();
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    caret(&mut app, 1);
    app.harness().key("Escape");
    command(&mut app, "notes.editor:extend-down");
    let mut peer = app.peer_document();
    NotesDoc::edit_page_body(
        &mut peer,
        page,
        &tilo_document::RichTextAction::InsertParagraph {
            after: before[0].id.clone(),
            text: "Peer".into(),
        },
    )
    .unwrap();
    peer.commit();
    app.merge_peer(&mut peer).unwrap();
    let merged = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    app.harness().key("Delete");
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        merged
    );
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.editor:delete"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    app.harness().type_text("x");
    let typed = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert_eq!(typed.len(), merged.len());
    assert_eq!(
        typed.iter().find(|block| block.text == "Peer"),
        merged.iter().find(|block| block.text == "Peer")
    );
}

#[test]
fn deletion_undo_preserves_a_concurrent_edit_to_a_surviving_block() {
    let (mut app, page) = app();
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    let mut peer = app.peer_document();
    NotesDoc::edit_page_body(
        &mut peer,
        page,
        &tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block: before[2].id.clone(),
            at: 0,
            del: 0,
            ins: "Peer ".into(),
        },
    )
    .unwrap();
    peer.commit();
    caret(&mut app, 1);
    app.harness().key("Escape");
    command(&mut app, "notes.editor:extend-down");
    command(&mut app, "notes.editor:delete");
    app.merge_peer(&mut peer).unwrap();
    let deleted = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].text, "Peer C");
    assert!(app.undo());
    let restored = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert_eq!(restored[..2], before[..2]);
    assert_eq!(restored[2].id, before[2].id);
    assert_eq!(restored[2].text, "Peer C");
}

#[test]
fn escape_selects_a_whole_block_and_delete_is_one_undo_step() {
    let (mut app, page) = app();
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    caret(&mut app, 1);
    app.harness().key("Escape");
    app.harness().key("Delete");
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before[1..]
    );
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
    assert!(app.redo());
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before[1..]
    );
}

#[test]
fn cut_requires_an_accepted_clipboard_and_copy_keeps_the_block_selection() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use ui::clipboard::{ClipboardBackend, ClipboardTarget};
    #[derive(Default)]
    struct Clipboard {
        accept: AtomicBool,
        text: Mutex<Option<String>>,
    }
    impl ClipboardBackend for Clipboard {
        fn set_text(&self, _: ClipboardTarget, text: &str) -> bool {
            if !self.accept.load(Ordering::Relaxed) {
                return false;
            }
            *self.text.lock().unwrap() = Some(text.to_owned());
            true
        }
        fn text(&self, _: ClipboardTarget) -> Option<String> {
            self.text.lock().unwrap().clone()
        }
        fn name(&self) -> &'static str {
            "notes-test-clipboard"
        }
    }
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page) = app();
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    caret(&mut app, 1);
    app.harness().key("Escape");
    command(&mut app, "notes.editor:extend-down");
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("x");
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
    assert_eq!(clipboard.text(ClipboardTarget::Clipboard), None);
    assert!(
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some("notes.editor:cut-blocks"),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
    menu(&mut app, 0);
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
    clipboard.accept.store(true, Ordering::Relaxed);
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("c");
    assert_eq!(
        clipboard.text(ClipboardTarget::Clipboard).as_deref(),
        Some("A\nB")
    );
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
    menu(&mut app, 1);
    menu(&mut app, 0);
    app.harness().set_modifiers(&[]);
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        vec![before[2].clone()]
    );
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
}

#[test]
fn context_menu_paste_leaves_block_mode_and_select_all_keeps_empty_rows() {
    use std::sync::Arc;
    use ui::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
    let clipboard = Arc::new(InProcessClipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page) = app();
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    caret(&mut app, 1);
    app.harness().key("Escape");
    command(&mut app, "notes.editor:extend-down");
    assert!(clipboard.set_text(ClipboardTarget::Clipboard, "日本"));
    menu(&mut app, 2);
    let pasted = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert_eq!(
        pasted
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        ["A", "B日本", "C"]
    );
    assert_eq!(
        pasted.iter().map(|block| &block.id).collect::<Vec<_>>(),
        before.iter().map(|block| &block.id).collect::<Vec<_>>()
    );
    caret(&mut app, 9);
    app.harness().key("End");
    app.harness().key("Enter");
    let with_empty = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert_eq!(with_empty.last().unwrap().text, "");
    app.harness().key("Escape");
    menu(&mut app, 3);
    app.harness().key("Delete");
    let empty = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert_eq!(empty.len(), 1);
    assert_eq!(empty[0].text, "");
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        with_empty
    );
}

#[test]
fn option_generated_text_leaves_whole_selection_without_replacing_other_blocks() {
    let (mut app, page) = app();
    caret(&mut app, 1);
    app.harness().key("Escape");
    command(&mut app, "notes.editor:extend-down");
    app.harness().set_modifiers(&["alt".into()]);
    app.harness().key("™");
    app.harness().set_modifiers(&[]);
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page))
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        ["A", "B™", "C"]
    );
}
