use super::*;

fn test_published_view(action: &str, view_id: &str, primary: bool) -> PublishedAction {
    PublishedAction {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        resource: "note:one".into(),
        action: action.into(),
        resource_kind: "note".into(),
        resource_label: "One".into(),
        action_label: "Open".into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        operation_kind: tilo_actions::OperationKind::View,
        params: vec![
            tilo_actions::ActionParam::optional(
                "slot_capability",
                tilo_actions::ParamType::Text,
                "Mount capability",
            ),
            tilo_actions::ActionParam::optional(
                "slot_view",
                tilo_actions::ParamType::Text,
                "View slot",
            ),
        ],
        contract: tilo_actions::ActionContract::view(),
        view: Some(tilo_actions::ResourceViewDescriptor {
            id: view_id.into(),
            label: "Editor".into(),
            description: None,
            open_action: action.into(),
            desktop: false,
            primary,
            range: None,
            mount: Some(tilo_actions::ResourceViewMountSpec::new(
                action,
                "slot_capability",
                "slot_view",
            )),
        }),
        input: PublishedInput::None,
    }
}

#[test]
fn published_resource_views_validate_bindings_and_resource_cardinality() {
    let editor = test_published_view("open", "editor", true);
    assert!(validate_resource_view_descriptors(std::slice::from_ref(&editor)).is_ok());

    let mut wrong_owner = editor.clone();
    wrong_owner.view.as_mut().unwrap().open_action = "preview".into();
    assert!(validate_resource_view_descriptors(std::slice::from_ref(&wrong_owner)).is_err());

    let mut unsafe_open = editor.clone();
    unsafe_open.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
    assert!(validate_resource_view_descriptors(std::slice::from_ref(&unsafe_open)).is_err());

    let mut invalid_mount = editor.clone();
    invalid_mount
        .view
        .as_mut()
        .unwrap()
        .mount
        .as_mut()
        .unwrap()
        .view_param = "slot_capability".into();
    assert!(validate_resource_view_descriptors(std::slice::from_ref(&invalid_mount)).is_err());

    let mut ranged = editor.clone();
    ranged.view.as_mut().unwrap().range =
        Some(tilo_actions::ResourceViewRangeSpec::new("show-range"));
    let mut target = editor.clone();
    target.action = "show-range".into();
    target.view = None;
    target.params = vec![
        tilo_actions::ActionParam::required("start", tilo_actions::ParamType::Integer, "Start"),
        tilo_actions::ActionParam::required("end", tilo_actions::ParamType::Integer, "End"),
        tilo_actions::ActionParam::required("unit", tilo_actions::ParamType::Text, "Unit"),
    ];
    assert!(validate_resource_view_descriptors(&[ranged.clone(), target.clone()]).is_ok());
    assert!(validate_resource_view_descriptors(std::slice::from_ref(&ranged)).is_err());
    target.operation_kind = tilo_actions::OperationKind::Document;
    assert!(validate_resource_view_descriptors(&[ranged, target]).is_err());

    let reader = test_published_view("read", "reader", true);
    assert!(validate_resource_view_descriptors(&[editor, reader]).is_err());
}

/// One row of a surface published without a label is left out alone: the app's other actions are
/// offered, and the refusal names the row it left out.
#[test]
fn an_unnamed_resource_leaves_out_only_its_own_actions() {
    let named = test_published_view("open", "editor", true);
    let mut unnamed = test_published_view("open", "editor", true);
    unnamed.resource = "conversation:ada".into();
    unnamed.resource_label = String::new();
    let valid = validate_published_surface(vec![named.clone(), unnamed])
        .expect("a surface with one unnamed row is taken");
    assert_eq!(
        valid.actions,
        vec![named],
        "the named resource's action is offered"
    );
    assert_eq!(
        valid.refused.len(),
        1,
        "one row is left out: {:?}",
        valid.refused
    );
    assert!(
        valid.refused[0].starts_with("`conversation:ada#open`")
            && valid.refused[0].contains("non-empty kind and labels"),
        "the refusal names the row and why: {:?}",
        valid.refused
    );
    // An address that breaks the rules still refuses the whole surface.
    let mut twice = test_published_view("open", "editor", true);
    twice.resource_label = "Again".into();
    assert!(
        validate_published_surface(vec![test_published_view("open", "editor", true), twice])
            .is_err(),
        "the same address twice refuses the surface"
    );
}
