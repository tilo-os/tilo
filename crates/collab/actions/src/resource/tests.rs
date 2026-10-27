use super::*;
use serde::{Deserialize, Serialize};

/// The rows the person saw are each refused, and the row the listing
/// builds for the same actions reads as an outcome.
#[test]
fn listed_rows_name_the_outcome_never_an_id_a_glyph_or_a_control_verb() {
    let seen = [
        (
            "Feeds \u{2014} Add a subscription \u{b7} 17471151804430418729",
            None,
        ),
        ("Refresh all feeds \u{2014} Focus", None),
        ("History \u{2014} History", None),
        ("Focus", Some("New folder")),
    ];
    for (title, subtitle) in seen {
        let row = ListedRow {
            title: title.to_owned(),
            subtitle: subtitle.map(str::to_owned),
        };
        assert!(!listed_row_problems(&row).is_empty(), "{row:?}");
    }
    // What the action does is its label; what it is described as, for
    // agents, never stands under it.
    assert_eq!(
        listed_row("Add a subscription", None),
        ListedRow {
            title: "Add a subscription".into(),
            subtitle: None,
        }
    );
    let item = listed_row("Unsubscribe", Some("Hacker News"));
    assert_eq!(item.subtitle.as_deref(), Some("Hacker News"));
    assert!(listed_row_problems(&item).is_empty());
    // The resource named like the action adds nothing under it.
    assert_eq!(listed_row("History", Some("History")).subtitle, None);
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenPath {
    path: String,
}

/// Filtering a resource's actions (by the viewer's permissions, say)
/// takes the views they open with them: a view whose open action is not
/// offered fails the action contract, which then refuses the whole
/// surface.
#[test]
fn a_bound_argument_passed_again_is_left_out_unless_it_names_another_value() {
    let spec = ActionSpec::typed_operation(
        "zoom",
        "Zoom",
        "factor",
        OperationKind::View,
        "view.zoom",
        vec![ActionParam::required("factor", ParamType::Number, "Factor")],
        |args| {
            EncodedAction::encode_operation("view.zoom", &args.number("factor"))
                .expect("bound number encodes")
        },
    )
    .with_bound(vec![
        ("pane".into(), serde_json::json!("p1")),
        ("level".into(), serde_json::json!(2)),
    ]);
    let names: Vec<&str> = spec.params().iter().map(|param| &*param.name).collect();
    assert_eq!(names, ["factor"]);
    for supplied in [
        serde_json::json!({"factor": 1.5}),
        serde_json::json!({"factor": 1.5, "pane": "p1", "level": 2}),
        serde_json::json!({"factor": 1.5, "pane": "window/panes/p1"}),
    ] {
        let supplied = supplied.as_object().expect("object");
        assert_eq!(
            spec.build_json(supplied).expect("binds").type_id,
            "view.zoom"
        );
        assert!(spec.validate_json(supplied).is_ok());
    }
    let other = serde_json::json!({"factor": 1.5, "level": 3});
    let refused = spec
        .build_json(other.as_object().expect("object"))
        .expect_err("another value is refused");
    assert_eq!(
        refused.to_string(),
        "argument `level` is `2` here: this action acts on that item only. Leave `level` \
         out, or invoke the action on the item you mean"
    );
    assert!(
        spec.build_json(
            serde_json::json!({"factor": 1.5, "zoom": 1})
                .as_object()
                .unwrap()
        )
        .is_err(),
        "an argument neither published nor bound is still unknown"
    );
}

#[test]
fn retaining_actions_keeps_only_the_views_still_opened() {
    let open = |id: &str| {
        ActionSpec::operation(
            id,
            id,
            OperationKind::View,
            "test.open",
            &OpenPath { path: id.into() },
        )
        .expect("view payload encodes")
    };
    let mut resource = Resource::new("thing", "test.thing", "Thing")
        .with_action(open("reveal"))
        .with_action(open("show"))
        .with_view(ResourceViewSpec::new("revealed", "Revealed", "reveal"))
        .with_view(ResourceViewSpec::new("shown", "Shown", "show"));
    resource.retain_actions(|action| action.id != "reveal");
    assert_eq!(
        resource
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        ["show"]
    );
    assert_eq!(
        resource
            .views
            .iter()
            .map(|view| view.id.as_str())
            .collect::<Vec<_>>(),
        ["shown"],
        "the view opened by the dropped action goes with it"
    );
}

/// A non-document operation needs no fake action implementation, an operation builder cannot
/// switch its declared protocol type, and presentation is bounded before publication.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn resource_operations_keep_their_declared_shape_and_bounds() {
    non_document_operation_needs_no_fake_action_implementation();
    operation_builder_cannot_switch_its_declared_protocol_type();
    resource_presentation_is_bounded_before_publication();
}

fn non_document_operation_needs_no_fake_action_implementation() {
    let spec = ActionSpec::operation(
        "open",
        "Open",
        OperationKind::Effect,
        "files.open",
        &OpenPath {
            path: "/tmp/report".into(),
        },
    )
    .expect("effect payload encodes");
    assert_eq!(spec.kind, OperationKind::Effect);
    assert!(spec.params().is_empty());
}

/// Explicit view builders validate open and mount bindings, implicit or unsafe bindings are
/// rejected, a typed view binds the schema it advertises, and a binding looks up the real spec and
/// validates static arguments.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn resource_views_bind_exactly_the_schema_they_advertise() {
    explicit_resource_view_builders_validate_open_and_mount_bindings();
    resource_view_validation_rejects_implicit_or_unsafe_bindings();
    typed_view_binds_the_same_schema_it_advertises();
    binding_looks_up_real_spec_and_validates_static_arguments();
}

fn explicit_resource_view_builders_validate_open_and_mount_bindings() {
    let open = ActionSpec::typed_operation(
        "open",
        "Open",
        "",
        OperationKind::View,
        "notes.open",
        vec![
            ActionParam::optional("slot_capability", ParamType::Text, "Capability"),
            ActionParam::optional("slot_view", ParamType::Text, "Qualified view"),
        ],
        |_| EncodedAction::encode_operation("notes.open", &()).unwrap(),
    );
    let view = ResourceViewSpec::new("editor", "Editor", "open")
        .with_description("Edit this note")
        .with_primary(true)
        .with_mount(ResourceViewMountSpec::new(
            "open",
            "slot_capability",
            "slot_view",
        ));
    let resource = Resource::new("note:1", "notes.note", "Note")
        .with_action(open)
        .with_view(view.clone());

    assert!(view.validate(&resource).is_empty());
    assert_eq!(resource.views, vec![view]);
}

fn resource_view_validation_rejects_implicit_or_unsafe_bindings() {
    let effect = ActionSpec::typed_operation(
        "open",
        "Open",
        "value",
        OperationKind::Effect,
        "notes.open",
        vec![ActionParam::required("value", ParamType::Text, "Value")],
        |_| EncodedAction::encode_operation("notes.open", &()).unwrap(),
    )
    .with_contract(ActionContract {
        confirmation: ConfirmationPolicy::Confirm,
        ..ActionContract::default()
    });
    let resource = Resource::new("note:1", "notes.note", "Note").with_action(effect);
    let view = ResourceViewSpec::new("editor", "Editor", "open")
        .with_mount(ResourceViewMountSpec::new("open", "missing", "missing"));
    let errors = view.validate(&resource).join("\n");

    assert!(errors.contains("must be a view operation"));
    assert!(errors.contains("must not require confirmation"));
    assert!(errors.contains("must not require caller parameters"));
    assert!(errors.contains("must be distinct"));
    assert!(errors.contains("must name an optional text parameter"));
}

fn typed_view_binds_the_same_schema_it_advertises() {
    let spec = ActionSpec::typed_operation(
        "zoom",
        "Zoom",
        "factor",
        OperationKind::View,
        "view.zoom",
        vec![ActionParam::required_owned(
            "factor",
            ParamType::Number,
            "Zoom factor",
        )],
        |args| {
            EncodedAction::encode_operation("view.zoom", &args.number("factor"))
                .expect("bound number encodes")
        },
    );
    let supplied = serde_json::json!({"factor": 1.25});
    let encoded = spec
        .build_json(supplied.as_object().expect("object"))
        .expect("declared number binds");
    assert_eq!(spec.kind, OperationKind::View);
    assert_eq!(encoded.type_id, "view.zoom");
    assert!(
        spec.build_json(
            serde_json::json!({"factor": "large"})
                .as_object()
                .expect("object")
        )
        .is_err()
    );
}

fn operation_builder_cannot_switch_its_declared_protocol_type() {
    let spec = ActionSpec::typed_operation(
        "run",
        "Run",
        "value",
        OperationKind::Effect,
        "safe.operation",
        vec![ActionParam::required("value", ParamType::Text, "Value")],
        |_| EncodedAction::encode_operation("other.operation", &()).unwrap(),
    );
    let encoded = spec
        .build_json(serde_json::json!({"value":"x"}).as_object().unwrap())
        .unwrap();
    assert_eq!(encoded.type_id, INVALID_ARGUMENTS_TYPE);
}

fn binding_looks_up_real_spec_and_validates_static_arguments() {
    let resource =
        Resource::new("view:1", "view", "View").with_action(ActionSpec::typed_operation(
            "zoom",
            "Zoom",
            "factor",
            OperationKind::View,
            "view.zoom",
            vec![ActionParam::required("factor", ParamType::Number, "Factor")],
            |args| EncodedAction::encode_operation("view.zoom", &args.number("factor")).unwrap(),
        ));
    assert!(ActionBinding::unit_for(&resource, "zoom").is_err());
    let binding = ActionBinding::static_json_for(
        &resource,
        "zoom",
        serde_json::json!({"factor": 1.5})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    let invocation = binding.invoke(()).unwrap();
    assert_eq!(invocation.address, ActionAddress::new("view:1", "zoom"));
    assert_eq!(invocation.arguments["factor"], 1.5);
    assert!(ActionBinding::unit_for(&resource, "missing").is_err());
}

fn resource_presentation_is_bounded_before_publication() {
    let valid = ResourcePresentation {
        detail: Some("19 °C".into()),
        badge: Some("LIVE".into()),
        preview: Some("Sunny with a light breeze".into()),
    };
    assert!(valid.is_valid());
    assert!(
        Resource::new("weather:home", "weather", "Home")
            .with_presentation(valid)
            .presentation
            .is_some()
    );

    let oversized = ResourcePresentation {
        detail: Some("x".repeat(ResourcePresentation::MAX_DETAIL_BYTES + 1)),
        badge: None,
        preview: None,
    };
    assert!(!oversized.is_valid());
    assert!(
        Resource::new("weather:home", "weather", "Home")
            .with_presentation(oversized)
            .presentation
            .is_none()
    );
    assert!(
        !ResourcePresentation {
            detail: None,
            badge: Some("unsafe\nsecond line".into()),
            preview: None,
        }
        .is_valid()
    );
    for preview in [
        "x".repeat(ResourcePresentation::MAX_PREVIEW_BYTES + 1),
        "unsafe\rpreview".into(),
        "unsafe\0preview".into(),
    ] {
        assert!(
            !ResourcePresentation {
                detail: None,
                badge: None,
                preview: Some(preview),
            }
            .is_valid()
        );
    }
    assert!(
        ResourcePresentation {
            detail: None,
            badge: None,
            preview: Some("x".repeat(ResourcePresentation::MAX_PREVIEW_BYTES)),
        }
        .is_valid()
    );
}

#[test]
fn continuation_identity_is_explicit_argument_aware_and_variant_checked() {
    let plain =
        ActionSpec::operation("run", "Run", OperationKind::Effect, "test.run", &()).unwrap();
    assert!(
        plain
            .continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Immediate))
            .is_none()
    );
    let immediate = ActionSpec::immediate_encoded(
        "open",
        "Open",
        ExecutionLocus::ClientLocal,
        EncodedAction::encode_operation("test.open", &()).unwrap(),
    );
    let prompted = ActionSpec::prompted_param(
        "rename",
        "Rename",
        ActionParam::required("value", ParamType::Text, "Name"),
        "name",
        ExecutionLocus::ClientLocal,
        |value| EncodedAction::encode_operation("test.rename", &value).unwrap(),
    );
    let typed = ActionSpec::typed(
        "count",
        "Count",
        "count",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required("count", ParamType::Integer, "Count")],
        |args| EncodedAction::encode_operation("test.count", &args.integer("count")).unwrap(),
    );
    for spec in [&immediate, &prompted, &typed] {
        assert!(
            spec.continuation_identity(ContinuationQuery::Draft)
                .is_none(),
            "every constructor is unsupported unless its provider opts in"
        );
    }

    let spec = plain.with_continuation_identity(|query| match query {
        ContinuationQuery::Exact(ContinuationArguments::Text(value)) => {
            Some(ContinuationEvidence::Exact {
                target: ContinuationKey::new("target", [value.as_bytes()]),
                handler: ContinuationKey::new("handler", [b"v1".as_slice()]),
            })
        }
        ContinuationQuery::Draft => Some(ContinuationEvidence::Draft {
            resolution_domain: ContinuationKey::new("domain", [b"aliases-v4".as_slice()]),
            handler: ContinuationKey::new("handler", [b"v1".as_slice()]),
        }),
        ContinuationQuery::Exact(_) => None,
    });
    assert!(matches!(
        spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Text("a"))),
        Some(ContinuationEvidence::Exact { target, .. })
            if target == ContinuationKey::new("target", [b"a".as_slice()])
    ));
    assert!(matches!(
        spec.continuation_identity(ContinuationQuery::Draft),
        Some(ContinuationEvidence::Draft { .. })
    ));
    assert!(
        spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
            serde_json::json!({PROMPTED_PARAM:"a"}).as_object().unwrap()
        )))
        .is_none(),
        "exact Text and exact JSON remain distinct provider inputs"
    );

    let mismatched = ActionSpec::operation("run", "Run", OperationKind::Effect, "test.run", &())
        .unwrap()
        .with_continuation_identity(|_| {
            Some(ContinuationEvidence::Draft {
                resolution_domain: ContinuationKey::new("domain", [b"v1".as_slice()]),
                handler: ContinuationKey::new("handler", [b"v1".as_slice()]),
            })
        });
    assert!(
        mismatched
            .continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Immediate))
            .is_none()
    );
}

/// A prompted action names its one argument for what it is; every caller
/// holding the typed text finds the name through the action, so renaming
/// it never needs a caller changed.
#[test]
fn a_prompted_action_names_its_argument_and_callers_ask_it_for_the_name() {
    let encode = |title: &str| EncodedAction::encode_operation("test.rename", &title).unwrap();
    let named = ActionSpec::prompted_param(
        "rename",
        "Rename",
        ActionParam::required("title", ParamType::Text, "The note's new title"),
        "note title…",
        ExecutionLocus::ClientLocal,
        encode,
    );
    assert_eq!(named.prompt_param(), Some("title"));
    assert_eq!(named.params()[0].name.as_ref(), "title");
    assert_eq!(
        named.params()[0].description.as_ref(),
        "The note's new title"
    );
    let typed = named.prompt_arguments("Plan").unwrap();
    assert_eq!(
        typed,
        serde_json::json!({"title": "Plan"})
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(named.build_json(&typed).unwrap(), encode("Plan"));
    let ActionInput::Prompted { prompt, .. } = named.input() else {
        panic!("a prompted action prompts");
    };
    assert_eq!(prompt, "The note's new title");
    assert_eq!(named.build_text("Plan"), Some(encode("Plan")));
    // An action that collects no text has no prompt parameter.
    let immediate =
        ActionSpec::operation("run", "Run", OperationKind::Effect, "test.run", &()).unwrap();
    assert_eq!(immediate.prompt_param(), None);
    assert_eq!(immediate.prompt_arguments("x"), None);
}

/// One action type serves every item that offers it: each spec is the
/// type bound to its item, builds for that item on every lane, and a
/// change to one item's action copies the type rather than reaching the
/// others.
#[test]
fn one_action_type_bound_to_many_targets_builds_for_each() {
    fn encode(item: &str, title: &str) -> EncodedAction {
        EncodedAction::encode_operation("notes.retitle", &(item, title)).unwrap()
    }
    let item = |target: &Target| target.get::<String>().cloned().unwrap_or_default();
    let retitle = ActionSpec::typed_operation_on(
        "retitle",
        "Retitle",
        "Title",
        OperationKind::Effect,
        "notes.retitle",
        vec![ActionParam::required("title", ParamType::Text, "The title")],
        move |target, args| encode(&item(target), args.text("title").unwrap_or("")),
    )
    .with_description("Give the note a new title");
    let ty = Arc::clone(retitle.action_type());
    let a = ty.bind(Target::new("a".to_owned()));
    let b = ty.bind(Target::new("b".to_owned()));
    assert!(Arc::ptr_eq(a.action_type(), b.action_type()));
    assert_eq!(a.description.as_deref(), Some("Give the note a new title"));
    assert_eq!(a.prompt_param(), b.prompt_param());
    let typed = a.prompt_arguments("Plan").unwrap();
    assert_eq!(a.build_json(&typed).unwrap(), encode("a", "Plan"));
    assert_eq!(b.build_json(&typed).unwrap(), encode("b", "Plan"));
    assert_eq!(b.build_text("Plan"), Some(encode("b", "Plan")));

    // A nullary type is encoded per target when it fires.
    let open = ActionSpec::typed_operation_on(
        "open",
        "Open",
        "",
        OperationKind::View,
        "notes.open",
        Vec::new(),
        move |target, _| EncodedAction::encode_operation("notes.open", &item(target)).unwrap(),
    );
    let open = Arc::clone(open.action_type());
    assert_eq!(
        open.bind(Target::none()).input(),
        ActionInput::Immediate(None)
    );
    assert_eq!(
        open.bind(Target::new("b".to_owned()))
            .encode_immediate()
            .as_deref(),
        Some(&EncodedAction::encode_operation("notes.open", &"b").unwrap())
    );

    // A deferred type builds for the target it is invoked on.
    let finish = ActionSpec::deferred_on(
        "finish",
        "Finish",
        ExecutionLocus::ClientLocal,
        "notes.finish",
        move |target| EncodedAction::encode_operation("notes.finish", &item(target)).unwrap(),
    );
    let finish = Arc::clone(finish.action_type()).bind(Target::new("a".to_owned()));
    assert_eq!(
        finish.build_deferred(),
        Some(EncodedAction::encode_operation("notes.finish", &"a").unwrap())
    );

    // Changing one item's action leaves the shared type alone.
    let unavailable = b.unavailable("locked", "The note is locked");
    assert!(!Arc::ptr_eq(unavailable.action_type(), &ty));
    assert!(!unavailable.contract.availability.is_available());
    assert!(a.contract.availability.is_available());
    assert_eq!(unavailable.build_json(&typed).unwrap(), encode("b", "Plan"));

    // A target is one allocation: its clones are the same target, an equal value made
    // apart is not.
    let target = Target::new("a".to_owned());
    assert!(target.same(&target.clone()));
    assert!(!target.same(&Target::new("a".to_owned())));
    assert!(Target::none().same(&Target::none()));
    assert!(!target.same(&Target::none()));
}

#[test]
fn the_sole_text_of_an_argument_object_is_found_whatever_it_is_called() {
    let sole = |value: serde_json::Value| {
        sole_text_argument(value.as_object().unwrap()).map(str::to_owned)
    };
    assert_eq!(sole(serde_json::json!({"title": "a"})), Some("a".into()));
    assert_eq!(sole(serde_json::json!({"value": "b"})), Some("b".into()));
    assert_eq!(sole(serde_json::json!({})), None);
    assert_eq!(sole(serde_json::json!({"a": "x", "b": "y"})), None);
    assert_eq!(sole(serde_json::json!({"n": 1})), None);
}

/// The public parts constructor only names an address and carries the
/// caller's arguments. It does not resolve the resource, run the action
/// or mark the request as authorized; the host does all three.
#[test]
fn control_invocation_from_parts_is_an_inert_address_and_arguments() {
    let arguments = serde_json::json!({"title": "Plan"})
        .as_object()
        .unwrap()
        .clone();
    let invocation = ControlInvocation::from_parts("notes.note:1", "edit-title", arguments.clone())
        .expect("a stable address");
    assert_eq!(
        invocation.address,
        ActionAddress::new("notes.note:1", "edit-title")
    );
    assert_eq!(invocation.arguments, arguments);
    assert_eq!(
        ActionAddress::parse(&invocation.address.encode()),
        Some(invocation.address)
    );
    assert!(
        ControlInvocation::from_parts("notes\u{1f}note", "edit-title", arguments.clone()).is_none()
    );
    assert!(ControlInvocation::from_parts("notes.note:1", "edit\u{1f}title", arguments).is_none());
}

#[test]
fn continuation_keys_preserve_tuple_boundaries_and_clone_by_value() {
    let first = ContinuationKey::new("scope", [b"ab".as_slice(), b"c".as_slice()]);
    let second = ContinuationKey::new("scope", [b"a".as_slice(), b"bc".as_slice()]);
    assert_ne!(first, second);
    assert_eq!(first, first.clone());
}

/// An action declares that it opens the file its `path` names, for any
/// file or for the extensions it names; one that only takes a `path`
/// declares nothing.
#[test]
fn a_path_opener_is_declared_not_named() {
    let action = |id: &str| {
        ActionSpec::typed_operation(
            id,
            "Import",
            "",
            OperationKind::Effect,
            "sheets.import",
            vec![ActionParam::optional("path", ParamType::Text, "File")],
            |_| EncodedAction::encode_operation("sheets.import", &()).unwrap(),
        )
    };
    let csv = action("import-csv").opens_path(&["CSV"]);
    assert!(csv.contract.opens_path());
    assert_eq!(
        csv.contract.opened_extensions().collect::<Vec<_>>(),
        ["csv"]
    );
    let any = action("open").opens_path(&[]);
    assert!(any.contract.opens_path());
    assert_eq!(any.contract.opened_extensions().count(), 0);
    let iso = action("import-iso");
    assert!(!iso.contract.opens_path());
}
