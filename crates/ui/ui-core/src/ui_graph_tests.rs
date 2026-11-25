//! The UI graph editor's tests, kept beside the code they read.

use super::*;

/// Replace `path`'s contents and move its mtime a second on, so the change is seen on
/// a filesystem whose timestamps are coarser than the time between two writes.
fn rewrite(path: &std::path::Path, contents: impl AsRef<[u8]>) {
    let before = std::fs::metadata(path).unwrap().modified().unwrap();
    std::fs::write(path, contents).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(before + std::time::Duration::from_secs(1))
        .unwrap();
}
use crate::a11y::{Action, Role};
use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT};

#[test]
fn graph_checked_controls_share_view_values_without_document_writes() {
    let document = UiDocument::from_json(
        r#"{"version":1,"root":{"type":"column","id":"root","children":[{"type":"checkbox","id":"check","value_path":"shared","accessible_name":"Check"},{"type":"toggle","id":"toggle","value_path":"shared","accessible_name":"Toggle"}]}}"#,
    ).unwrap();
    let context = UiBuildContext::default();
    let widget = document.build_with(&Theme::dark(), &context).unwrap();
    let mut driver = crate::headless::Driver::new(crate::State::new(), widget, (360, 200));
    let before = driver.dump_json();
    driver.perform_named("check", &Action::ToggleCheck).unwrap();
    driver
        .expect(&crate::headless::SemanticExpectation::named("toggle").checked(true))
        .unwrap();
    assert_eq!(context.values().get_bool("shared"), Some(true));
    driver
        .perform_named("toggle", &Action::ToggleCheck)
        .unwrap();
    driver
        .expect(&crate::headless::SemanticExpectation::named("check").checked(false))
        .unwrap();
    assert_eq!(context.values().get_bool("shared"), Some(false));
    assert_eq!(
        driver.dump_json(),
        before,
        "unrouted graph controls edit view state only"
    );
}

fn graph_field(id: &str, value_path: &str) -> UiNode {
    UiNode::TextInput {
        id: id.into(),
        label: None,
        accessible_name: id.into(),
        placeholder: String::new(),
        value_path: Some(value_path.into()),
        initial_value: String::new(),
        multiline: false,
        disabled: false,
    }
}

#[test]
fn a_routed_path_edits_through_the_apps_action_and_shows_its_value() {
    use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};
    let spec = ActionSpec::typed_operation(
        "rename",
        "Rename",
        "The new title",
        OperationKind::Effect,
        "graph.rename",
        vec![ActionParam::required("title", ParamType::Text, "Title")],
        |args| EncodedAction::encode_operation("graph.rename", &args.to_json()).unwrap(),
    );
    let resource =
        tilo_actions::ResourceSpec::new("ordinary:graph", "graph", "Graph").with_action(spec);
    let action = tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |title| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("title".into(), serde_json::Value::String(title));
        arguments
    })
    .unwrap();
    let title = LocalState::new("Atlas".to_owned());
    let values = UiValues::default();
    values.route("title", title.clone(), action);
    let context = UiBuildContext::default().with_values(values.clone());
    let document = UiDocument::new(UiNode::Column {
        id: "root".into(),
        children: vec![
            graph_field("title-a", "title"),
            graph_field("title-b", "title"),
            graph_field("draft", "draft"),
        ],
        gap: None,
        no_padding: false,
        intrinsic_height: false,
        fill_remaining_height: false,
    });
    let mut root = document.build_with(&Theme::dark(), &context).unwrap();
    crate::widget::controls::refresh_control_action_availability(
        root.as_mut(),
        &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
            "ordinary:graph",
            "rename",
        )]),
    );
    let mut doc = AutoCommit::new();
    Widget::sync(root.as_mut(), &doc, &ROOT);
    assert_eq!(
        named_value(root.as_ref(), "title-b").as_deref(),
        Some("Atlas")
    );

    let set = |root: &mut Element, doc: &mut AutoCommit, id: &str, value: &str| {
        assert!(perform_named(
            root.as_mut(),
            id,
            &Action::SetValue(value.into()),
            doc
        ));
    };
    set(&mut root, &mut doc, "title-a", "Atlas II");
    let invocation = crate::widget::controls::take_control_invocation(root.as_mut())
        .expect("the edit is the app's action");
    assert_eq!(invocation.address.action, "rename");
    assert_eq!(invocation.arguments["title"], "Atlas II");
    assert!(
        automerge::ReadDoc::get(&doc, &ROOT, "title")
            .unwrap()
            .is_none(),
        "the input never writes the document"
    );
    title.set("Atlas II".to_owned());
    Widget::sync(root.as_mut(), &doc, &ROOT);
    assert_eq!(
        named_value(root.as_ref(), "title-b").as_deref(),
        Some("Atlas II")
    );

    set(&mut root, &mut doc, "draft", "Notes");
    assert!(
        crate::widget::controls::take_control_invocation(root.as_mut()).is_none(),
        "an unrouted path invokes nothing"
    );
    assert_eq!(values.get("draft").as_deref(), Some("Notes"));
    assert!(
        automerge::ReadDoc::get(&doc, &ROOT, "draft")
            .unwrap()
            .is_none()
    );
}

fn perform_named(widget: &mut dyn Widget, id: &str, action: &Action, doc: &mut AutoCommit) -> bool {
    crate::widget::find_test_id_mut(widget, id).is_some_and(|widget| {
        Widget::perform_action(
            widget,
            action,
            &[],
            &mut crate::widget::Edits::lend(doc),
            &ROOT,
        )
    })
}

fn named_value(widget: &dyn Widget, id: &str) -> Option<String> {
    crate::widget::find_test_id(widget, id)?
        .accessibility()?
        .value
}

fn run_operation(
    live: &mut impl Widget,
    kind: crate::tilo_actions::OperationKind,
    type_id: &str,
    payload: &[u8],
) -> Result<Option<crate::tilo_actions::ActionOutput>, String> {
    crate::widget::ResourceOperation::local(
        crate::tilo_actions::InvocationId::new(),
        kind,
        type_id,
        payload,
    )
    .start(
        live,
        &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
        &ROOT,
    )
    .map(|started| started.and_then(crate::widget::ResourceOperationStart::completed))
}

fn sample() -> UiDocument {
    UiDocument::new(UiNode::Column {
        id: "root".into(),
        children: vec![
            UiNode::Text {
                id: "title-view".into(),
                text: UiTextSource::Document {
                    path: "title".into(),
                },
                role: UiTextRole::Title,
            },
            UiNode::TextInput {
                id: "title-input".into(),
                label: Some("Title".into()),
                accessible_name: String::new(),
                placeholder: "Untitled".into(),
                value_path: Some("title".into()),
                initial_value: String::new(),
                multiline: false,
                disabled: false,
            },
            UiNode::Button {
                id: "save".into(),
                label: "Save".into(),
                accessible_name: None,
                tooltip: Some("Save document".into()),
                on_click_path: Some("save-requested".into()),
                click_value: Some("yes".into()),
                navigate: None,
                enabled: true,
                role: UiButtonRole::Primary,
            },
        ],
        gap: Some(8.0),
        no_padding: false,
        intrinsic_height: false,
        fill_remaining_height: false,
    })
}

#[test]
fn app_component_factories_receive_props_and_keep_graph_lifecycle() {
    let document = UiDocument::from_json(
        r#"{"version":1,"root":{"type":"component","id":"profile","name":"profile-card","props":{"title":"Ada"}}}"#,
    )
    .unwrap();
    assert!(matches!(
        document.build(&Theme::dark()),
        Err(UiGraphError::UnknownComponent { id, name })
            if id == "profile" && name == "profile-card"
    ));

    let seen = Rc::new(RefCell::new(Vec::new()));
    let seen_by_factory = Rc::clone(&seen);
    let context = UiBuildContext::default().with_component("profile-card", move |request| {
        seen_by_factory.borrow_mut().push((
            request.id.to_owned(),
            request.name.to_owned(),
            request.props.get("title").cloned(),
        ));
        let title = request
            .props
            .get("title")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "title must be a string".to_owned())?;
        Ok(Box::new(Text::new(title).with_id(request.id)))
    });
    let widget = document.build_with(&Theme::dark(), &context).unwrap();
    let mut app = crate::headless::Driver::new(crate::State::new(), widget, (320, 120));
    app.expect(
        &crate::headless::SemanticExpectation::named("profile")
            .role(Role::StaticText)
            .name("Ada"),
    )
    .expect("factory result remains a normal graph widget");
    assert_eq!(
        seen.borrow().as_slice(),
        [(
            "profile".to_owned(),
            "profile-card".to_owned(),
            Some(serde_json::json!("Ada"))
        )]
    );

    let failing = UiBuildContext::default()
        .with_component("profile-card", |_| Err("missing account".to_owned()));
    assert!(matches!(
        document.build_with(&Theme::dark(), &failing),
        Err(UiGraphError::ComponentBuild { message, .. }) if message == "missing account"
    ));
}

#[test]
fn builder_previews_and_edits_unloaded_app_components() {
    let document = UiDocument::from_json(
        r#"{"version":1,"root":{"type":"component","id":"profile","name":"profile-card","props":{"title":"Ada"}}}"#,
    )
    .unwrap();
    let handle = UiGraphHandle::new(document).unwrap();
    let editor = UiGraphEditor::new(handle.clone(), Theme::dark()).unwrap();
    let mut app = crate::headless::Driver::new(crate::State::new(), Box::new(editor), (1200, 720));
    app.expect(
        &crate::headless::SemanticExpectation::named("profile")
            .role(Role::StaticText)
            .name("Component: profile-card"),
    )
    .expect("builder uses a named placeholder without app code");

    let mut component = handle.node("profile").unwrap();
    set_editable_property(
        &mut component,
        EditableProperty::ComponentProps,
        parse_editable_property(EditableProperty::ComponentProps, r#"{"title":"Grace"}"#).unwrap(),
    );
    handle.replace_node("profile", component).unwrap();
    assert!(matches!(
        handle.node("profile"),
        Ok(UiNode::Component { props, .. })
            if props.get("title") == Some(&serde_json::json!("Grace"))
    ));
}

#[test]
fn graph_json_round_trips_and_builds_real_accessible_widgets() {
    fn has_title(node: &crate::headless::InspectionNode) -> bool {
        node.semantics.as_ref().is_some_and(|semantics| {
            semantics.role == (Role::Heading { level: 1 })
                && semantics.name.as_deref() == Some("Roadmap")
        }) || node.children.iter().any(has_title)
    }

    let document = sample();
    let json = document.to_json_pretty().unwrap();
    let reparsed = UiDocument::from_json(&json).unwrap();
    assert_eq!(reparsed, document);
    let mut title_doc = AutoCommit::new();
    let text = title_doc.put_object(&ROOT, "title", ObjType::Text).unwrap();
    title_doc.splice_text(&text, 0, 0, "Roadmap").unwrap();
    let values = UiValues::default();
    let context = UiBuildContext::default().with_values(values.clone());
    let mut driver = crate::headless::Driver::new(
        crate::State::from_doc(title_doc),
        reparsed.build_with(&Theme::dark(), &context).unwrap(),
        (640, 360),
    );
    let inspection = driver.inspection();
    assert!(has_title(&inspection.root));
    driver
        .expect(
            &crate::headless::SemanticExpectation::named("title-input")
                .role(Role::TextInput)
                .name("Title"),
        )
        .expect("serialized label names the real input");
    assert!(
        driver.targets().contains_key("title-view"),
        "serialized text ids remain addressable in scenarios"
    );
    driver
        .perform_named("save", &Action::Activate)
        .expect("serialized button is a real button");
    assert_eq!(values.take_click("save-requested").as_deref(), Some("yes"));
    assert_eq!(
        values.take_click("save-requested"),
        None,
        "a click is taken once"
    );
    assert!(
        !driver.dump_json().contains("save-requested"),
        "the button never writes the document"
    );
}

#[test]
fn serialized_check_label_builds_visible_and_spoken_semantics() {
    fn has_visible_label(node: &crate::headless::InspectionNode) -> bool {
        node.semantics.as_ref().is_some_and(|semantics| {
            semantics.role == Role::StaticText && semantics.name.as_deref() == Some("Remember me")
        }) || node.children.iter().any(has_visible_label)
    }

    let document = UiDocument::new(UiNode::Checkbox {
        id: "remember".into(),
        label: Some("Remember me".into()),
        accessible_name: String::new(),
        value_path: "remember".into(),
        enabled: true,
    });
    let mut driver = crate::headless::Driver::new(
        crate::State::new(),
        document.build(&Theme::dark()).unwrap(),
        (360, 120),
    );

    driver
        .expect(
            &crate::headless::SemanticExpectation::named("remember")
                .role(Role::Checkbox)
                .name("Remember me"),
        )
        .expect("serialized label names the checkbox");
    assert!(has_visible_label(&driver.inspection().root));
}

#[test]
fn editor_label_fields_clear_the_fallback_name_and_vice_versa() {
    let mut node = UiNode::Toggle {
        id: "sync".into(),
        label: None,
        accessible_name: "Unlabeled sync".into(),
        value_path: "sync".into(),
        enabled: true,
    };

    set_editable_property(
        &mut node,
        EditableProperty::CheckLabel,
        ParsedProperty::OptionalText(Some("Sync changes".into())),
    );
    assert!(matches!(
        &node,
        UiNode::Toggle {
            label: Some(label),
            accessible_name,
            ..
        } if label == "Sync changes" && accessible_name.is_empty()
    ));

    set_editable_property(
        &mut node,
        EditableProperty::CheckAccessibleName,
        ParsedProperty::Text("Background synchronization".into()),
    );
    assert!(matches!(
        &node,
        UiNode::Toggle {
            label: None,
            accessible_name,
            ..
        } if accessible_name == "Background synchronization"
    ));
}

#[test]
fn routed_graph_buttons_publish_navigation_and_leave_the_document_alone() {
    fn screen(id: &str, title: &str, button: UiNode) -> UiDocument {
        UiDocument::new(UiNode::Column {
            id: id.into(),
            children: vec![
                UiNode::Text {
                    id: format!("{id}-title"),
                    text: UiTextSource::Static {
                        value: title.into(),
                    },
                    role: UiTextRole::Title,
                },
                button,
            ],
            gap: None,
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        })
    }

    let home = screen(
        "home-root",
        "Home",
        UiNode::Button {
            id: "open-settings".into(),
            label: "Settings".into(),
            accessible_name: None,
            tooltip: None,
            on_click_path: None,
            click_value: None,
            navigate: Some(UiNavigation::Push {
                route: "settings".into(),
            }),
            enabled: true,
            role: UiButtonRole::Primary,
        },
    );
    let settings = screen(
        "settings-root",
        "Settings screen",
        UiNode::Button {
            id: "back-home".into(),
            label: "Back".into(),
            accessible_name: None,
            tooltip: None,
            on_click_path: None,
            click_value: None,
            navigate: Some(UiNavigation::Back),
            enabled: true,
            role: UiButtonRole::Secondary,
        },
    );
    let router = crate::widget::Router::new(
        "home",
        ["home", "settings"],
        &Theme::dark(),
        move |route, theme, handle| {
            let document = if route == "home" { &home } else { &settings };
            document
                .build_with(
                    theme,
                    &UiBuildContext::default().with_router(handle.clone()),
                )
                .map_err(|error| error.to_string())
        },
    )
    .unwrap();
    let handle = router.handle();
    let mut driver =
        crate::headless::Driver::new(crate::State::new(), Box::new(router), (640, 360));
    let document_before = driver.dump_json();

    driver
        .perform_named("open-settings", &Action::Activate)
        .unwrap();
    assert_eq!(handle.current(), "settings");
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Settings screen")
    );
    driver
        .perform_named("back-home", &Action::Activate)
        .unwrap();
    assert_eq!(handle.current(), "home");
    assert_eq!(driver.dump_json(), document_before);

    let audit = driver.native_app_audit();
    assert!(audit.is_certified(), "{:?}", audit.issues);
    driver
        .perform_named("open-settings", &crate::a11y::Action::Activate)
        .unwrap();
    assert_eq!(handle.current(), "settings");
    assert!(driver.native_app_audit().is_certified());
    assert_eq!(driver.dump_json(), document_before);
}

#[test]
fn navigation_to_an_unknown_registered_screen_is_disabled() {
    fn find_missing(node: &crate::headless::InspectionNode) -> Option<&crate::a11y::AccessNode> {
        node.semantics
            .as_ref()
            .filter(|semantics| semantics.name.as_deref() == Some("Missing"))
            .or_else(|| node.children.iter().find_map(find_missing))
    }

    let mut document = sample();
    if let UiNode::Column { children, .. } = &mut document.root {
        children.push(UiNode::Button {
            id: "missing".into(),
            label: "Missing".into(),
            accessible_name: None,
            tooltip: None,
            on_click_path: None,
            click_value: None,
            navigate: Some(UiNavigation::Push {
                route: "missing".into(),
            }),
            enabled: true,
            role: UiButtonRole::Secondary,
        });
    }
    let route = crate::widget::RouteHandle::new("home", ["home"]).unwrap();
    let root = document
        .build_with(
            &Theme::dark(),
            &UiBuildContext::default().with_router(route.clone()),
        )
        .unwrap();
    let mut driver = crate::headless::Driver::new(crate::State::new(), root, (640, 360));
    let inspection = driver.inspection();
    let missing = find_missing(&inspection.root).expect("missing-route button remains visible");
    assert!(missing.state.disabled);
    driver
        .perform_named("missing", &Action::Activate)
        .expect("a disabled button ignores activation without crashing");
    assert_eq!(route.current(), "home");
}

tilo_test_env::scenario! {
    fn an_invalid_graph_is_rejected_and_leaves_the_last_good_one;

    phase exhausted_graph_revision_rejects_a_mutation_without_reusing_live_state;
    {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let before = handle.document().unwrap();
        handle.inner.write().unwrap().revision = u64::MAX;
        let replacement = UiDocument::new(UiNode::Text {
            id: "replacement".into(),
            text: UiTextSource::Static {
                value: "Replacement".into(),
            },
            role: UiTextRole::Body,
        });
        assert!(matches!(
            handle.replace(replacement),
            Err(UiGraphError::RevisionExhausted)
        ));
        assert_eq!(handle.document().unwrap(), before);
    }

    phase validation_is_atomic_and_accessibility_is_required;
    {
        let handle = UiGraphHandle::new(sample()).unwrap();
        let before = handle.to_json_pretty().unwrap();
        let error = handle
            .update_node("save", |node| {
                if let UiNode::Button {
                    label,
                    accessible_name,
                    ..
                } = node
                {
                    label.clear();
                    *accessible_name = None;
                }
            })
            .unwrap_err();
        assert!(matches!(error, UiGraphError::MissingAccessibleName(_)));
        assert_eq!(handle.to_json_pretty().unwrap(), before);
    }

    phase duplicate_ids_and_unknown_fields_are_rejected;
    {
        let mut duplicate = sample();
        if let UiNode::Column { children, .. } = &mut duplicate.root {
            *children.last_mut().unwrap() = UiNode::Spacer {
                id: "title-input".into(),
            };
        }
        assert!(matches!(
            duplicate.validate(),
            Err(UiGraphError::DuplicateId(_))
        ));
        let json = r#"{"version":1,"root":{"type":"spacer","id":"root","mystery":1}}"#;
        assert!(matches!(
            UiDocument::from_json(json),
            Err(UiGraphError::Json(_))
        ));
    }
}

#[test]
fn live_edit_rebuilds_the_real_subtree_and_round_trips() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    assert_eq!(handle.document().unwrap().nodes().len(), 4);
    assert!(matches!(handle.node("save"), Ok(UiNode::Button { .. })));
    let live = handle.widget(&Theme::dark()).unwrap();
    let mut driver = crate::headless::Driver::new(crate::State::new(), Box::new(live), (640, 360));
    assert!(driver.inspection_json().unwrap().contains("Save"));

    handle
        .update_node("save", |node| {
            if let UiNode::Button { label, role, .. } = node {
                *label = "Publish".into();
                *role = UiButtonRole::Destructive;
            }
        })
        .unwrap();
    driver.relayout();
    assert!(driver.inspection_json().unwrap().contains("Publish"));
    assert!(
        !driver
            .inspection_json()
            .unwrap()
            .contains("\"name\": \"Save\"")
    );
    let saved = handle.to_json_pretty().unwrap();
    let reopened = UiDocument::from_json(&saved).unwrap();
    assert!(matches!(
        reopened.node("save"),
        Some(UiNode::Button { label, .. }) if label == "Publish"
    ));
}

fn set_named_state(widget: &mut dyn Widget, id: &str, state: &str) -> bool {
    let Some(widget) = crate::widget::find_test_id_mut(widget, id) else {
        return false;
    };
    widget.set_focus(true);
    crate::widget::undo_points::set_state(widget, state);
    true
}

fn named_state(widget: &dyn Widget, id: &str) -> Option<String> {
    crate::widget::undo_points::get_state(crate::widget::find_test_id(widget, id)?)
}

fn named_widget_ptr(widget: &dyn Widget, id: &str) -> Option<usize> {
    crate::widget::find_test_id(widget, id)
        .map(|widget| (widget as *const dyn Widget).cast::<()>().addr())
}

#[test]
fn live_reconcile_preserves_text_input_ephemeral_state() {
    let document = UiDocument::new(UiNode::Column {
        id: "root".into(),
        children: vec![
            UiNode::Text {
                id: "title".into(),
                text: UiTextSource::Static {
                    value: "Hello".into(),
                },
                role: UiTextRole::Title,
            },
            UiNode::TextInput {
                id: "name".into(),
                label: None,
                accessible_name: "Name".into(),
                placeholder: "Type here".into(),
                value_path: None,
                initial_value: "Ada".into(),
                multiline: false,
                disabled: false,
            },
        ],
        gap: Some(8.0),
        no_padding: false,
        intrinsic_height: false,
        fill_remaining_height: false,
    });
    let handle = UiGraphHandle::new(document).unwrap();
    let mut live = handle.widget(&Theme::dark()).unwrap();
    assert!(set_named_state(&mut live, "name", "2"));
    let before = named_state(&live, "name");
    assert_eq!(before.as_deref(), Some("2"));
    let before_ptr = named_widget_ptr(&live, "name").expect("name widget");

    handle
        .update_node("title", |node| {
            if let UiNode::Text {
                text: UiTextSource::Static { value },
                ..
            } = node
            {
                *value = "Projects".into();
            }
        })
        .unwrap();
    let doc = AutoCommit::new();
    Widget::sync(&mut live, &doc, &automerge::ROOT);
    assert_eq!(named_state(&live, "name"), before);
    assert_eq!(
        named_widget_ptr(&live, "name"),
        Some(before_ptr),
        "unchanged TextInput must keep the same widget instance"
    );
    assert!(live.take_structure_dirty());
}

#[test]
fn live_reconcile_transfers_ephemeral_state_when_leaf_props_change() {
    let document = UiDocument::new(UiNode::Column {
        id: "root".into(),
        children: vec![UiNode::TextInput {
            id: "name".into(),
            label: None,
            accessible_name: "Name".into(),
            placeholder: "Before".into(),
            value_path: None,
            initial_value: "Ada".into(),
            multiline: false,
            disabled: false,
        }],
        gap: None,
        no_padding: false,
        intrinsic_height: false,
        fill_remaining_height: false,
    });
    let handle = UiGraphHandle::new(document).unwrap();
    let mut live = handle.widget(&Theme::dark()).unwrap();
    assert!(set_named_state(&mut live, "name", "4"));
    let before = named_state(&live, "name");
    assert_eq!(before.as_deref(), Some("4"));
    let before_ptr = named_widget_ptr(&live, "name").expect("name widget");

    handle
        .apply_edits(vec![UiGraphEdit::SetProperty {
            id: "name".into(),
            property: "placeholder".into(),
            value: serde_json::json!("After"),
        }])
        .unwrap();
    let doc = AutoCommit::new();
    Widget::sync(&mut live, &doc, &automerge::ROOT);
    assert_eq!(named_state(&live, "name"), before);
    assert_ne!(
        named_widget_ptr(&live, "name"),
        Some(before_ptr),
        "leaf property changes rebuild the widget"
    );
    assert!(matches!(
        handle.node("name"),
        Ok(UiNode::TextInput { placeholder, .. }) if placeholder == "After"
    ));
}

#[test]
fn live_graph_reject_bad_apply_keeps_last_good_tree() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    let mut live = handle.widget(&Theme::dark()).unwrap();
    let before = handle.to_json_pretty().unwrap();
    let edits = r#"[{"op":"set-property","id":"save","property":"label","value":""}]"#;
    let payload = serde_json::to_vec(&serde_json::json!({ "edits": edits })).unwrap();
    let error = run_operation(
        &mut live,
        crate::tilo_actions::OperationKind::Effect,
        "tilo.ui.graph.apply",
        &payload,
    )
    .unwrap_err();
    assert!(
        error.contains("edit") || error.contains("Unknown") || error.contains("missing"),
        "rejected apply must surface an error, got {error}"
    );
    assert_eq!(handle.to_json_pretty().unwrap(), before);
    let doc = AutoCommit::new();
    Widget::sync(&mut live, &doc, &automerge::ROOT);
    assert!(
        crate::headless::Driver::new(crate::State::new(), Box::new(live), (640, 360))
            .inspection_json()
            .unwrap()
            .contains("Save"),
        "last good tree must stay on screen after a rejected apply"
    );
}

#[test]
fn poll_live_source_rejects_invalid_json_without_replacing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.json");
    std::fs::write(
        &path,
        r#"{"version":1,"root":{"type":"text","id":"title","text":{"source":"static","value":"Good"},"role":"title"}}"#,
    )
    .unwrap();
    let handle = UiGraphHandle::open_live(&path).unwrap();
    let before = handle.to_json_pretty().unwrap();
    rewrite(&path, "{not-valid-graph");
    assert!(handle.poll_live_source().is_err());
    assert_eq!(handle.to_json_pretty().unwrap(), before);
    rewrite(
        &path,
        r#"{"version":1,"root":{"type":"button","id":"title","label":"","accessibleName":null}}"#,
    );
    assert!(handle.poll_live_source().is_err());
    assert_eq!(handle.to_json_pretty().unwrap(), before);
}

#[test]
fn live_graph_actions_apply_and_dump() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    let mut live = handle.widget(&Theme::dark()).unwrap();
    let resources = crate::widget::resources::resources(&live, &AutoCommit::new());
    assert!(
        resources
            .iter()
            .any(|resource| resource.kind == "tilo.ui-graph")
    );
    let edits = r#"[{"op":"set-property","id":"save","property":"label","value":"Publish"}]"#;
    let payload = serde_json::to_vec(&serde_json::json!({ "edits": edits })).unwrap();
    let output = run_operation(
        &mut live,
        crate::tilo_actions::OperationKind::Effect,
        "tilo.ui.graph.apply",
        &payload,
    )
    .unwrap()
    .unwrap();
    assert!(output.value.is_none());
    assert!(matches!(
        handle.node("save"),
        Ok(UiNode::Button { label, .. }) if label == "Publish"
    ));
    let dumped = run_operation(
        &mut live,
        crate::tilo_actions::OperationKind::View,
        "tilo.ui.graph.dump",
        b"null",
    )
    .unwrap()
    .unwrap();
    assert!(
        dumped
            .value
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .is_some_and(|json| json.contains("Publish"))
    );
}

#[test]
fn open_live_reloads_when_the_file_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.json");
    std::fs::write(
        &path,
        r#"{"version":1,"root":{"type":"text","id":"title","text":{"source":"static","value":"Before"},"role":"title"}}"#,
    )
    .unwrap();
    let handle = UiGraphHandle::open_live(&path).unwrap();
    assert!(matches!(
        handle.node("title"),
        Ok(UiNode::Text {
            text: UiTextSource::Static { value },
            ..
        }) if value == "Before"
    ));
    rewrite(
        &path,
        r#"{"version":1,"root":{"type":"text","id":"title","text":{"source":"static","value":"After"},"role":"title"}}"#,
    );
    assert!(handle.poll_live_source().unwrap());
    assert!(matches!(
        handle.node("title"),
        Ok(UiNode::Text {
            text: UiTextSource::Static { value },
            ..
        }) if value == "After"
    ));
}

tilo_test_env::scenario! {
    fn structural_edits_are_validated_and_atomic;

    phase structural_insert_is_validated_and_atomic;
    {
        let mut document = sample();
        document
            .insert_child(
                "root",
                1,
                UiNode::Text {
                    id: "subtitle".into(),
                    text: UiTextSource::Static {
                        value: "A small project".into(),
                    },
                    role: UiTextRole::Caption,
                },
            )
            .unwrap();
        let ids: Vec<_> = document.nodes().iter().map(|node| node.id()).collect();
        assert_eq!(
            ids,
            ["root", "title-view", "subtitle", "title-input", "save"]
        );

        let before = document.clone();
        let error = document
            .insert_child("root", 0, UiNode::Spacer { id: "save".into() })
            .unwrap_err();
        assert!(matches!(error, UiGraphError::DuplicateId(id) if id == "save"));
        assert_eq!(document, before, "a failed structural edit is atomic");
    }

    phase move_reparents_and_reorders_without_losing_the_subtree;
    {
        let mut document = sample();
        document
            .insert_child(
                "root",
                1,
                UiNode::Row {
                    id: "actions".into(),
                    children: vec![UiNode::Spacer {
                        id: "action-gap".into(),
                    }],
                    no_wrap: true,
                    fill_remaining_height: false,
                },
            )
            .unwrap();
        document.move_node("save", "actions", 0).unwrap();
        let actions = document.node("actions").unwrap();
        assert_eq!(
            actions
                .children()
                .iter()
                .map(|child| child.id())
                .collect::<Vec<_>>(),
            ["save", "action-gap"]
        );
        assert!(matches!(
            document.move_node("actions", "save", 0),
            Err(UiGraphError::MoveIntoDescendant { .. })
        ));
        assert!(document.node("save").is_some());
    }

    phase remove_and_replace_obey_root_and_container_invariants;
    {
        let mut document = UiDocument::new(UiNode::Container {
            id: "root".into(),
            child: Box::new(UiNode::Text {
                id: "body".into(),
                text: UiTextSource::Static {
                    value: "Before".into(),
                },
                role: UiTextRole::Body,
            }),
            background: true,
            no_padding: false,
            fill_height: false,
            width: None,
            height: None,
        });
        assert!(matches!(
            document.remove_node("body"),
            Err(UiGraphError::RequiredContainerChild { .. })
        ));
        document
            .replace_node(
                "body",
                UiNode::Button {
                    id: "continue".into(),
                    label: "Continue".into(),
                    accessible_name: None,
                    tooltip: None,
                    on_click_path: None,
                    click_value: None,
                    navigate: None,
                    enabled: true,
                    role: UiButtonRole::Primary,
                },
            )
            .unwrap();
        assert!(document.node("body").is_none());
        assert!(document.node("continue").is_some());
        assert!(matches!(
            document.remove_node("root"),
            Err(UiGraphError::CannotRemoveRoot)
        ));
    }
}

#[test]
fn live_graph_history_is_atomic_bounded_and_invalidated_by_new_edits() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    assert!(!handle.can_undo());
    assert!(!handle.can_redo());

    handle
        .update_node("save", |node| {
            if let UiNode::Button { label, .. } = node {
                *label = "Publish".into();
            }
        })
        .unwrap();
    assert!(handle.can_undo());
    assert!(!handle.can_redo());

    let invalid = handle.update_node("save", |node| {
        if let UiNode::Button { id, .. } = node {
            *id = "root".into();
        }
    });
    assert!(matches!(invalid, Err(UiGraphError::DuplicateId(_))));
    assert!(handle.undo().unwrap());
    assert!(matches!(
        handle.node("save"),
        Ok(UiNode::Button { label, .. }) if label == "Save"
    ));
    assert!(
        !handle.can_undo(),
        "the rejected edit did not enter history"
    );
    assert!(handle.can_redo());

    assert!(handle.redo().unwrap());
    assert!(matches!(
        handle.node("save"),
        Ok(UiNode::Button { label, .. }) if label == "Publish"
    ));
    assert!(handle.undo().unwrap());
    handle
        .update_node("save", |node| {
            if let UiNode::Button { label, .. } = node {
                *label = "Share".into();
            }
        })
        .unwrap();
    assert!(!handle.can_redo(), "a divergent edit starts a new future");

    for index in 0..UI_GRAPH_HISTORY_LIMIT.saturating_add(5) {
        handle
            .update_node("save", |node| {
                if let UiNode::Button { label, .. } = node {
                    *label = format!("Share {index}");
                }
            })
            .unwrap();
    }
    let mut undo_count = 0;
    while handle.undo().unwrap() {
        undo_count += 1;
    }
    assert_eq!(undo_count, UI_GRAPH_HISTORY_LIMIT);
    handle.clear_history().unwrap();
    assert!(!handle.can_undo());
    assert!(!handle.can_redo());
}

#[test]
fn grouped_live_edits_share_one_undo_baseline_until_finished() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    for label in ["Publ", "Publish"] {
        handle
            .update_node_grouped("save", "button-label", |node| {
                if let UiNode::Button { label: current, .. } = node {
                    *current = label.into();
                }
            })
            .unwrap();
    }
    assert!(handle.undo().unwrap());
    assert!(matches!(
        handle.node("save"),
        Ok(UiNode::Button { label, .. }) if label == "Save"
    ));
    assert!(
        !handle.can_undo(),
        "the two keystroke updates formed one step"
    );
    assert!(handle.redo().unwrap());
    assert!(matches!(
        handle.node("save"),
        Ok(UiNode::Button { label, .. }) if label == "Publish"
    ));

    handle.finish_edit_group().unwrap();
    handle
        .update_node_grouped("save", "button-label", |node| {
            if let UiNode::Button { label, .. } = node {
                *label = "Export".into();
            }
        })
        .unwrap();
    assert!(handle.undo().unwrap());
    assert!(matches!(
        handle.node("save"),
        Ok(UiNode::Button { label, .. }) if label == "Publish"
    ));
}

#[test]
fn live_structural_edits_rebuild_and_save_the_same_graph() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    let values = UiValues::default();
    let live = handle
        .widget_with(
            &Theme::dark(),
            UiBuildContext::default().with_values(values.clone()),
        )
        .unwrap();
    let mut driver = crate::headless::Driver::new(crate::State::new(), Box::new(live), (640, 360));
    handle
        .insert_child(
            "root",
            0,
            UiNode::Button {
                id: "new-project".into(),
                label: "New project".into(),
                accessible_name: None,
                tooltip: None,
                on_click_path: Some("new-project".into()),
                click_value: Some("requested".into()),
                navigate: None,
                enabled: true,
                role: UiButtonRole::Primary,
            },
        )
        .unwrap();
    driver.relayout();
    driver
        .perform_named("new-project", &Action::Activate)
        .unwrap();
    assert_eq!(
        values.take_click("new-project").as_deref(),
        Some("requested")
    );

    let removed = handle.remove_node("new-project").unwrap();
    assert_eq!(removed.id(), "new-project");
    driver.relayout();
    assert!(
        driver
            .perform_named("new-project", &Action::Activate)
            .is_err()
    );
    assert!(UiDocument::from_json(&handle.to_json_pretty().unwrap()).is_ok());
}

fn edit_builder_button_properties(driver: &mut crate::headless::Driver) {
    driver
        .perform_named("builder-property", &Action::SetValue("Publish".into()))
        .unwrap();
    driver
        .perform_named(
            "builder-button-accessible-name",
            &Action::SetValue("Publish changes".into()),
        )
        .unwrap();
    driver
        .perform_named(
            "builder-button-tooltip",
            &Action::SetValue("Publish this document".into()),
        )
        .unwrap();
    driver
        .perform_named(
            "builder-button-click-path",
            &Action::SetValue("publish-requested".into()),
        )
        .unwrap();
    driver
        .perform_named(
            "builder-button-click-value",
            &Action::SetValue("yes".into()),
        )
        .unwrap();
    driver
        .perform_named("builder-button-role", &Action::Activate)
        .unwrap();
    driver.relayout();
    driver
        .perform_named("builder-button-navigation", &Action::Activate)
        .unwrap();
    driver.relayout();
    driver
        .perform_named("builder-button-route", &Action::SetValue("settings".into()))
        .unwrap();
    driver
        .perform_named("builder-button-enabled", &Action::Activate)
        .unwrap();
    driver.relayout();
}

#[test]
fn graphical_editor_changes_and_saves_the_live_preview_graph() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    let saved = Rc::new(RefCell::new(None));
    let saved_from_callback = Rc::clone(&saved);
    let editor = UiGraphEditor::new(handle.clone(), Theme::dark())
        .unwrap()
        .on_save(move |source| {
            *saved_from_callback.borrow_mut() = Some(source.to_owned());
            Ok(())
        });
    let mut driver =
        crate::headless::Driver::new(crate::State::new(), Box::new(editor), (1_400, 800));

    let initial = driver.inspection_json().unwrap();
    assert!(initial.contains("builder-add-button"));
    assert!(initial.contains("builder-add-checkbox"));
    assert!(initial.contains("title-input"));

    driver
        .perform_named("builder-add-button", &Action::Activate)
        .unwrap();
    driver.relayout();
    assert!(matches!(
        handle.node("button"),
        Ok(UiNode::Button { label, .. }) if label == "Button"
    ));

    driver
        .perform_named("builder-undo", &Action::Activate)
        .unwrap();
    driver.relayout();
    assert!(handle.node("button").is_err());
    driver
        .perform_named("builder-redo", &Action::Activate)
        .unwrap();
    driver.relayout();
    assert!(handle.node("button").is_ok());
    driver
        .perform_named("builder-node-button", &Action::Activate)
        .unwrap();
    driver.relayout();

    edit_builder_button_properties(&mut driver);
    assert!(matches!(
        handle.node("button"),
        Ok(UiNode::Button {
            label,
            accessible_name: Some(accessible_name),
            tooltip: Some(tooltip),
            on_click_path: Some(on_click_path),
            click_value: Some(click_value),
            navigate: Some(UiNavigation::Push { route }),
            enabled: false,
            role: UiButtonRole::Primary,
            ..
        }) if label == "Publish"
            && accessible_name == "Publish changes"
            && tooltip == "Publish this document"
            && on_click_path == "publish-requested"
            && click_value == "yes"
            && route == "settings"
    ));
    assert!(driver.inspection_json().unwrap().contains("Publish"));

    driver
        .perform_named("builder-save", &Action::Activate)
        .unwrap();
    let saved = saved.borrow().clone().expect("Save callback ran");
    let reopened = UiDocument::from_json(&saved).unwrap();
    assert!(matches!(
        reopened.node("button"),
        Some(UiNode::Button {
            label,
            accessible_name: Some(accessible_name),
            on_click_path: Some(on_click_path),
            navigate: Some(UiNavigation::Push { route }),
            enabled: false,
            role: UiButtonRole::Primary,
            ..
        }) if label == "Publish"
            && accessible_name == "Publish changes"
            && on_click_path == "publish-requested"
            && route == "settings"
    ));
}

#[test]
fn graphical_editor_validates_property_edits_atomically_and_tracks_renamed_ids() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    let editor = UiGraphEditor::new(handle.clone(), Theme::dark()).unwrap();
    let mut driver =
        crate::headless::Driver::new(crate::State::new(), Box::new(editor), (1_400, 800));

    driver
        .perform_named("builder-add-container", &Action::Activate)
        .unwrap();
    driver.relayout();
    let before_invalid_number = handle.to_json_pretty().unwrap();
    driver
        .perform_named("builder-container-width", &Action::SetValue("wide".into()))
        .unwrap();
    assert_eq!(handle.to_json_pretty().unwrap(), before_invalid_number);

    driver
        .perform_named("builder-node-id", &Action::SetValue("panel".into()))
        .unwrap();
    driver.relayout();
    assert!(handle.node("container").is_err());
    assert!(handle.node("panel").is_ok());

    let before_duplicate_id = handle.to_json_pretty().unwrap();
    driver
        .perform_named("builder-node-id", &Action::SetValue("root".into()))
        .unwrap();
    assert_eq!(handle.to_json_pretty().unwrap(), before_duplicate_id);
    assert!(driver.inspection_json().unwrap().contains("panel"));

    driver
        .perform_named("builder-node-root", &Action::Activate)
        .unwrap();
    driver.relayout();
    driver
        .perform_named("builder-add-checkbox", &Action::Activate)
        .unwrap();
    driver.relayout();
    let before_invalid_binding = handle.to_json_pretty().unwrap();
    driver
        .perform_named("builder-check-value-path", &Action::SetValue(String::new()))
        .unwrap();
    assert_eq!(handle.to_json_pretty().unwrap(), before_invalid_binding);
}

#[test]
fn graphical_editor_keeps_renames_live_and_routes_shortcuts_to_local_history() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    let editor = UiGraphEditor::new(handle.clone(), Theme::dark()).unwrap();
    let mut driver =
        crate::headless::Driver::new(crate::State::new(), Box::new(editor), (1_400, 800));

    driver
        .perform_named("builder-add-container", &Action::Activate)
        .unwrap();
    driver.relayout();
    driver
        .perform_named("builder-node-id", &Action::SetValue("panel".into()))
        .unwrap();
    driver.relayout();
    driver
        .perform_named("builder-node-id", &Action::SetValue("panel-final".into()))
        .unwrap();
    driver.relayout();
    assert!(handle.node("container").is_err());
    assert!(handle.node("panel").is_err());
    assert!(handle.node("panel-final").is_ok());

    driver
        .perform_named("builder-node-id", &Action::Focus)
        .unwrap();
    driver.set_modifiers(&["ctrl".to_owned()]);
    driver.key("z");
    assert!(handle.node("container").is_ok());
    assert!(handle.node("panel-final").is_err());

    driver.set_modifiers(&["ctrl".to_owned(), "shift".to_owned()]);
    driver.key("z");
    assert!(handle.node("panel-final").is_ok());
    driver.set_modifiers(&[]);
}

#[test]
fn graphical_editor_exposes_layout_and_text_source_choices() {
    let handle = UiGraphHandle::new(sample()).unwrap();
    let editor = UiGraphEditor::new(handle.clone(), Theme::dark()).unwrap();
    let mut driver =
        crate::headless::Driver::new(crate::State::new(), Box::new(editor), (1_400, 800));

    driver
        .perform_named("builder-column-gap", &Action::SetValue("12.5".into()))
        .unwrap();
    driver
        .perform_named("builder-column-no-padding", &Action::Activate)
        .unwrap();
    driver.relayout();
    assert!(matches!(
        handle.node("root"),
        Ok(UiNode::Column {
            gap: Some(12.5),
            no_padding: true,
            ..
        })
    ));

    driver
        .perform_named("builder-node-title-view", &Action::Activate)
        .unwrap();
    driver.relayout();
    driver
        .perform_named("builder-text-role", &Action::Activate)
        .unwrap();
    driver.relayout();
    driver
        .perform_named("builder-text-source", &Action::Activate)
        .unwrap();
    driver.relayout();
    assert!(matches!(
        handle.node("title-view"),
        Ok(UiNode::Text {
            text: UiTextSource::Static { value },
            role: UiTextRole::Heading,
            ..
        }) if value == "title"
    ));
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Text source: Static")
    );
}
