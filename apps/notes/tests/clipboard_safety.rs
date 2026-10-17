//! Clipboard refusal must never change a note, its selection or its history.
use notes::{actions, read};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use ui::clipboard::{ClipboardBackend, ClipboardTarget};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

#[derive(Default)]
struct Clipboard {
    accept: AtomicBool,
    text: Mutex<Option<String>>,
    image: Mutex<Option<ui::clipboard::ClipboardImage>>,
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
    fn image(&self) -> Option<ui::clipboard::ClipboardImage> {
        self.image.lock().unwrap().clone()
    }
    fn name(&self) -> &'static str {
        "notes-refusing-clipboard"
    }
}

fn app() -> (TestApp<notes::NotesShell>, notes::PageId) {
    let mut app = TestApp::<notes::NotesShell>::new().unwrap();
    let page = notes::PageId::new_v4();
    app.invoke(Invoke::apply(&actions::create_page_action(page, None, "Clipboard")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app.harness().type_text("Ä😀");
    app.harness().key("Enter");
    app.harness().type_text("日本");
    app.harness().key("Enter");
    app.harness().type_text("After");
    (app, page)
}

fn menu(app: &mut TestApp<notes::NotesShell>, item: usize) {
    use ui::winit::event::{ElementState, Modifiers, MouseButton};
    let rect = app.harness().targets()["block_editor"];
    app.harness().set_modifiers(&[]);
    app.harness().dispatch_to_root_with(
        &ui::synthetic_input::cursor_moved(rect.x0 + 20.0, rect.y0 + 10.0),
        &Modifiers::default(),
    );
    app.harness().dispatch_to_root_with(
        &ui::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
        &Modifiers::default(),
    );
    for _ in 0..=item {
        app.harness().key("ArrowDown");
    }
    app.harness().key("Enter");
}

fn cut(app: &mut TestApp<notes::NotesShell>, route: &str) {
    match route {
        "keyboard" => {
            app.harness().set_modifiers(&["ctrl".into()]);
            app.harness().key("x");
            app.harness().set_modifiers(&[]);
        }
        "menu" => menu(app, 0),
        _ => panic!("unknown route"),
    }
}

fn refused_cut(route: &str) {
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page) = app();
    let blocks = app.read_document(|doc| read::page_blocks(doc, page));
    app.invoke(
        Invoke::apply(&actions::edit_body_action(
            page,
            tilo_document::RichTextAction::SetMark {
                region: tilo_document::action::RegionKind::Body,
                spans: blocks[..2]
                    .iter()
                    .map(|block| tilo_document::action::Span {
                        block: block.id.clone(),
                        start: 0,
                        end: block.text.chars().count(),
                    })
                    .collect(),
                name: "strong".into(),
                value: Some(tilo_document::action::MarkVal::Bool(true)),
                also_clear: vec![],
            },
        ))
        .unwrap(),
    )
    .unwrap();
    app.harness()
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 5,
            },
        )
        .unwrap();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    assert!(before[..2].iter().all(|block| !block.marks.is_empty()));
    let versions = app.versions().len();
    cut(&mut app, route);
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before,
        "refused {route} cut preserves canonical blocks"
    );
    assert_eq!(
        app.versions().len(),
        versions,
        "refusal creates no history step"
    );
    assert_eq!(clipboard.text(ClipboardTarget::Clipboard), None);
    clipboard.accept.store(true, Ordering::Relaxed);
    cut(&mut app, route);
    assert_eq!(
        clipboard.text(ClipboardTarget::Clipboard).as_deref(),
        Some("Ä😀\n日本"),
        "refusal preserved the exact selection"
    );
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(
        after
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        ["", "After"]
    );
    assert_eq!(after[0].id, before[0].id);
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
}

#[test]
fn refused_keyboard_cut_preserves_unicode_selection_and_history() {
    refused_cut("keyboard");
}
#[test]
fn refused_context_menu_cut_preserves_unicode_selection_and_history() {
    refused_cut("menu");
}

#[test]
fn refused_table_context_menu_cut_preserves_cell_selection_and_history() {
    use tilo_document::{
        RichTextAction,
        action::{AttrVal, RegionKind},
    };
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page) = app();
    let id = app.read_document(|doc| read::page_blocks(doc, page)[0].id.clone());
    for action in [
        RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: id.clone(),
            at: 0,
            del: 2,
            ins: "| Name | Value |\n| --- | --- |\n| 日本 | other |".into(),
        },
        RichTextAction::SetBlockAttr {
            region: RegionKind::Body,
            blocks: vec![id.clone()],
            key: "table_source".into(),
            value: AttrVal::Bool(true),
        },
    ] {
        app.invoke(Invoke::apply(&actions::edit_body_action(page, action)).unwrap())
            .unwrap();
    }
    let target = format!("notes.table-cell:{id}:1:0");
    app.harness()
        .perform_semantic_named("block_editor", Some(&target), &ui::a11y::Action::Focus)
        .unwrap();
    menu(&mut app, 3);
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    let versions = app.versions().len();
    menu(&mut app, 0);
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    assert_eq!(app.versions().len(), versions);
    clipboard.accept.store(true, Ordering::Relaxed);
    menu(&mut app, 0);
    assert_eq!(
        clipboard.text(ClipboardTarget::Clipboard).as_deref(),
        Some("日本")
    );
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert!(!after[0].text.contains("日本"));
    assert!(after[0].text.contains("other"));
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(after[1..], before[1..]);
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
}

fn refused_image_cut(route: &str) {
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page, source) = image_app();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    let versions = app.versions().len();
    if route == "keyboard" {
        // The browser preflights Cut with Copy before attempting its write.
        app.harness().set_modifiers(&["ctrl".into()]);
        app.harness().key("c");
        app.harness().set_modifiers(&[]);
        app.harness().key("Q");
        assert_eq!(
            app.read_document(|doc| read::page_blocks(doc, page)),
            before,
            "copy must not expose hidden source to subsequent typing"
        );
    }
    cut(&mut app, route);
    app.harness().key("Q");
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before,
        "typing after refused image cut must not replace hidden source"
    );
    assert_eq!(app.versions().len(), versions);
    clipboard.accept.store(true, Ordering::Relaxed);
    cut(&mut app, route);
    assert_eq!(
        clipboard.text(ClipboardTarget::Clipboard).as_deref(),
        Some(source)
    );
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)[0].text.clone()),
        ""
    );
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
}
#[test]
fn refused_image_keyboard_cut_does_not_expose_hidden_source_to_typing() {
    refused_image_cut("keyboard");
}
#[test]
fn refused_image_menu_cut_does_not_expose_hidden_source_to_typing() {
    refused_image_cut("menu");
}

fn image_app() -> (TestApp<notes::NotesShell>, notes::PageId, &'static str) {
    let (mut app, page) = app();
    let id = app.read_document(|doc| read::page_blocks(doc, page)[0].id.clone());
    let source = "![Synthetic preview](asset:missing-test-image)";
    app.invoke(
        Invoke::apply(&actions::edit_body_action(
            page,
            tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block: id.clone(),
                at: 0,
                del: 2,
                ins: source.into(),
            },
        ))
        .unwrap(),
    )
    .unwrap();
    for target in [
        format!("notes.image-edit:{id}"),
        format!("notes.image-done:{id}"),
    ] {
        app.harness()
            .perform_semantic_named("block_editor", Some(&target), &ui::a11y::Action::Activate)
            .unwrap();
    }
    app.harness()
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 0,
            },
        )
        .unwrap();
    (app, page, source)
}

fn image_replacement(route: &str) {
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page, source) = image_app();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    let assets_before = app.read_document(notes::read::images);
    let versions = app.versions().len();
    *clipboard.image.lock().unwrap() = Some(image_bytes());
    paste(&mut app, route);
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert!(!after.iter().any(|block| block.text == source));
    assert_eq!(
        after
            .iter()
            .filter(|block| block.text.starts_with("!["))
            .count(),
        1
    );
    assert!(
        after
            .iter()
            .any(|block| block.text.starts_with("![Pasted image](asset:"))
    );
    assert_eq!(
        app.versions().len(),
        versions.checked_add(1).unwrap(),
        "one canonical image-paste action"
    );
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    assert_eq!(app.read_document(notes::read::images), assets_before);
    assert!(app.redo());
    assert_eq!(app.read_document(|doc| read::page_blocks(doc, page)), after);
}

fn paste(app: &mut TestApp<notes::NotesShell>, route: &str) {
    if route == "menu" {
        menu(app, 2);
    } else {
        app.harness().set_modifiers(&["ctrl".into()]);
        app.harness().key("v");
        app.harness().set_modifiers(&[]);
    }
}

fn image_bytes() -> ui::clipboard::ClipboardImage {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(2, 2, image::Rgb([40, 80, 120]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    ui::clipboard::ClipboardImage {
        mime: "image/png",
        bytes: bytes.into_inner(),
    }
}

#[test]
fn context_menu_image_paste_still_replaces_the_preview_and_undoes() {
    image_replacement("menu");
}
#[test]
fn keyboard_image_paste_replaces_the_preview_in_one_action() {
    image_replacement("keyboard");
}

#[test]
fn invalid_image_paste_preserves_selected_source_and_history_then_retries() {
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page, _) = image_app();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    let versions = app.versions().len();
    *clipboard.image.lock().unwrap() = Some(ui::clipboard::ClipboardImage {
        mime: "image/png",
        bytes: vec![0, 1, 2, 3],
    });
    for route in ["keyboard", "menu"] {
        paste(&mut app, route);
        app.harness().key("Q");
        assert_eq!(
            app.read_document(|doc| read::page_blocks(doc, page)),
            before
        );
        assert_eq!(app.versions().len(), versions);
    }
    *clipboard.image.lock().unwrap() = Some(image_bytes());
    paste(&mut app, "menu");
    assert_eq!(app.versions().len(), versions.checked_add(1).unwrap());
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
}

#[test]
fn image_paste_replaces_a_marked_unicode_range_and_restores_it_in_one_undo() {
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page) = app();
    let blocks = app.read_document(|doc| read::page_blocks(doc, page));
    app.invoke(
        Invoke::apply(&actions::edit_body_action(
            page,
            tilo_document::RichTextAction::SetMark {
                region: tilo_document::action::RegionKind::Body,
                spans: blocks[..2]
                    .iter()
                    .map(|block| tilo_document::action::Span {
                        block: block.id.clone(),
                        start: 0,
                        end: 2,
                    })
                    .collect(),
                name: "strong".into(),
                value: Some(tilo_document::action::MarkVal::Bool(true)),
                also_clear: vec![],
            },
        ))
        .unwrap(),
    )
    .unwrap();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    let versions = app.versions().len();
    app.harness()
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: 1,
                focus: 4,
            },
        )
        .unwrap();
    *clipboard.image.lock().unwrap() = Some(image_bytes());
    paste(&mut app, "keyboard");
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(
        after.len(),
        4,
        "prefix, image, suffix, untouched next block"
    );
    assert_eq!(after[0].text, "Ä");
    assert!(after[1].text.starts_with("![Pasted image](asset:"));
    assert_eq!(after[2].text, "本");
    assert_eq!(after[3], before[2]);
    assert!(!after[0].marks.is_empty());
    assert!(!after[2].marks.is_empty());
    assert_eq!(app.versions().len(), versions.checked_add(1).unwrap());
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    assert!(app.redo());
    assert_eq!(app.read_document(|doc| read::page_blocks(doc, page)), after);
}

#[test]
fn image_paste_into_a_fresh_empty_note_is_undoable() {
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let mut app = TestApp::<notes::NotesShell>::new().unwrap();
    let page = notes::PageId::new_v4();
    app.invoke(Invoke::apply(&actions::create_page_action(page, None, "Image")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    *clipboard.image.lock().unwrap() = Some(image_bytes());
    paste(&mut app, "keyboard");
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    // The picture, then the empty line the caret goes on.
    assert_eq!(after.len(), 2);
    assert!(after[0].text.starts_with("![Pasted image](asset:"));
    assert_eq!(after[1].text, "");
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
}

#[test]
fn image_paste_excludes_a_column_zero_endpoint_and_preserves_peer_text_through_undo() {
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page) = app();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    let mut peer = app.peer_document();
    crate::component_support::edit_page_body(
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
    app.harness()
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 3,
            },
        )
        .unwrap();
    *clipboard.image.lock().unwrap() = Some(image_bytes());
    paste(&mut app, "keyboard");
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    // The selected line becomes the picture and an empty line for the
    // caret; the lines after it are untouched.
    assert_eq!(after.len(), 4);
    assert!(after[0].text.starts_with("![Pasted image](asset:"));
    assert_eq!(after[1].text, "");
    assert_eq!(after[2..], before[1..]);
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)[3].text.clone()),
        "Peer After"
    );
    assert!(app.undo());
    let undone = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(undone[..2], before[..2]);
    assert_eq!(undone[2].text, "Peer After");
}

#[test]
fn image_paste_refuses_during_composition_and_for_viewers() {
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page) = app();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    let versions = app.versions().len();
    *clipboard.image.lock().unwrap() = Some(image_bytes());
    app.harness()
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 5,
            },
        )
        .unwrap();
    app.harness()
        .dispatch_to_root(&ui::winit::event::WindowEvent::Ime(
            ui::winit::event::Ime::Preedit("に".into(), Some((0, 3))),
        ));
    paste(&mut app, "keyboard");
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    assert_eq!(app.versions().len(), versions);
    app.harness()
        .dispatch_to_root(&ui::winit::event::WindowEvent::Ime(
            ui::winit::event::Ime::Preedit(String::new(), None),
        ));
    app.set_role(ui::permissions::Role::Viewer);
    paste(&mut app, "keyboard");
    menu(&mut app, 2);
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    assert_eq!(app.versions().len(), versions);
}

#[test]
fn image_paste_replaces_empty_rows_and_selected_separators_without_touching_the_next_id() {
    for empty in [false, true] {
        let clipboard = Arc::new(Clipboard::default());
        let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
        let _host = ui::host::enter(&services);
        let (mut app, page) = app();
        if empty {
            let id = app.read_document(|doc| read::page_blocks(doc, page)[0].id.clone());
            app.invoke(
                Invoke::apply(&actions::edit_body_action(
                    page,
                    tilo_document::RichTextAction::SpliceText {
                        region: tilo_document::action::RegionKind::Body,
                        block: id,
                        at: 0,
                        del: 2,
                        ins: String::new(),
                    },
                ))
                .unwrap(),
            )
            .unwrap();
        }
        let before = app.read_document(|doc| read::page_blocks(doc, page));
        let anchor = if empty { 0 } else { 2 };
        app.harness()
            .perform_named(
                "block_editor",
                &ui::a11y::Action::SetSelection {
                    anchor,
                    focus: anchor + 1,
                },
            )
            .unwrap();
        *clipboard.image.lock().unwrap() = Some(image_bytes());
        paste(&mut app, "keyboard");
        let after = app.read_document(|doc| read::page_blocks(doc, page));
        let image_at = usize::from(!empty);
        assert!(after[image_at].text.starts_with("![Pasted image](asset:"));
        assert_eq!(after[image_at + 1].text, "", "the caret's line under it");
        assert_eq!(after[image_at + 2..], before[1..]);
        if !empty {
            assert_eq!(after[0], before[0]);
        }
        assert!(app.undo());
        assert_eq!(
            app.read_document(|doc| read::page_blocks(doc, page)),
            before
        );
    }
}

#[test]
fn unavailable_image_paste_keeps_preview_source_protected_then_accepts_text() {
    for text in [None, Some(String::new())] {
        let clipboard = Arc::new(Clipboard::default());
        *clipboard.text.lock().unwrap() = text;
        let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
        let _host = ui::host::enter(&services);
        let (mut app, page, _) = image_app();
        let before = app.read_document(|doc| read::page_blocks(doc, page));
        let versions = app.versions().len();
        for route in ["keyboard", "menu"] {
            paste(&mut app, route);
            app.harness().key("Q");
            assert_eq!(
                app.read_document(|doc| read::page_blocks(doc, page)),
                before
            );
            assert_eq!(app.versions().len(), versions);
        }
        *clipboard.text.lock().unwrap() = Some("replacement".into());
        paste(&mut app, "keyboard");
        assert_eq!(
            app.read_document(|doc| read::page_blocks(doc, page)[0].text.clone()),
            "replacement"
        );
        assert!(app.undo());
        assert_eq!(
            app.read_document(|doc| read::page_blocks(doc, page)),
            before
        );
    }
}
