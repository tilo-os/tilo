use super::*;
fn resource() -> Resource {
    Resource::new(
        "word-processor.document",
        "word_processor.document",
        "Document",
    )
    .with_action(spec())
}

#[test]
fn semantic_read_action_encodes_requested_capacity_and_closed_resource() {
    let resource = resource();
    for page_size in [1, 64] {
        let arguments = serde_json::json!({"pageSize":page_size});
        let encoded = resource.actions[0]
            .build_json(arguments.as_object().unwrap())
            .unwrap();
        assert_eq!(encoded.type_id, OPERATION_TYPE);
        validate_request(&resource, &encoded.payload).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(decoded["pageSize"], page_size);
        assert!(decoded.get("heads").is_none());
    }
    let mut wrong = resource.clone();
    wrong.id = tilo_actions::ResourceId::new("word-processor.other");
    assert_eq!(
        validate_request(&wrong, br#"{"pageSize":1,"cursor":null}"#).unwrap_err(),
        SemanticReadError::InvalidRequest
    );
}

#[test]
fn semantic_read_action_rejects_ambiguous_or_oversized_request_before_projection() {
    let resource = resource();
    for payload in [
        r#"{"pageSize":0}"#,
        r#"{"pageSize":65}"#,
        r#"{"pageSize":1,"pageSize":2}"#,
        r#"{"pageSize":1,"heads":[]}"#,
        r#"{"pageSize":1,"cursor":"{}"}"#,
        r#"{"pageSize":null}"#,
    ] {
        assert_eq!(
            validate_request(&resource, payload.as_bytes()).unwrap_err(),
            SemanticReadError::InvalidRequest
        );
    }
    let oversized = serde_json::json!({"pageSize":1,"cursor":"x".repeat(1025)});
    assert_eq!(
        validate_request(&resource, &serde_json::to_vec(&oversized).unwrap()).unwrap_err(),
        SemanticReadError::Limit
    );
    assert_eq!(
        validate_request(&resource, &vec![b' '; MAX_REQUEST_BYTES + 1]).unwrap_err(),
        SemanticReadError::Limit
    );
    for page_size in [0, 65] {
        assert!(
            resource.actions[0]
                .build_json(
                    serde_json::json!({"pageSize":page_size})
                        .as_object()
                        .unwrap()
                )
                .is_err()
        );
    }
}

#[test]
fn semantic_read_action_existing_door_refuses_unqualified_read_and_json_contract_decodes() {
    let resource = resource();
    assert!(
        resource.actions[0]
            .contract
            .validate_declaration()
            .is_empty()
    );
    let address = tilo_actions::ActionAddress::new("word-processor.document", "read-text");
    assert!(matches!(
        tilo_actions::door::encode_json(
            &[resource.clone()],
            &address,
            serde_json::json!({"pageSize":2}).as_object().unwrap()
        ),
        Err(tilo_actions::DoorError::Unavailable { .. })
    ));
    let wire = serde_json::to_value(&resource.actions[0].contract).unwrap();
    assert_eq!(wire["permission"], "read");
    assert_eq!(wire["execution"]["mode"], "immediate");
    assert_eq!(wire["result"]["type"], "json");
    let decoded: ActionContract = serde_json::from_value(wire).unwrap();
    let value = serde_json::json!({"version":1,"record":{"roomId":"77777777-7777-4777-8777-777777777777","appId":"word-processor","recordId":""},"basis":{"heads":[]},"content":{"segments":[{"block":"p1","kind":"paragraph","text":"hello","complete":true}]},"nextCursor":null});
    decoded
        .result
        .validate(&tilo_actions::ActionOutput {
            value: Some(value.clone()),
            ..Default::default()
        })
        .unwrap();
    let mut unstamped = value;
    unstamped.as_object_mut().unwrap().remove("basis");
    assert!(
        decoded
            .result
            .validate(&tilo_actions::ActionOutput {
                value: Some(unstamped),
                ..Default::default()
            })
            .is_err()
    );
    // JSON schema validation is inert shape validation, never a private SDK stamp.
}
