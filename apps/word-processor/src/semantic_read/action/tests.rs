use super::*;
fn resource() -> Resource {
    Resource::new("word-processor.document", "tilo.rich-text", "Document").with_action(spec())
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
    let mut wrong = resource;
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
fn semantic_read_action_encodes_and_json_contract_decodes() {
    let resource = resource();
    assert!(
        resource.actions[0]
            .contract
            .validate_declaration()
            .is_empty()
    );
    let address = tilo_actions::ActionAddress::new("word-processor.document", "read-text");
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
}

#[test]
fn the_registered_projection_pages_body_text_at_the_given_heads() {
    let mut source = tilo_doc::Document::new();
    let first = source.insert_block(None, "heading");
    source.splice_text(&first, 0, 0, "Q3 Planning");
    let second = source.insert_block(Some(&first), "paragraph");
    source.splice_text(&second, 0, 0, "Book the venue");
    let mut doc = automerge::AutoCommit::load(&source.save()).unwrap();
    let heads = doc.get_heads();
    let page = project(&doc, &heads, br#"{"pageSize":1,"cursor":null}"#).unwrap();
    assert_eq!(page.content["segments"][0]["text"], "Q3 Planning");
    let cursor = page.next_cursor.expect("a second page");
    let request = serde_json::json!({"pageSize":8,"cursor":cursor});
    let rest = project(&doc, &heads, &serde_json::to_vec(&request).unwrap()).unwrap();
    assert_eq!(rest.content["segments"][0]["text"], "Book the venue");
    assert!(rest.next_cursor.is_none());

    automerge::transaction::Transactable::put(&mut doc, automerge::ROOT, "touched", true).unwrap();
    doc.commit();
    let later = doc.get_heads();
    assert_eq!(
        project(&doc, &later, &serde_json::to_vec(&request).unwrap()).err(),
        Some(SemanticReadError::StaleCursor),
        "a cursor from an earlier basis is refused"
    );
}
