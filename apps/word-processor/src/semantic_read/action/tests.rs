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
    let page = project(
        &doc,
        &heads,
        br#"{"pageSize":1,"cursor":null,"format":"segments"}"#,
    )
    .unwrap();
    assert_eq!(page.content["segments"][0]["text"], "Q3 Planning");
    let cursor = page.next_cursor.expect("a second page");
    let request = serde_json::json!({"pageSize":8,"cursor":cursor,"format":"segments"});
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

#[test]
fn read_text_shows_bold_italic_code_and_links_as_markdown() {
    let mut source = tilo_doc::Document::new();
    let memo = source.insert_block(None, "paragraph");
    source.splice_text(&memo, 0, 0, "Deadline: October 15, run make, see docs");
    source.add_mark(&memo, 0, 20, "strong", true);
    source.add_mark(&memo, 10, 20, "em", true);
    source.add_mark(&memo, 26, 30, "fontFamily", "mono");
    source.add_mark(&memo, 36, 40, "link", "https://tilo.so");
    source.add_mark(&memo, 36, 40, "underline", true);
    let plain = source.insert_block(Some(&memo), "paragraph");
    source.splice_text(&plain, 0, 0, "unmarked");
    source.add_mark(&plain, 0, 8, "strong", true);
    source.add_mark(&plain, 0, 8, "strong", automerge::ScalarValue::Null);
    let mut doc = automerge::AutoCommit::load(&source.save()).unwrap();
    let heads = doc.get_heads();
    let page = project(&doc, &heads, br#"{"pageSize":64,"cursor":null}"#).unwrap();
    assert_eq!(
        page.content["markdown"],
        format!(
            "**Deadline: *October 15***, run `make`, see [docs](https://tilo.so) {{#{memo}}}\nunmarked {{#{plain}}}\n"
        )
    );
}

#[test]
fn read_text_defaults_to_one_markdown_page_of_up_to_64_blocks() {
    let resource = resource();
    let encoded = resource.actions[0]
        .build_json(serde_json::json!({}).as_object().unwrap())
        .unwrap();
    validate_request(&resource, &encoded.payload).unwrap();
    let decoded: serde_json::Value = serde_json::from_slice(&encoded.payload).unwrap();
    assert_eq!(decoded["pageSize"], 64);
    assert_eq!(decoded["format"], "markdown");

    let mut source = tilo_doc::Document::new();
    let heading = source.insert_block(None, "heading");
    source.splice_text(&heading, 0, 0, "Standup");
    source.as_mut().set_attr(&heading, "level", 2_i64);
    let mut previous = heading.clone();
    let mut cells = Vec::new();
    for (row, col, text) in [
        (0, 0, "Person"),
        (0, 1, "Blocked"),
        (1, 0, "Maya"),
        (1, 1, "no"),
    ] {
        let cell = source.insert_block(Some(&previous), "cell");
        source.splice_text(&cell, 0, 0, text);
        source.as_mut().set_attr(&cell, "table", "t1");
        source.as_mut().set_attr(&cell, "row", i64::from(row));
        source.as_mut().set_attr(&cell, "col", i64::from(col));
        cells.push(cell.clone());
        previous = cell;
    }
    let mut doc = automerge::AutoCommit::load(&source.save()).unwrap();
    let heads = doc.get_heads();
    let page = project(&doc, &heads, &encoded.payload).unwrap();
    assert_eq!(
        page.content["markdown"],
        format!(
            "## Standup {{#{heading}}}\n| Person {{#{}}} | Blocked {{#{}}} | {{#table t1}}\n| --- | --- |\n| Maya {{#{}}} | no {{#{}}} |\n",
            cells[0], cells[1], cells[2], cells[3]
        )
    );
    assert!(page.next_cursor.is_none());
    let segments = project(
        &doc,
        &heads,
        br#"{"pageSize":64,"cursor":null,"format":"segments"}"#,
    )
    .unwrap()
    .content;
    assert_eq!(segments["segments"][0]["level"], 2);
    assert_eq!(segments["segments"][4]["block"], cells[3].as_str());
    assert_eq!(segments["segments"][4]["row"], 1);
    assert_eq!(segments["segments"][4]["table"], "t1");
    for content in [page.content, segments] {
        let value = serde_json::json!({"version":1,"record":{"roomId":null,"appId":"word-processor","recordId":""},"basis":{"heads":[]},"content":content,"nextCursor":null});
        resource.actions[0]
            .contract
            .result
            .validate(&tilo_actions::ActionOutput {
                value: Some(value),
                ..Default::default()
            })
            .unwrap();
    }
}
