use super::*;
use crate::actions::SNIPPETS_ACTION_TYPE;
use ui::headless::Framed;
use ui::widget::Widget;

fn library_of(app: &SnippetsApp) -> Vec<Snippet> {
    app.core.borrow().library.snippets.clone()
}

/// Live apps start and stay empty; the demo and an empty doc's sync seed.
#[test]
fn live_and_demo_apps_seed_only_when_asked() {
    // Case `live_app_starts_empty`.
    {
        let app = SnippetsApp::new();
        assert_eq!(app.library_len_for_test(), 0, "live library starts empty");
        assert!(
            app.open_id_for_test().is_none(),
            "nothing is opened on an empty library"
        );
        assert_eq!(app.visible_count_for_test(), 0);
    }

    // Case `demo_seeds_and_opens_a_snippet`.
    {
        let app = SnippetsApp::new_demo();
        assert!(app.library_len_for_test() >= 10);
        assert!(app.open_id_for_test().is_some());
        assert!(app.visible_count_for_test() >= 10);
    }

    // Case `empty_live_app_does_not_seed_the_shared_doc`.
    {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            SnippetsDoc::list(&doc).is_empty(),
            "an empty live library must write no snippets into the shared store"
        );
    }

    // Case `sync_seeds_into_empty_doc`.
    {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(SnippetsDoc::list(&doc).len(), app.library_len_for_test());
        let recorded = app
            .last_dispatched_for_test()
            .expect("starter snippets are created by an action");
        assert!(String::from_utf8_lossy(&recorded.payload).contains("CreateSnippet"));
    }
}

/// The sidebar lists the library's places with their counts, and choosing
/// one narrows the list.
#[test]
fn sidebar_places_narrow_the_list() {
    let app = SnippetsApp::new_demo();
    let sections = app.core.borrow().nav_sections();
    let ids: Vec<&str> = sections.iter().map(|section| section.id.as_str()).collect();
    assert_eq!(ids, ["library", "folders", "tags"]);

    let mut doc = AutoCommit::new();
    let all = app.visible_count_for_test();
    let mut app = app;
    app.requests
        .borrow_mut()
        .push(Action::Select(Selection::Smart(SmartFolder::Favorites)));
    app.settle(&mut doc);
    let favorites = app.visible_count_for_test();
    assert!(favorites > 0 && favorites < all);
    assert_eq!(
        app.titles.0, "Favorites",
        "the list column is titled by its place"
    );
    assert!(
        library_of(&app)
            .iter()
            .filter(|s| app.core.borrow().visible.contains(&s.id))
            .all(|s| s.favorite)
    );

    // An empty library has no folder or tag sections.
    let empty = SnippetsApp::new();
    let sections = empty.core.borrow().nav_sections();
    assert_eq!(sections.len(), 1);
}

/// Copying, creating and editing snippets.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn copy_and_creation_flows_update_the_library() {
    // Case `copying_a_plain_snippet_bumps_uses`.
    {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let id = library_of(&app)
            .iter()
            .find(|s| !s.has_placeholders())
            .map(|s| s.id)
            .expect("a placeholder-free snippet exists");
        let before = app.snippet_uses_for_test(id);
        app.copy_for_test(id, &mut doc);
        assert_eq!(app.snippet_uses_for_test(id), before + 1);
        assert!(!app.expand_open_for_test());
    }

    // Case `copying_a_templated_snippet_opens_expand`.
    {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let id = library_of(&app)
            .iter()
            .find(|s| s.has_placeholders())
            .map(|s| s.id)
            .expect("a templated snippet exists");
        app.copy_for_test(id, &mut doc);
        assert!(
            app.expand_open_for_test(),
            "templated copy routes through expand"
        );
    }

    // Case `focusing_new_snippet_does_not_activate_it`.
    {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.core
            .borrow_mut()
            .list
            .hits
            .push((Rect::new(0.0, 0.0, 100.0, 40.0), Action::NewSnippet));
        let target = Action::NewSnippet.semantic_id();
        let before = doc.get_heads();
        Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Focus,
            &mut doc,
            &automerge::ROOT,
        );
        assert_eq!(
            doc.get_heads(),
            before,
            "focusing a button must not author a snippet"
        );
        assert!(Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(SnippetsDoc::list(&doc).len(), 1);
    }

    // Case `new_snippet_grows_library_and_opens_it`.
    {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let before = app.library_len_for_test();
        app.new_snippet_for_test(&mut doc);
        assert_eq!(app.library_len_for_test(), before + 1);
        assert!(app.open_id_for_test().is_some());
    }

    // Case `new_snippet_body_can_be_edited_through_the_public_action_and_reloaded`.
    {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        app.new_snippet_for_test(&mut doc);
        app.sync(&mut doc, &root);
        let id = app.open_id_for_test().unwrap();
        let body = "fn main() {\n    println!(\"Grün ${name}\");\n}\n";
        assert!(Widget::perform_semantic_action(
            &mut app,
            "snippets.body",
            &ui::a11y::Action::SetValue(body.into()),
            &mut doc,
            &root,
        ));
        assert_eq!(SnippetsDoc::get(&doc, id).unwrap().body, body);

        let resource = ui::resources::resource(&app, &doc, &actions::snippet_resource_id(id))
            .expect("the edited snippet is an addressable resource");
        let edit = resource
            .actions
            .iter()
            .find(|action| action.id == "edit-body")
            .expect("body editing is available to automation and the palette");
        let encoded = match &edit.input {
            tilo_actions::ActionInput::Prompted { build, .. } => build(body),
            tilo_actions::ActionInput::Immediate(_)
            | tilo_actions::ActionInput::Deferred { .. } => {
                panic!("body editing accepts text")
            }
        };
        assert_eq!(app.last_dispatched_for_test().unwrap(), encoded);
        let mut loaded = AutoCommit::load(&doc.save()).unwrap();
        let mut reopened = SnippetsApp::new();
        reopened.sync(&mut loaded, &root);
        assert_eq!(SnippetsDoc::get(&loaded, id).unwrap().body, body);
        assert_eq!(reopened.library_len_for_test(), 1);
    }

    // Case `the_language_picker_sets_the_chosen_language`.
    {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let id = app.open_id_for_test().unwrap();
        app.requests
            .borrow_mut()
            .push(Action::SetLanguage(id, Language::Rust));
        app.settle(&mut doc);
        assert_eq!(SnippetsDoc::get(&doc, id).unwrap().language, Language::Rust);
        assert_eq!(
            app.detail,
            DetailKey::of(&app.core.borrow()),
            "the content column follows the new language"
        );
    }

    // Case `the_overflow_menu_opens_under_its_button_and_duplicates`.
    {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let id = app.open_id_for_test().unwrap();
        let before = app.library_len_for_test();
        {
            let mut core = app.core.borrow_mut();
            core.window = Rect::new(0.0, 0.0, 1280.0, 800.0);
            core.more_anchor = Rect::new(1200.0, 12.0, 1232.0, 44.0);
        }
        app.requests.borrow_mut().push(Action::More(id));
        app.settle(&mut doc);
        let menu = app
            .core
            .borrow()
            .menu
            .as_ref()
            .map(|open| Widget::get_rect(&open.menu))
            .expect("the overflow menu is open");
        assert!(menu.y0 >= 44.0, "the menu opens below its button");
        assert!(Widget::perform_semantic_action(
            &mut app,
            "snippets.menu:0",
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(app.library_len_for_test(), before + 1);
        assert!(app.core.borrow().menu.is_none(), "choosing closes the menu");
    }
}

/// The list is one focus stop whose keys move the open snippet.
#[test]
fn the_list_moves_the_open_snippet_by_keys() {
    let app = SnippetsApp::new_demo();
    let mut core = app.core.borrow_mut();
    let first = core.visible[0];
    let second = core.visible[1];
    let last = *core.visible.last().unwrap();
    core.open = Some(first);
    assert!(core.list_key(&Key::Named(NamedKey::ArrowDown)));
    assert_eq!(core.open, Some(second));
    assert!(core.list_key(&Key::Named(NamedKey::ArrowUp)));
    assert!(core.list_key(&Key::Named(NamedKey::ArrowUp)));
    assert_eq!(core.open, Some(first), "Up stops at the top");
    assert!(core.list_key(&Key::Named(NamedKey::End)));
    assert_eq!(core.open, Some(last));
    assert!(core.list_key(&Key::Named(NamedKey::Home)));
    assert_eq!(core.open, Some(first));
    assert!(!core.list_key(&Key::Character("a".into())));
}

/// The variable dialog is addressable, IME edits undo, and the body editor adopts peer changes.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn body_and_variable_editors_are_semantic_undoable_and_peer_aware() {
    // Case `variable_dialog_fields_and_cancel_are_semantically_addressable`.
    {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.new_snippet_for_test(&mut doc);
        let id = app.open_id_for_test().unwrap();
        app.perform_semantic_action(
            "snippets.body",
            &ui::a11y::Action::SetValue("Hello ${name}".into()),
            &mut doc,
            &automerge::ROOT,
        );
        app.core.borrow_mut().start_expand(id);
        assert!(app.perform_semantic_action(
            "snippets.expand.field.name",
            &ui::a11y::Action::SetValue("Grün".into()),
            &mut doc,
            &automerge::ROOT
        ));
        assert_eq!(
            app.core
                .borrow()
                .expand
                .as_ref()
                .unwrap()
                .preview_for_test(),
            "Hello Grün"
        );
        assert_eq!(SnippetsDoc::get(&doc, id).unwrap().body, "Hello ${name}");
        assert!(!app.perform_semantic_action(
            "snippets.body",
            &ui::a11y::Action::SetValue("hidden edit".into()),
            &mut doc,
            &automerge::ROOT
        ));
        assert!(app.perform_semantic_action(
            "snippets.expand.cancel",
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT
        ));
        assert!(app.core.borrow().expand.is_none());
    }

    // Case `body_ime_edit_uses_the_document_action_and_can_be_undone`.
    {
        let mut app = SnippetsApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        app.new_snippet_for_test(&mut state.doc);
        let id = app.open_id_for_test().unwrap();
        ui::undo::take_committed_action(&mut app);
        app.sync(&mut state.doc, &state.root);
        {
            // The content column is on screen with the editor laid out.
            let mut core = app.core.borrow_mut();
            core.body_reachable = true;
            core.body_editor
                .set_rect(Rect::new(300.0, 200.0, 900.0, 700.0));
        }
        assert!(app.perform_semantic_action(
            "snippets.body",
            &ui::a11y::Action::Focus,
            &mut state.doc,
            &state.root
        ));
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut handled = false;
        app.handle_event(
            &WindowEvent::Ime(winit::event::Ime::Commit("Grün\n  ${name}\n".into())),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(handled);
        assert_eq!(
            SnippetsDoc::get(&state.doc, id).unwrap().body,
            "Grün\n  ${name}\n"
        );
        let key = Key::Character("z".into());
        let undo_event = WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: KeyEvent::synthetic(
                winit::event::RawKeyEvent {
                    physical_key: winit::keyboard::PhysicalKey::Code(
                        winit::keyboard::KeyCode::KeyZ,
                    ),
                    state: ElementState::Pressed,
                },
                key.clone(),
                None,
                winit::keyboard::KeyLocation::Standard,
                false,
                None,
                key,
            ),
            is_synthetic: true,
        };
        handled = false;
        // The editor hands Ctrl+Z on to the host's document undo.
        let editor_took_undo = app.core.borrow_mut().route_body_event(
            &undo_event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL),
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(
            !editor_took_undo,
            "Ctrl+Z must reach the host's document undo dispatcher"
        );
        let author = ui::collaboration::local_user_id();
        let root: *mut dyn Widget = &mut app;
        assert!(ui::undo::record_dispatched_action(
            &mut state,
            root,
            &HashMap::new(),
            &author
        ));
        state.undo_as(Some(&author), None, None).unwrap();
        app.sync(&mut state.doc, &state.root);
        assert_eq!(SnippetsDoc::get(&state.doc, id).unwrap().body, "");
        assert_eq!(app.core.borrow().body_editor.value(), "");
    }

    // Case `body_editor_adopts_peer_changes_and_selection_without_authoring_edits`.
    {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.new_snippet_for_test(&mut doc);
        let first = app.open_id_for_test().unwrap();
        app.perform_semantic_action(
            "snippets.body",
            &ui::a11y::Action::SetValue("first\n".into()),
            &mut doc,
            &automerge::ROOT,
        );
        app.new_snippet_for_test(&mut doc);
        let second = app.open_id_for_test().unwrap();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.core.borrow().body_editor.value(), "");
        tilo_actions::Action::apply(&actions::set_body_action(second, "remote 🌍\n"), &mut doc)
            .unwrap();
        let heads = doc.get_heads();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.core.borrow().body_editor.value(), "remote 🌍\n");
        app.core.borrow_mut().open_snippet(first);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.core.borrow().body_editor.value(), "first\n");
        assert_eq!(doc.get_heads(), heads);
        assert!(app.core.borrow_mut().body_editor.take_dirty().is_none());
    }
}

// ---- responsive layout ---------------------------------------------

fn framed(app: SnippetsApp, width: u32) -> Framed<SnippetsApp> {
    Framed::new(ui::State::new(), app, (width, 800), Theme::dark())
}

/// The shell docks the sidebar when wide, makes it a drawer when narrow,
/// and pushes the snippet in front of the list when very narrow.
#[test]
fn the_shell_docks_wide_draws_narrow_and_pushes_very_narrow() {
    // Case `wide_keeps_sidebar_docked_and_open`.
    {
        let app = framed(SnippetsApp::new_demo(), 1280);
        assert!(
            !app.sidebar_is_overlay_for_test(),
            "wide window docks the sidebar"
        );
        assert!(app.sidebar_open_for_test());
        assert!(
            !app.is_push_for_test(),
            "wide window keeps list and snippet side by side"
        );
    }

    // Case `narrow_makes_the_sidebar_a_closed_drawer`.
    {
        let mut app = framed(SnippetsApp::new_demo(), 720);
        assert!(
            app.sidebar_is_overlay_for_test(),
            "narrow window floats the sidebar"
        );
        assert!(!app.sidebar_open_for_test(), "the drawer starts closed");
        app.toggle_sidebar_for_test();
        assert!(app.sidebar_open_for_test(), "the toggle opens the drawer");
        app.toggle_sidebar_for_test();
        assert!(!app.sidebar_open_for_test(), "and closes it again");
    }

    // Case `very_narrow_pushes_the_open_snippet`.
    {
        let mut app = framed(SnippetsApp::new_demo(), 480);
        assert!(app.is_push_for_test(), "very narrow window pushes");
        app.open_first_for_test();
        assert!(
            app.push_showing_detail_for_test(),
            "choosing a snippet pushes it in front of the list"
        );
    }

    // Case `wide_choice_does_not_push`.
    {
        let mut app = framed(SnippetsApp::new_demo(), 1280);
        app.open_first_for_test();
        assert!(!app.push_showing_detail_for_test());
    }
}

// ---- action model: affordances dispatch through the shared path ----

/// Seed a demo app and its shared document, as the live frame order does.
fn seeded_app() -> (SnippetsApp, AutoCommit) {
    let mut app = SnippetsApp::new_demo();
    let mut doc = AutoCommit::new();
    app.sync(&mut doc, &automerge::ROOT);
    (app, doc)
}

/// Delete and favorite dispatch the palette's actions, record one undo
/// boundary, and the doc stays authoritative.
#[test]
fn controls_dispatch_palette_actions_with_one_undo() {
    // Case `delete_dispatches_the_same_action_as_the_palette`.
    {
        let (mut app, mut doc) = seeded_app();
        let id = SnippetsDoc::list(&doc)[0].id;
        let resource_id = format!("snippets.snippet:{id}");
        let snip = ui::resources::resource(&app, &doc, &resource_id)
            .expect("a resource for the target snippet");
        let spec = snip.actions.iter().find(|a| a.id == "delete").unwrap();
        let spec_encoded = match &spec.input {
            tilo_actions::ActionInput::Immediate(e) => e.clone(),
            tilo_actions::ActionInput::Prompted { .. }
            | tilo_actions::ActionInput::Deferred { .. } => {
                panic!("delete is immediate")
            }
        };

        app.delete_for_test(id, &mut doc);

        let dispatched = app
            .last_dispatched_for_test()
            .expect("the control dispatched an action");
        assert_eq!(dispatched.type_id, SNIPPETS_ACTION_TYPE);
        assert_eq!(dispatched.type_id, spec_encoded.type_id);
        assert_eq!(
            dispatched.payload, spec_encoded.payload,
            "the control and the palette build the identical EncodedAction",
        );
        assert!(SnippetsDoc::get(&doc, id).is_none());
        assert!(!library_of(&app).iter().any(|s| s.id == id));
    }

    // Case `favorite_dispatches_the_same_action_as_the_palette`.
    {
        let (mut app, mut doc) = seeded_app();
        let id = SnippetsDoc::list(&doc)
            .into_iter()
            .find(|s| !s.favorite)
            .map(|s| s.id)
            .expect("an unstarred seeded snippet");
        let resource_id = format!("snippets.snippet:{id}");
        let snip = ui::resources::resource(&app, &doc, &resource_id).unwrap();
        let spec = snip
            .actions
            .iter()
            .find(|a| a.id == "toggle-favorite")
            .unwrap();
        let spec_encoded = match &spec.input {
            tilo_actions::ActionInput::Immediate(e) => e.clone(),
            tilo_actions::ActionInput::Prompted { .. }
            | tilo_actions::ActionInput::Deferred { .. } => {
                panic!("toggle-favorite is immediate")
            }
        };

        app.toggle_favorite_for_test(id, &mut doc);

        let dispatched = app.last_dispatched_for_test().unwrap();
        assert_eq!(dispatched.payload, spec_encoded.payload);
        assert!(SnippetsDoc::get(&doc, id).unwrap().favorite);
    }

    // Case `control_records_one_undo_boundary_through_the_host`.
    {
        let mut app = SnippetsApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let id = SnippetsDoc::list(&state.doc)
            .into_iter()
            .find(|s| !s.favorite)
            .map(|s| s.id)
            .expect("an unstarred snippet");

        app.toggle_favorite_for_test(id, &mut state.doc);
        assert!(SnippetsDoc::get(&state.doc, id).unwrap().favorite);

        let local_author = ui::collaboration::local_user_id();
        let root: *mut dyn Widget = &mut app;
        assert!(
            ui::undo::record_dispatched_action(
                &mut state,
                root,
                &std::collections::HashMap::new(),
                &local_author,
            ),
            "the host recorded an undo boundary for the dispatched favorite"
        );
        assert!(
            ui::undo::take_committed_action(&mut app).is_none(),
            "rising-edge: the captured heads are cleared on read",
        );
        let entry = state
            .peek_last_undo()
            .expect("exactly one undo boundary recorded");
        assert_eq!(entry.author.as_ref(), Some(&local_author));

        state
            .undo_as(Some(&local_author), None, None)
            .expect("an undo entry to pop");
        app.sync(&mut state.doc, &state.root);
        assert!(!SnippetsDoc::get(&state.doc, id).unwrap().favorite);
    }

    // Case `steady_state_reprojects_the_document`.
    {
        let (mut app, mut doc) = seeded_app();
        let before = app.library_len_for_test();
        let id = SnippetsDoc::list(&doc)[0].id;
        SnippetsDoc::remove(&mut doc, id);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.library_len_for_test(), before - 1);
        assert!(!library_of(&app).iter().any(|s| s.id == id));
    }
}

/// The content column's controls keep their measured size across a window
/// resize and a rebuild for another snippet, and its header buttons follow
/// the open snippet.
#[test]
fn the_content_column_lays_out_its_controls() {
    let mut app = framed(SnippetsApp::new_demo(), 1280);
    let sizes = |app: &Framed<SnippetsApp>| {
        let id = app.open_id_for_test().unwrap();
        let targets = Framed::harness(app).targets();
        let rect = |name: String| targets.get(&name).copied().unwrap_or(Rect::ZERO);
        (
            rect(format!("snippets.snippet:{id}.language")),
            rect(format!("snippets.snippet:{id}.copy")),
        )
    };
    let (language, copy) = sizes(&app);
    assert!(language.width() > language.height() && copy.width() > copy.height());
    Framed::harness_mut(&mut app).resize((960, 800)).unwrap();
    assert_eq!(sizes(&app).1.size(), copy.size(), "a resize keeps the size");
    let other = app.core.borrow().visible[2];
    app.requests.borrow_mut().push(Action::OpenSnippet(other));
    Framed::harness_mut(&mut app).relayout();
    let (language, copy) = sizes(&app);
    assert_eq!(app.open_id_for_test(), Some(other));
    assert!(language.width() > language.height() && copy.width() > copy.height());
    let targets = Framed::harness(&app).targets();
    assert!(targets.contains_key("snippets.favorite") && targets.contains_key("snippets.more"));
}
