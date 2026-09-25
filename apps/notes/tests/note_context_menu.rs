use notes::NotesApp;
use std::collections::HashMap;
use taffy::prelude::*;
use tempfile::TempDir;
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

/// Build + lay out `app` at `w`x`h`, looping until the responsive
/// structure settles, returning the final taffy tree + child node so
/// the caller can dispatch events against a live layout.
fn settle_layout(
    app: &mut NotesApp,
    state: &mut ui::State,
    w: u16,
    h: u16,
) -> Result<(TaffyTree<()>, NodeId), String> {
    let theme = Theme::dark();
    let mut text_renderer = crate::component_support::renderer();
    app.sync(&mut state.doc, &state.root);
    let mut result: Option<(TaffyTree<()>, NodeId)> = None;
    for _ in 0..5 {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(f32::from(w)),
                        height: Dimension::length(f32::from(h)),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .map_err(|e| e.to_string())?;
        {
            let measure_map = node_map.clone();
            let tr = &mut text_renderer;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(f32::from(w)),
                        height: AvailableSpace::Definite(f32::from(h)),
                    },
                    |known, avail, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, avail, tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
        let settled = !app.take_structure_dirty();
        result = Some((taffy, child));
        if settled {
            break;
        }
    }
    result.ok_or_else(|| "layout never settled".to_string())
}

fn dispatch(
    app: &mut NotesApp,
    state: &mut ui::State,
    taffy: &TaffyTree<()>,
    child: NodeId,
    modifiers: Modifiers,
    event: &WindowEvent,
) -> bool {
    let mut handled = false;
    let mut focus_target = None;
    app.handle_event(
        event,
        1.0,
        &modifiers,
        &mut state.doc,
        &state.root,
        &mut handled,
        &mut focus_target,
        child,
        taffy,
    );
    handled
}

#[test]
fn open_note_context_menu_works_on_title_body_and_margin_in_both_layouts() {
    for width in [480, 1200] {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let mut state = ui::State::new();
        let (tree, node) = settle_layout(&mut app, &mut state, width, 800).unwrap();
        let title = app.main_title_center_for_test();
        for point in [
            title,
            Point::new(f64::from(width) - 20.0, 400.0),
            Point::new(title.x, 220.0),
        ] {
            dispatch(
                &mut app,
                &mut state,
                &tree,
                node,
                Modifiers::default(),
                &cursor_moved(point.x, point.y),
            );
            assert!(dispatch(
                &mut app,
                &mut state,
                &tree,
                node,
                Modifiers::default(),
                &mouse_press_right()
            ));
            assert!(
                app.note_menu_open_for_test(),
                "note menu missing at {point:?}, width {width}"
            );
            dispatch(
                &mut app,
                &mut state,
                &tree,
                node,
                Modifiers::default(),
                &named_key(NamedKey::Escape),
            );
            assert!(!app.note_menu_open_for_test());
        }
        // Keyboard activation of New child note follows the shared action path.
        let before = app.workspace_pages_for_test().len();
        let parent = app.selected_for_test().unwrap();
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &cursor_moved(title.x, title.y),
        );
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &mouse_press_right(),
        );
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &named_key(NamedKey::ArrowDown),
        );
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &named_key(NamedKey::Enter),
        );
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

fn editor_rect(widget: &dyn Widget) -> Option<vello::kurbo::Rect> {
    if widget.name() == "BlockEditor" {
        return Some(widget.get_rect());
    }
    widget.children().into_iter().find_map(editor_rect)
}

#[test]
fn table_menu_refuses_commands_after_remote_content_changes_at_same_cell() {
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
        let mut state = ui::State::new();
        let (tree, node) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
        let id = app.main_panel_mut().editor().blocks()[0].id.clone();
        assert!(app.main_panel_mut().editor_mut().perform_semantic_action(
            &format!("notes.table-cell:{id}:2:0"),
            &ui::a11y::Action::Focus,
            &mut state.doc,
            &state.root,
        ));
        assert!(app.main_panel_mut().editor_mut().perform_semantic_action(
            "notes.editor:select-all",
            &ui::a11y::Action::Activate,
            &mut state.doc,
            &state.root,
        ));
        let rect = editor_rect(&app).unwrap();
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &cursor_moved(rect.x0 + 40.0, rect.y0 + 50.0),
        );
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &mouse_press_right(),
        );
        assert!(app.note_menu_open_for_test());
        let canonical_source = app.main_panel_mut().editor().blocks()[0].text().to_owned();
        let changed = canonical_source.replace("| Before |\n", "");
        assert_ne!(changed, canonical_source);
        notes::doc::NotesDoc::apply_block_ops(
            &mut state.doc,
            page,
            &[ui::widget::BlockEditOp::TextSplice {
                block_id: id.clone(),
                start: 0,
                end: canonical_source.len(),
                text: changed.clone(),
            }],
        )
        .unwrap();
        let (tree, node) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
        assert_eq!(
            app.main_panel_mut().editor().focused_table_cell(),
            Some((id, 2, 0))
        );
        let heads = state.doc.get_heads();
        // Exercise both Cut and Add row. The same coordinate now addresses
        // a different row; neither captured command may touch it.
        for _ in 0..selection_steps {
            dispatch(
                &mut app,
                &mut state,
                &tree,
                node,
                Modifiers::default(),
                &named_key(NamedKey::ArrowDown),
            );
        }
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &named_key(NamedKey::Enter),
        );
        assert!(!app.note_menu_open_for_test());
        assert_eq!(state.doc.get_heads(), heads);
        assert_eq!(app.main_panel_mut().editor().blocks()[0].text(), changed);
    }
}

#[test]
fn table_context_commands_edit_the_selected_table_without_creating_notes() {
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
        let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
        let mut state = ui::State::new();
        let (tree, node) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
        app.draw(
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            &mut crate::component_support::renderer(),
            &Theme::dark(),
            &ui::presence::PresenceCtx::empty(),
        );
        let id = app.main_panel_mut().editor().blocks()[0].id.clone();
        assert!(app.main_panel_mut().editor_mut().perform_semantic_action(
            &format!("notes.table-cell:{id}:1:0"),
            &ui::a11y::Action::Focus,
            &mut state.doc,
            &state.root,
        ));
        let rect = editor_rect(&app).unwrap();
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &cursor_moved(rect.x0 + 40.0, rect.y0 + 50.0),
        );
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &mouse_press_right(),
        );
        assert!(app.note_menu_open_for_test());
        // Four clipboard commands precede the table commands; separators
        // do not consume a keyboard selection step.
        for _ in 0..5 + command_slot {
            dispatch(
                &mut app,
                &mut state,
                &tree,
                node,
                Modifiers::default(),
                &named_key(NamedKey::ArrowDown),
            );
        }
        dispatch(
            &mut app,
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &named_key(NamedKey::Enter),
        );
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "single interaction test follows selection and shortcut isolation through the menu lifecycle"
)]
fn note_text_menu_preserves_selection_and_blocks_background_shortcuts() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let mut state = ui::State::new();
    let (tree, node) = settle_layout(&mut app, &mut state, 1200, 800).unwrap();
    let before = app.editor_text_for_test();
    let pages_before = app.workspace_pages_for_test().len();
    let rect = editor_rect(&app).expect("body editor laid out");
    let body = Point::new(rect.x0 + 10.0, rect.y0 + 10.0);
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &cursor_moved(body.x, body.y),
    );
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &mouse_press_right(),
    );
    assert!(app.note_menu_open_for_test());
    let mut key = named_key(NamedKey::Space);
    if let WindowEvent::KeyboardInput { event, .. } = &mut key {
        event.logical_key = Key::Character("n".into());
        event.text = Some("n".into());
    }
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
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
            &mut state,
            &tree,
            node,
            Modifiers::default(),
            &named_key(NamedKey::ArrowDown),
        );
    }
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &named_key(NamedKey::Enter),
    );
    assert!(!app.note_menu_open_for_test());
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &mouse_press_right(),
    );
    assert!(app.note_menu_open_for_test());
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &named_key(NamedKey::ArrowDown),
    );
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &named_key(NamedKey::Enter),
    );
    assert!(
        app.editor_text_for_test().trim().is_empty(),
        "cut replaces the preserved document selection"
    );
    app.sync(&mut state.doc, &state.root);
    assert!(
        app.editor_text_for_test().trim().is_empty(),
        "cut survives canonical sync"
    );
    assert_eq!(app.workspace_pages_for_test().len(), pages_before);
}

#[test]
fn narrow_sidebar_context_menu_keeps_its_drawer_visible_when_selecting_a_note() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    app.apply_event_for_test(notes::sidebar::OutEvent::CreateChild { parent: None });
    let mut state = ui::State::new();
    let (tree, node) = settle_layout(&mut app, &mut state, 480, 800).unwrap();
    let toggle = app.menu_btn_center_for_test();
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &cursor_moved(toggle.x, toggle.y),
    );
    let mut left = mouse_press_right();
    if let WindowEvent::MouseInput { button, .. } = &mut left {
        *button = MouseButton::Left;
    }
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &left,
    );
    assert!(app.drawer_open_for_test());
    let (tree, node) = settle_layout(&mut app, &mut state, 480, 800).unwrap();
    let row = app.sidebar_row_center_for_test(0).unwrap();
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &cursor_moved(row.x, row.y),
    );
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &mouse_press_right(),
    );
    assert!(app.sidebar_menu_open_for_test());
    assert!(
        app.drawer_open_for_test(),
        "right-click selection must not hide its own menu"
    );
    dispatch(
        &mut app,
        &mut state,
        &tree,
        node,
        Modifiers::default(),
        &named_key(NamedKey::Escape),
    );
    assert!(!app.sidebar_menu_open_for_test());
    assert!(
        app.drawer_open_for_test(),
        "Escape dismisses the menu before its drawer"
    );
}
