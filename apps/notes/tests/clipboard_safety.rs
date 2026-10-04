//! Clipboard refusal must never change a note, its selection or its history.
use notes::{actions, doc::NotesDoc};
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
    app.invoke(Invoke::apply(&actions::new_page_action(page, None, 0, "Clipboard")).unwrap())
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
    let blocks = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
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
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert!(before[..2].iter().all(|block| !block.marks.is_empty()));
    let versions = app.versions().len();
    cut(&mut app, route);
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
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
    let after = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
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
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
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
    let id = app.read_document(|doc| NotesDoc::page_blocks(doc, page)[0].id.clone());
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
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    let versions = app.versions().len();
    menu(&mut app, 0);
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
    assert_eq!(app.versions().len(), versions);
    clipboard.accept.store(true, Ordering::Relaxed);
    menu(&mut app, 0);
    assert_eq!(
        clipboard.text(ClipboardTarget::Clipboard).as_deref(),
        Some("日本")
    );
    let after = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    assert!(!after[0].text.contains("日本"));
    assert!(after[0].text.contains("other"));
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(after[1..], before[1..]);
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
}

fn refused_image_cut(route: &str) {
    let clipboard = Arc::new(Clipboard::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let (mut app, page, source) = image_app();
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    let versions = app.versions().len();
    if route == "keyboard" {
        // The browser preflights Cut with Copy before attempting its write.
        app.harness().set_modifiers(&["ctrl".into()]);
        app.harness().key("c");
        app.harness().set_modifiers(&[]);
        app.harness().key("Q");
        assert_eq!(
            app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
            before,
            "copy must not expose hidden source to subsequent typing"
        );
    }
    cut(&mut app, route);
    app.harness().key("Q");
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
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
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)[0].text.clone()),
        ""
    );
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
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
    let id = app.read_document(|doc| NotesDoc::page_blocks(doc, page)[0].id.clone());
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
