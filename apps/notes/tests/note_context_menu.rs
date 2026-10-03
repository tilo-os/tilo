use notes::NotesApp;
use tempfile::TempDir;
use ui::headless::{FrameOptions, Framed};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};
fn cursor_moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}

fn mouse_press_right() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: ElementState::Pressed,
        button: MouseButton::Right,
    }
}

fn named_key(key: NamedKey) -> WindowEvent {
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Named(key);
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

/// Mount `app` over `state` and run the production frame at `w`x`h`.
fn settle_layout(app: NotesApp, state: ui::State, w: u16, h: u16) -> Framed<NotesApp> {
    Framed::with_options(
        state,
        app,
        (u32::from(w), u32::from(h)),
        FrameOptions {
            text: Some(crate::component_support::renderer()),
            ..FrameOptions::default()
        },
    )
}

fn dispatch(app: &mut Framed<NotesApp>, modifiers: Modifiers, event: &WindowEvent) -> bool {
    Framed::harness_mut(app).dispatch_to_root_with(event, &modifiers)
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
            let tmp = TempDir::new().unwrap();
            let app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
            let state = ui::State::new();
            let mut app = settle_layout(app, state, width, 800);
            let title = app.main_title_center_for_test();
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
                    app.note_menu_open_for_test(),
                    "note menu missing at {point:?}, width {width}"
                );
                dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Escape));
                assert!(!app.note_menu_open_for_test());
            }
            // Keyboard activation of New child note follows the shared action path.
            let before = app.workspace_pages_for_test().len();
            let parent = app.selected_for_test().unwrap();
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
            assert!(!app.note_menu_open_for_test());
            let pages = app.workspace_pages_for_test();
            assert_eq!(pages.len(), before + 1);
            assert!(
                pages
                    .iter()
                    .any(|p| p.id == app.selected_for_test().unwrap() && p.parent == Some(parent))
            );
        }
    }

    eprintln!("phase note_text_menu_preserves_selection_and_blocks_background_shortcuts");
    {
        let tmp = TempDir::new().unwrap();
        let app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let state = ui::State::new();
        let mut app = settle_layout(app, state, 1200, 800);
        let before = app.editor_text_for_test();
        let pages_before = app.workspace_pages_for_test().len();
        let rect = editor_rect(&*app).expect("body editor laid out");
        let body = Point::new(rect.x0 + 10.0, rect.y0 + 10.0);
        dispatch(
            &mut app,
            Modifiers::default(),
            &cursor_moved(body.x, body.y),
        );
        dispatch(&mut app, Modifiers::default(), &mouse_press_right());
        assert!(app.note_menu_open_for_test());
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
        assert!(app.note_menu_open_for_test());
        assert_eq!(
            app.workspace_pages_for_test().len(),
            pages_before,
            "menu captures Ctrl+N"
        );
        assert_eq!(
            app.editor_text_for_test(),
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
        assert!(!app.note_menu_open_for_test());
        dispatch(&mut app, Modifiers::default(), &mouse_press_right());
        assert!(app.note_menu_open_for_test());
        dispatch(
            &mut app,
            Modifiers::default(),
            &named_key(NamedKey::ArrowDown),
        );
        dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Enter));
        assert!(
            app.editor_text_for_test().trim().is_empty(),
            "cut replaces the preserved document selection"
        );
        let (notes, state) = Framed::split(&mut app);
        notes.sync(&mut state.doc, &state.root);
        assert!(
            app.editor_text_for_test().trim().is_empty(),
            "cut survives canonical sync"
        );
        assert_eq!(app.workspace_pages_for_test().len(), pages_before);
    }

    eprintln!("phase narrow_sidebar_context_menu_keeps_its_drawer_visible_when_selecting_a_note");
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        app.apply_event_for_test(notes::sidebar::OutEvent::CreateChild { parent: None });
        let state = ui::State::new();
        let mut app = settle_layout(app, state, 480, 800);
        // The shell's toggle is an action-bound control: click it through the
        // production path, which runs the action after the click.
        let toggle = app.menu_btn_center_for_test();
        Framed::harness_mut(&mut app).click_at(toggle.x, toggle.y);
        assert!(app.drawer_open_for_test());
        Framed::harness_mut(&mut app)
            .relayout_at((480, 800))
            .unwrap();
        let row = app.sidebar_row_center_for_test(0).unwrap();
        dispatch(&mut app, Modifiers::default(), &cursor_moved(row.x, row.y));
        dispatch(&mut app, Modifiers::default(), &mouse_press_right());
        assert!(app.sidebar_menu_open_for_test());
        assert!(
            app.drawer_open_for_test(),
            "right-click selection must not hide its own menu"
        );
        dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Escape));
        assert!(!app.sidebar_menu_open_for_test());
        assert!(
            app.drawer_open_for_test(),
            "Escape dismisses the menu before its drawer"
        );
    }
}

fn editor_rect(widget: &dyn Widget) -> Option<vello::kurbo::Rect> {
    if widget.name() == "BlockEditor" {
        return Some(widget.get_rect());
    }
    widget.children().into_iter().find_map(editor_rect)
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn table_context_commands_edit_only_the_current_table() {
    eprintln!("phase table_menu_refuses_commands_after_remote_content_changes_at_same_cell");
    {
        for selection_steps in [1, 5] {
            let tmp = TempDir::new().unwrap();
            notes::storage::ensure_layout(tmp.path()).unwrap();
            let mut workspace = notes::Workspace::new();
            let page = workspace.create_page(None);
            workspace.set_selected(Some(page));
            notes::storage::save_workspace(tmp.path(), &workspace).unwrap();
            let source = "| Name |\n| --- |\n| Before |\n| Selected |\n| Following |\n";
            notes::storage::write_page_body(tmp.path(), page, source).unwrap();
            let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
            app.bypass_seed_grace_for_test();
            let state = ui::State::new();
            let mut app = settle_layout(app, state, 1200, 800);
            let id = app.main_panel_mut().editor().blocks()[0].id.clone();
            let (notes, state) = Framed::split(&mut app);
            assert!(notes.main_panel_mut().editor_mut().perform_semantic_action(
                &format!("notes.table-cell:{id}:2:0"),
                &ui::a11y::Action::Focus,
                &mut state.doc,
                &state.root,
            ));
            assert!(notes.main_panel_mut().editor_mut().perform_semantic_action(
                "notes.editor:select-all",
                &ui::a11y::Action::Activate,
                &mut state.doc,
                &state.root,
            ));
            let rect = editor_rect(&*app).unwrap();
            dispatch(
                &mut app,
                Modifiers::default(),
                &cursor_moved(rect.x0 + 40.0, rect.y0 + 50.0),
            );
            dispatch(&mut app, Modifiers::default(), &mouse_press_right());
            assert!(app.note_menu_open_for_test());
            let canonical_source = app.main_panel_mut().editor().blocks()[0].text().to_owned();
            let changed = canonical_source.replace("| Before |\n", "");
            assert_ne!(changed, canonical_source);
            let state = Framed::split(&mut app).1;
            crate::component_support::replace_block_text(&mut state.doc, page, &id, &changed);
            Framed::harness_mut(&mut app)
                .relayout_at((1200, 800))
                .unwrap();
            assert_eq!(
                app.main_panel_mut().editor().focused_table_cell(),
                Some((id, 2, 0))
            );
            let heads = Framed::split(&mut app).1.doc.get_heads();
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
            assert!(!app.note_menu_open_for_test());
            assert_eq!(Framed::split(&mut app).1.doc.get_heads(), heads);
            assert_eq!(app.main_panel_mut().editor().blocks()[0].text(), changed);
        }
    }

    eprintln!("phase table_context_commands_edit_the_selected_table_without_creating_notes");
    {
        for (command_slot, expected_rows, expected_columns) in
            [(0, 3, 2), (1, 2, 3), (2, 1, 2), (3, 2, 1)]
        {
            let tmp = TempDir::new().unwrap();
            notes::storage::ensure_layout(tmp.path()).unwrap();
            let mut workspace = notes::Workspace::new();
            let page = workspace.create_page(None);
            workspace.set_selected(Some(page));
            notes::storage::save_workspace(tmp.path(), &workspace).unwrap();
            notes::storage::write_page_body(
                tmp.path(),
                page,
                "| Name | Status |\n| --- | --- |\n| Mina | Ready |\n",
            )
            .unwrap();
            let app = NotesApp::spawn(tmp.path().into()).unwrap();
            let state = ui::State::new();
            let mut app = settle_layout(app, state, 1200, 800);
            app.draw(
                &mut vello::Scene::new(),
                vello::kurbo::Affine::IDENTITY,
                &mut crate::component_support::renderer(),
                &Theme::dark(),
                &ui::presence::PresenceCtx::empty(),
            );
            let id = app.main_panel_mut().editor().blocks()[0].id.clone();
            let (notes, state) = Framed::split(&mut app);
            assert!(notes.main_panel_mut().editor_mut().perform_semantic_action(
                &format!("notes.table-cell:{id}:1:0"),
                &ui::a11y::Action::Focus,
                &mut state.doc,
                &state.root,
            ));
            let rect = editor_rect(&*app).unwrap();
            dispatch(
                &mut app,
                Modifiers::default(),
                &cursor_moved(rect.x0 + 40.0, rect.y0 + 50.0),
            );
            dispatch(&mut app, Modifiers::default(), &mouse_press_right());
            assert!(app.note_menu_open_for_test());
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
            assert!(!app.note_menu_open_for_test());
            assert_eq!(app.workspace_pages_for_test().len(), 1);
            let accessibility = app.main_panel_mut().editor().accessibility().unwrap();
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
            app.flush_saves();
            let expected = app.main_panel_mut().editor().blocks()[0].text().to_owned();
            drop(app);
            let mut reopened = NotesApp::spawn(tmp.path().into()).unwrap();
            assert_eq!(
                reopened.main_panel_mut().editor().blocks()[0].text(),
                expected
            );
        }
    }
}
