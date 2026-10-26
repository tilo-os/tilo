use serde_json::{Value, json};
use tilo_control::{
    client::invoke_wire,
    protocol::{Command, ListingDetail},
};

#[test]
fn semantic_requests_round_trip_through_the_shared_wire() {
    let value = invoke_wire(
        "notes",
        "edit@note",
        &json!({"text": "é🦀"}),
        Some("device"),
        Some(300),
    )
    .unwrap();
    let command: Command = serde_json::from_value(value.clone()).unwrap();
    assert!(
        matches!(&command, Command::ActionsInvoke { provider, id, args, device, wait_ms }
        if provider == "notes" && id == "edit@note" && args["text"] == "é🦀"
        && device.as_deref() == Some("device") && *wait_ms == Some(300))
    );
    assert_eq!(serde_json::to_value(command).unwrap(), value);

    let command: Command = serde_json::from_value(json!({
        "op": "actions_enumerate", "query": "note", "detail": "compact", "limit": 20,
        "cursor": "5.ab", "kinds": ["action"], "files": true, "every_workspace": true,
        "wait_ms": 100, "after_revision": "old"
    }))
    .unwrap();
    assert!(
        matches!(command, Command::ActionsEnumerate { query, page, wait_ms, after_revision, .. }
        if query == "note" && page.detail == ListingDetail::Compact && page.limit == Some(20)
        && page.cursor.as_deref() == Some("5.ab") && page.files && page.every_workspace
        && wait_ms == Some(100) && after_revision.as_deref() == Some("old"))
    );
}

#[test]
fn nullary_compatibility_and_invalid_arguments_have_one_definition() {
    for value in [
        json!({"op":"actions_invoke","provider":"p","id":"a"}),
        json!({"op":"actions_invoke","provider":"p","id":"a","args":null}),
    ] {
        let command: Command = serde_json::from_value(value).unwrap();
        assert!(matches!(command, Command::ActionsInvoke { args, .. } if args.is_empty()));
    }
    for value in [
        json!({"op":"actions_invoke","provider":"","id":"a"}),
        json!({"op":"actions_invoke","provider":"p","id":""}),
        json!({"op":"actions_invoke","provider":"p","id":"a","args":[]}),
        json!({"op":"actions_enumerate","query":42}),
        json!({"op":"actions_status","invocation_id":-1}),
    ] {
        assert!(serde_json::from_value::<Command>(value).is_err());
    }
    assert!(invoke_wire("p", "a", &Value::Null, None, None).is_err());
    assert!(invoke_wire("p", "a", &json!([]), None, None).is_err());
}

#[test]
fn participant_metadata_is_validated_before_endpoint_dispatch() {
    let request = tilo_control::protocol::parse_request::<Value, Value>(
        r#"{"op":"state_dump","agent":"reader","agent_label":"Read\u0000er","window":"notes"}"#,
    )
    .unwrap();
    assert_eq!(request.agent.unwrap().label.as_deref(), Some("Reader"));
    assert_eq!(request.window.as_deref(), Some("notes"));
    for (line, code) in [
        (r#"{"op":"state_dump","agent":""}"#, "invalid_agent"),
        (r#"{"op":"state_dump","window":""}"#, "invalid_window"),
        (
            r#"{"op":"actions_invoke","provider":"p","id":"a","args":[]}"#,
            "parse_error",
        ),
    ] {
        let error = tilo_control::protocol::parse_request::<Value, Value>(line).unwrap_err();
        assert_eq!(error.code, code);
    }
    let command: Command = serde_json::from_value(json!({"op":"state_dump"})).unwrap();
    assert!(command.admin_permitted());
    assert!(!command.agent_permitted());
    let command: Command = serde_json::from_value(json!({"op":"key","key":"Enter"})).unwrap();
    assert!(!command.admin_permitted());
    assert!(command.agent_permitted());
}

#[test]
fn wire_types_preserve_test_payloads_without_renderer_dependencies() {
    let command: Command =
        serde_json::from_value(json!({"op":"gpu_fault","fault":"device_lost"})).unwrap();
    assert!(matches!(command, Command::GpuFault { fault } if fault == "device_lost"));
    let command: Command =
        serde_json::from_value(json!({"op":"swipe","fingers":3,"dx":12,"dy":0})).unwrap();
    assert!(matches!(command, Command::Swipe { release: true, .. }));
}
