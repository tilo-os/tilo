use super::encode_resource_action;
use tilo_actions::{
    ActionAddress, ActionArgs, ActionInput, ActionParam, ActionSpec, DoorError, EncodedAction,
    OperationKind, ParamType, Resource,
};

fn address(action: &str) -> ActionAddress {
    ActionAddress::new("record", action)
}

#[test]
fn absent_arguments_build_optional_typed_defaults_but_not_required_text() {
    let optional = ActionSpec::typed_operation(
        "history",
        "History",
        "Limit",
        OperationKind::View,
        "history",
        vec![ActionParam::optional(
            "limit",
            ParamType::Integer,
            "Maximum entries",
        )],
        |args: &ActionArgs| {
            EncodedAction::encode_operation("history", &args.integer("limit").unwrap_or(50_i64))
                .expect("the bounded integer serializes")
        },
    );
    let prompted = ActionSpec::prompted_operation(
        "rename",
        "Rename",
        "New name",
        "Name",
        OperationKind::View,
        "rename",
        |name| EncodedAction::encode_operation("rename", &name).expect("text serializes"),
    );
    assert!(matches!(&optional.input, ActionInput::Prompted { .. }));
    let resources = vec![
        Resource::new("record", "record", "Record")
            .with_action(optional)
            .with_action(prompted),
    ];

    let encoded = encode_resource_action(&resources, &address("history"), None, None).unwrap();
    assert_eq!(encoded.type_id, "history");
    assert_eq!(serde_json::from_slice::<i64>(&encoded.payload).unwrap(), 50);
    assert_eq!(
        encode_resource_action(&resources, &address("rename"), None, None),
        Err(DoorError::MissingArgument(address("rename")))
    );
}
