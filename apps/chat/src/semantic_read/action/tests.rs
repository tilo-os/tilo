use super::*;
use automerge::transaction::Transactable;

#[test]
fn semantic_read_action_continuation_stays_with_its_real_channel() {
    let mut doc = automerge::AutoCommit::new();
    for channel in ["one", "two"] {
        crate::doc::ChatDoc::insert_channel(
            &mut doc,
            channel,
            "ws",
            "cat",
            channel,
            crate::model::ChannelKind::Text,
        );
    }
    for id in ["m1", "m2"] {
        crate::doc::ChatDoc::push_message_with_id(
            &mut doc,
            "one",
            id,
            crate::doc::NewMessage {
                text: id.into(),
                ..Default::default()
            },
        );
    }
    doc.commit();
    let page = super::super::read_messages(
        &doc,
        &doc.base_heads(),
        "one",
        None,
        tilo_doc::semantic_read::Limits {
            items: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let cursor = serde_json::to_string(&page.next.unwrap()).unwrap();
    let args = serde_json::json!({"pageSize":1,"cursor":cursor});
    let actual = resource();
    let encoded = actual.actions[0]
        .build_json(args.as_object().unwrap())
        .unwrap();
    validate_request(&actual, &encoded.payload).unwrap();
    let other = Resource::new("chat.channel:two", "chat.channel", "Other").with_action(spec("two"));
    let encoded = other.actions[0]
        .build_json(args.as_object().unwrap())
        .unwrap();
    assert_eq!(
        validate_request(&other, &encoded.payload).unwrap_err(),
        SemanticReadError::InvalidRequest
    );
}
fn resource() -> Resource {
    Resource::new("chat.channel:one", "chat.channel", "Document").with_action(spec("one"))
}

#[test]
fn semantic_read_action_encodes_requested_capacity_and_closed_resource() {
    let resource = resource();
    for page_size in [1, 32] {
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
    let mut wrong = resource;
    wrong.id = tilo_actions::ResourceId::new("chat.channel:two");
    assert_eq!(
        validate_request(&wrong, br#"{"channel":"one","pageSize":1,"cursor":null}"#).unwrap_err(),
        SemanticReadError::InvalidRequest
    );
}

#[test]
fn semantic_read_action_rejects_ambiguous_or_oversized_request_before_projection() {
    let resource = resource();
    for payload in [
        r#"{"channel":"one","pageSize":0}"#,
        r#"{"channel":"one","pageSize":33}"#,
        r#"{"channel":"one","pageSize":1,"pageSize":2}"#,
        r#"{"channel":"one","pageSize":1,"heads":[]}"#,
        r#"{"channel":"one","pageSize":1,"cursor":"{}"}"#,
        r#"{"channel":"one","pageSize":null}"#,
    ] {
        assert_eq!(
            validate_request(&resource, payload.as_bytes()).unwrap_err(),
            SemanticReadError::InvalidRequest
        );
    }
    let mut oversized = serde_json::json!({"pageSize":1,"cursor":"x".repeat(1025)});
    oversized["channel"] = serde_json::json!("one");
    assert_eq!(
        validate_request(&resource, &serde_json::to_vec(&oversized).unwrap()).unwrap_err(),
        SemanticReadError::Limit
    );
    assert_eq!(
        validate_request(&resource, &vec![b' '; MAX_REQUEST_BYTES + 1]).unwrap_err(),
        SemanticReadError::Limit
    );
    assert!(
        resource.actions[0]
            .build_json(serde_json::json!({"pageSize":0}).as_object().unwrap())
            .is_err()
    );
}

/// Asking for more than a page holds reads a full page (with the cursor to
/// the rest) instead of refusing, so a model asking for 50 still reads.
#[test]
fn semantic_read_action_clamps_a_page_size_over_32_to_32() {
    let resource = resource();
    for page_size in [33, 50, 10_000] {
        let encoded = resource.actions[0]
            .build_json(
                serde_json::json!({"pageSize":page_size})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        validate_request(&resource, &encoded.payload).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(decoded["pageSize"], 32, "{page_size}");
    }
}

#[test]
fn semantic_read_action_encodes_and_json_contract_decodes() {
    let resource = resource();
    assert!(
        resource.actions[0]
            .contract
            .validate_declaration()
            .is_empty()
    );
    let address = tilo_actions::ActionAddress::new("chat.channel:one", "read-messages");
    let encoded = tilo_actions::door::encode_json(
        std::slice::from_ref(&resource),
        &address,
        serde_json::json!({"pageSize":2}).as_object().unwrap(),
    )
    .unwrap();
    assert_eq!(encoded.type_id, OPERATION_TYPE);
    let wire = serde_json::to_value(&resource.actions[0].contract).unwrap();
    assert_eq!(wire["permission"], "read");
    assert_eq!(wire["execution"]["mode"], "immediate");
    assert_eq!(wire["result"]["type"], "json");
    let decoded: ActionContract = serde_json::from_value(wire).unwrap();
    let value = serde_json::json!({"version":1,"record":{"roomId":"77777777-7777-4777-8777-777777777777","appId":"chat","recordId":""},"basis":{"heads":[]},"content":{"channel":"one","messages":[{"id":"m1","author":"Ada","authorId":"u_ada","ts":"-1","parent":"","edited":false,"text":"hello","complete":true}]},"nextCursor":null});
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
}
