use crate::framework_support::{add_note, notes, open, surface};
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use vello::kurbo::Point;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};
fn cursor_moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}

fn mouse_press_right() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Right,
    }
}

fn named_key(key: NamedKey) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(key),
        None,
        ElementState::Pressed,
    ))
}

/// Notes with one note (`body`) open, laid out `width` x 800.
fn note_at(width: u16, body: &str) -> (TestApp<NotesShell>, PageId) {
    let mut app = notes();
    let page = add_note(&mut app, "Welcome", body);
    open(&mut app, page);
    app.harness().relayout_at((u32::from(width), 800)).unwrap();
    (app, page)
}

fn dispatch(app: &mut TestApp<NotesShell>, modifiers: Modifiers, event: &WindowEvent) -> bool {
    app.harness().dispatch_to_root_with(event, &modifiers)
}

fn menu_open(app: &mut TestApp<NotesShell>) -> bool {
    surface(app, |notes| notes.note_menu_open_for_test())
}

fn text(app: &mut TestApp<NotesShell>) -> String {
    surface(app, |notes| notes.editor_text_for_test())
}

fn first_block(app: &mut TestApp<NotesShell>) -> (String, String) {
    surface(app, |notes| {
        let block = &notes.main_panel_mut().editor().blocks()[0];
        (block.id.clone(), block.text().to_owned())
    })
}

fn editor_rect(app: &mut TestApp<NotesShell>) -> vello::kurbo::Rect {
    *app.harness()
        .targets()
        .get("block_editor")
        .expect("body editor laid out")
}

/// Perform `action` on the editor's semantic `target`, as assistive
/// technology does.
fn semantic(app: &mut TestApp<NotesShell>, target: &str, action: &ui::a11y::Action) {
    app.harness()
        .perform_semantic_named("block_editor", Some(target), action)
        .unwrap();
}

#[test]
fn the_note_actions_button_opens_the_note_menu_anchored_to_itself() {
    for width in [480, 1200] {
        let (mut app, _) = note_at(width, "Welcome to Notes");
        let button = surface(&mut app, |notes| notes.note_menu_button_center_for_test());
        assert!(
            button.x > 0.0,
            "the header lays the button out, width {width}"
        );
        app.harness().click_at(button.x, button.y);
        assert!(menu_open(&mut app), "width {width}");
        // The menu lays its rows out when it paints.
        let _ = app.harness().render_rgba();
        let (menu, anchor) = surface(&mut app, |notes| notes.note_menu_rects_for_test())
            .expect("an open menu has a rect");
        assert!(
            menu.y0 >= anchor.y1 - 1.0 && menu.y0 <= anchor.y1 + 16.0,
            "the menu opens just under its button: {menu:?} vs {anchor:?}"
        );
        assert!(
            menu.x1 >= anchor.x1 - 1.0 && menu.x0 < anchor.x1,
            "the menu hangs from the button's edge: {menu:?} vs {anchor:?}"
        );
        dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Escape));
        assert!(!menu_open(&mut app));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "single interaction test follows selection and shortcut isolation through the menu lifecycle"
)]
#[test]
fn note_context_menus_open_everywhere_and_keep_selection_and_drawer() {
    eprintln!("phase open_note_context_menu_works_on_title_body_and_margin_in_both_layouts");
    {
        for width in [480, 1200] {
            let (mut app, _) = note_at(width, "A first paragraph.\n\nAnd a second one.");
            let title = surface(&mut app, |notes| notes.main_title_center_for_test());
            for point in [
                title,
                Point::new(f64::from(width) - 20.0, 400.0),
                Point::new(title.x, 220.0),
            ] {
                dispatch(
                    &mut app,
                    Modifiers::default(),
                    &cursor_moved(point.x, point.y),
                );
                assert!(dispatch(
                    &mut app,
                    Modifiers::default(),
                    &mouse_press_right()
                ));
                assert!(
                    menu_open(&mut app),
                    "note menu missing at {point:?}, width {width}"
                );
                dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Escape));
                assert!(!menu_open(&mut app));
            }
            // Keyboard activation of New child note follows the shared action path.
            let before = app.state().pages().len();
            let parent = surface(&mut app, |notes| notes.selected_for_test()).unwrap();
            dispatch(
                &mut app,
                Modifiers::default(),
                &cursor_moved(title.x, title.y),
            );
            dispatch(&mut app, Modifiers::default(), &mouse_press_right());
            dispatch(
                &mut app,
                Modifiers::default(),
                &named_key(NamedKey::ArrowDown),
            );
            dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Enter));
            assert!(!menu_open(&mut app));
            let pages = app.state().pages().to_vec();
            assert_eq!(pages.len(), before + 1);
            let selected = surface(&mut app, |notes| notes.selected_for_test()).unwrap();
            assert!(
                pages
                    .iter()
                    .any(|p| p.id == selected && p.parent == Some(parent))
            );
        }
    }

    eprintln!("phase note_text_menu_preserves_selection_and_blocks_background_shortcuts");
    {
        let (mut app, _) = note_at(1200, "A first paragraph.\n\nAnd a second one.");
        let before = text(&mut app);
        let pages_before = app.state().pages().len();
        let rect = editor_rect(&mut app);
        let body = Point::new(rect.x0 + 10.0, rect.y0 + 10.0);
        dispatch(
            &mut app,
            Modifiers::default(),
            &cursor_moved(body.x, body.y),
        );
        dispatch(&mut app, Modifiers::default(), &mouse_press_right());
        assert!(menu_open(&mut app));
        let mut key = named_key(NamedKey::Space);
        if let WindowEvent::KeyboardInput { event, .. } = &mut key {
            event.logical_key = Key::Character("n".into());
            event.text = Some("n".into());
        }
        dispatch(
            &mut app,
            winit::keyboard::ModifiersState::CONTROL.into(),
            &key,
        );
        assert!(menu_open(&mut app));
        assert_eq!(
            app.state().pages().len(),
            pages_before,
            "menu captures Ctrl+N"
        );
        assert_eq!(
            text(&mut app),
            before,
            "menu input must not type into the body"
        );
        // Select all via the fourth body command, then reopen without touching
        // the selection and cut via the first command.
        for _ in 0..4 {
            dispatch(
                &mut app,
                Modifiers::default(),
                &named_key(NamedKey::ArrowDown),
            );
        }
        dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Enter));
        assert!(!menu_open(&mut app));
        dispatch(&mut app, Modifiers::default(), &mouse_press_right());
        assert!(menu_open(&mut app));
        dispatch(
            &mut app,
            Modifiers::default(),
            &named_key(NamedKey::ArrowDown),
        );
        dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Enter));
        assert!(
            text(&mut app).trim().is_empty(),
            "cut replaces the preserved document selection"
        );
        app.harness().relayout();
        assert!(
            text(&mut app).trim().is_empty(),
            "cut survives canonical sync"
        );
        assert_eq!(app.state().pages().len(), pages_before);
    }

    eprintln!("phase narrow_sidebar_context_menu_keeps_its_drawer_visible_when_selecting_a_note");
    {
        let (mut app, _) = note_at(480, "");
        let second = add_note(&mut app, "Second", "");
        open(&mut app, second);
        let toggle = surface(&mut app, |notes| notes.menu_btn_center_for_test());
        app.harness().click_at(toggle.x, toggle.y);
        assert!(
            app.harness().control_failures().is_empty(),
            "{:?}",
            app.harness().control_failures()
        );
        assert!(surface(&mut app, |notes| notes.drawer_open_for_test()));
        app.harness().relayout_at((480, 800)).unwrap();
        let row = surface(&mut app, |notes| notes.sidebar_row_center_for_test(0)).unwrap();
        dispatch(&mut app, Modifiers::default(), &cursor_moved(row.x, row.y));
        dispatch(&mut app, Modifiers::default(), &mouse_press_right());
        assert!(surface(&mut app, |notes| notes.sidebar_menu_open_for_test()));
        assert!(
            surface(&mut app, |notes| notes.drawer_open_for_test()),
            "right-click selection must not hide its own menu"
        );
        dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Escape));
        assert!(!surface(&mut app, |notes| notes.sidebar_menu_open_for_test()));
        assert!(
            surface(&mut app, |notes| notes.drawer_open_for_test()),
            "Escape dismisses the menu before its drawer"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn table_context_commands_edit_only_the_current_table() {
    eprintln!("phase table_menu_refuses_commands_after_remote_content_changes_at_same_cell");
    {
        for selection_steps in [1, 5] {
            let source = "| Name |\n| --- |\n| Before |\n| Selected |\n| Following |\n";
            let (mut app, page) = note_at(1200, source);
            let (id, _) = first_block(&mut app);
            semantic(
                &mut app,
                &format!("notes.table-cell:{id}:2:0"),
                &ui::a11y::Action::Focus,
            );
            app.harness().set_modifiers(&["ctrl".into()]);
            app.harness().key("a");
            app.harness().set_modifiers(&[]);
            let rect = editor_rect(&mut app);
            dispatch(
                &mut app,
                Modifiers::default(),
                &cursor_moved(rect.x0 + 40.0, rect.y0 + 50.0),
            );
            dispatch(&mut app, Modifiers::default(), &mouse_press_right());
            assert!(menu_open(&mut app));
            let (_, canonical_source) = first_block(&mut app);
            let changed = canonical_source.replace("| Before |\n", "");
            assert_ne!(changed, canonical_source);
            app.merge_peer_edit(|doc| {
                crate::component_support::replace_block_text(doc, page, &id, &changed);
            });
            assert_eq!(
                surface(&mut app, |notes| notes
                    .main_panel_mut()
                    .editor()
                    .focused_table_cell()),
                Some((id, 2, 0))
            );
            let heads = app.peer_document().get_heads();
            // Exercise both Cut and Add row. The same coordinate now addresses
            // a different row; neither captured command may touch it.
            for _ in 0..selection_steps {
                dispatch(
                    &mut app,
                    Modifiers::default(),
                    &named_key(NamedKey::ArrowDown),
                );
            }
            dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Enter));
            assert!(!menu_open(&mut app));
            assert_eq!(app.peer_document().get_heads(), heads);
            assert_eq!(first_block(&mut app).1, changed);
        }
    }

    eprintln!("phase table_context_commands_edit_the_selected_table_without_creating_notes");
    {
        for (command_slot, expected_rows, expected_columns) in
            [(0, 3, 2), (1, 2, 3), (2, 1, 2), (3, 2, 1)]
        {
            let (mut app, _) =
                note_at(1200, "| Name | Status |\n| --- | --- |\n| Mina | Ready |\n");
            let (id, _) = first_block(&mut app);
            semantic(
                &mut app,
                &format!("notes.table-cell:{id}:1:0"),
                &ui::a11y::Action::Focus,
            );
            let rect = editor_rect(&mut app);
            dispatch(
                &mut app,
                Modifiers::default(),
                &cursor_moved(rect.x0 + 40.0, rect.y0 + 50.0),
            );
            dispatch(&mut app, Modifiers::default(), &mouse_press_right());
            assert!(menu_open(&mut app));
            // Four clipboard commands precede the table commands; separators
            // do not consume a keyboard selection step.
            for _ in 0..5 + command_slot {
                dispatch(
                    &mut app,
                    Modifiers::default(),
                    &named_key(NamedKey::ArrowDown),
                );
            }
            dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Enter));
            assert!(!menu_open(&mut app));
            assert_eq!(app.state().pages().len(), 1);
            let accessibility = surface(&mut app, |notes| {
                ui::widget::Widget::accessibility(notes.main_panel_mut().editor()).unwrap()
            });
            let table = accessibility.children[0]
                .children
                .iter()
                .find(|child| child.role == ui::a11y::Role::Table)
                .unwrap();
            assert_eq!(
                table.children.len(),
                expected_rows,
                "table command {command_slot}"
            );
            assert!(
                table
                    .children
                    .iter()
                    .all(|row| row.children.len() == expected_columns)
            );
            let (_, expected) = first_block(&mut app);
            let page = app.state().pages()[0].id;
            let reopened = app.reopen().unwrap();
            assert_eq!(
                reopened.read_document(|doc| notes::doc::NotesDoc::page_block_snapshots(doc, page))
                    [0]
                .1,
                expected
            );
        }
    }
}
