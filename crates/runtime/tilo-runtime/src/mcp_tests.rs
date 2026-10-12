use super::*;

#[test]
fn container_rows_preserve_their_canonical_visibility_contract() {
    let contract = tilo_actions::ActionContract::view();
    let row = CatalogueRow {
        provider: "71".into(),
        app_id: "so.tilo.test".into(),
        parent_resource: "root".into(),
        resource: tilo_actions::remote::CatalogueResource {
            id: "aux:document".into(),
            kind: "document".into(),
            label: "A decorative label".into(),
            presentation: None,
        },
        action: None,
        has_children: true,
        visibility: Some(tilo_actions::remote::RowVisibility {
            action: "read".into(),
            contract: contract.clone(),
        }),
    };
    let value = row_json(&row);
    assert_eq!(value["resource"], "aux:document");
    assert_eq!(value["visibility_action"], "read");
    assert_eq!(
        value["visibility_contract"],
        serde_json::to_value(contract).unwrap()
    );
}

#[test]
fn a_resolved_row_says_each_thing_once() {
    let mut row = action_row(
        "doc:1",
        "save-as",
        vec![tilo_actions::ActionParam::required(
            "path",
            tilo_actions::ParamType::Text,
            "Where",
        )],
    );
    row.parent_resource = "doc:1".into();
    row.visibility = Some(tilo_actions::remote::RowVisibility {
        action: "save-as".into(),
        contract: tilo_actions::ActionContract::default(),
    });
    let resolved = resolved_row_json(&row);
    assert_eq!(resolved["resource"], "doc:1");
    assert_eq!(resolved["descriptor"]["params"][0]["name"], "path");
    assert_eq!(
        resolved["descriptor"]["contract"],
        row_json(&row)["descriptor"]["contract"]
    );
    for key in [
        "parent_resource",
        "visibility_action",
        "visibility_contract",
    ] {
        assert!(resolved.get(key).is_none(), "{key}: {resolved}");
    }
    for key in ["resource", "resource_kind", "resource_label"] {
        assert!(
            resolved["descriptor"].get(key).is_none(),
            "{key}: {resolved}"
        );
    }

    // A visibility other than the action's own contract is kept.
    row.visibility = Some(tilo_actions::remote::RowVisibility {
        action: "read".into(),
        contract: tilo_actions::ActionContract::view(),
    });
    assert_eq!(resolved_row_json(&row)["visibility_action"], "read");
}

mod schema_rows {
    use tilo_resource::{Model, resource};

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Note {
        pub title: String,
        pub pinned: bool,
    }

    #[resource(kind = "test.note", label = "Note")]
    impl Note {
        /// Rename the note.
        #[action]
        pub fn rename(&mut self, #[param("New title")] title: String) {
            self.title = title;
        }

        /// Pin or unpin the note.
        #[action]
        pub fn pin(&mut self, pinned: Option<bool>) {
            self.pinned = pinned.unwrap_or(true);
        }
    }
}

#[test]
fn query_rows_carry_the_schema_of_the_generic_apply_argument() {
    use schema_rows::Note;
    let resource = tilo_resource::publish::<Note>("test.note:1", "Note", &Note::default());
    let descriptor = tilo_actions::door::survey(&[resource])
        .into_iter()
        .find(|descriptor| descriptor.address.action == tilo_resource::APPLY)
        .unwrap();
    let row = CatalogueRow {
        provider: "71".into(),
        app_id: "so.tilo.test".into(),
        parent_resource: String::new(),
        resource: tilo_actions::remote::CatalogueResource {
            id: "test.note:1".into(),
            kind: "test.note".into(),
            label: "Note".into(),
            presentation: None,
        },
        action: Some(descriptor),
        has_children: false,
        visibility: None,
    };
    assert_eq!(
        browsed_row_json(&row)["descriptor"]["params"][0]["schema"],
        row_json(&row)["descriptor"]["params"][0]["schema"],
        "a small schema stays in a browsed page"
    );
    let mut large = row.clone();
    if let Some(descriptor) = large.action.as_mut() {
        descriptor.params[0].schema = Some(json!({"description": "x".repeat(4096)}).into());
    }
    let browsed = browsed_row_json(&large);
    assert!(browsed["descriptor"]["params"][0].get("schema").is_none());
    assert!(browsed["descriptor"]["params"][0]["schema_omitted"].is_string());
    assert!(
        row_json(&large)["descriptor"]["params"][0]["schema"].is_object(),
        "resolve keeps the full schema"
    );
    // Operations that restate a named action are named, not repeated: the
    // row of `rename` carries its parameters.
    let schema = &row_json(&row)["descriptor"]["params"][0]["schema"];
    let operations = schema["properties"]["op"]["oneOf"].as_array().unwrap();
    let named = &operations
        .iter()
        .find(|operation| operation["properties"]["action"]["enum"].is_array())
        .unwrap()["properties"]["action"]["enum"];
    assert!(named.as_array().unwrap().contains(&json!("rename")));
    assert!(named.as_array().unwrap().contains(&json!("pin")));
    let rename = tilo_actions::door::survey(&[tilo_resource::publish::<Note>(
        "test.note:1",
        "Note",
        &Note::default(),
    )])
    .into_iter()
    .find(|descriptor| descriptor.address.action == "rename")
    .unwrap();
    assert_eq!(rename.params[0].name, "title");
    // What an `apply` invocation carries validates against what the row shows.
    for action in [
        Note::RENAME.with("Groceries").build(0).unwrap(),
        // An absent option is written as `null`.
        tilo_resource::Stamped {
            operation: schema_rows::NoteAction::Pin(schema_rows::note::Pin { pinned: None }),
            now_ms: 0,
        },
    ] {
        let invocation = tilo_resource::Invoke::<Note>::apply(&action).unwrap();
        let value = &invocation.arguments[tilo_resource::APPLY_PARAM];
        tilo_actions::validate_json_schema(value, schema).unwrap();
    }
}

fn action_row(
    resource: &str,
    action: &str,
    params: Vec<tilo_actions::ActionParam>,
) -> CatalogueRow {
    CatalogueRow {
        provider: "71".into(),
        app_id: "so.tilo.writer".into(),
        parent_resource: String::new(),
        resource: tilo_actions::remote::CatalogueResource {
            id: resource.into(),
            kind: "writer.document".into(),
            label: "Document".into(),
            presentation: None,
        },
        action: Some(tilo_actions::ActionDescriptor {
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(resource, action),
            resource_kind: "writer.document".into(),
            resource_label: "Document".into(),
            action_label: format!("Label of {action}"),
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Document,
            contract: tilo_actions::ActionContract::default(),
            input: tilo_actions::InputSpec::None,
            params,
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

#[test]
fn a_compact_page_names_each_resource_once_and_each_action_by_signature() {
    use tilo_actions::{ActionParam, ParamType};
    let mut save_as = action_row(
        "doc:1",
        "save-as",
        vec![ActionParam::optional(
            "path",
            ParamType::Text,
            "Where to write",
        )],
    );
    if let Some(action) = save_as.action.as_mut() {
        action.contract.risk = tilo_actions::RiskLevel::Destructive;
        action.description =
            Some("Write the document to a file. Formats follow the extension.".into());
    }
    let mut style = action_row(
        "doc:1",
        "set-style",
        vec![
            ActionParam::required("kind", ParamType::Text, "Block kind")
                .with_choices(["heading", "list"]),
            ActionParam::optional("level", ParamType::Integer, "Heading level"),
        ],
    );
    style.has_children = true;
    let mut apply = action_row(
        "doc:1",
        "apply",
        vec![ActionParam::required(
            "action",
            ParamType::Object,
            "An edit",
        )],
    );
    if let Some(action) = apply.action.as_mut() {
        action.params[0].schema = Some(json!({"oneOf": []}).into());
    }
    let other = action_row("doc:2", "save", Vec::new());
    let page = CataloguePage {
        revision: "r1".into(),
        rows: vec![save_as, style, apply, other],
        next_cursor: b"tilo-dir-7".to_vec(),
        ..CataloguePage::default()
    };
    let compact = page::compact_described(&page, Some(&mut page::Handles::default()), true);
    assert_eq!(
        compact,
        json!({
            "revision": "r1",
            "exhausted": false,
            "next_cursor": "tilo-dir-7",
            "resources": [
                {"resource": "doc:1", "kind": "writer.document", "label": "Document", "children": true, "actions": {
                    "save-as": "Label of save-as — Write the document to a file (path?) [destructive]",
                    "set-style": "Label of set-style (kind:heading|list, level?:integer)",
                    "apply": "Label of apply (action:object*)",
                }},
                {"resource": "doc:2", "kind": "writer.document", "label": "Document", "actions": {
                    "save": "Label of save",
                }},
            ],
        })
    );
    assert!(
        compact.to_string().len() * 4 < page_json(&page).to_string().len(),
        "the compact page is a fraction of the full one"
    );
}

/// The rows a needle matched are described; the rest of the resource's
/// actions are bare signatures. A first sentence is never cut short of its
/// end at an ordinary length.
#[test]
fn only_matched_rows_are_described_and_whole() {
    let described = |id: &str, text: &str| {
        let mut row = action_row("doc:1", id, Vec::new());
        if let Some(action) = row.action.as_mut() {
            action.description = Some(text.into());
        }
        row
    };
    let edit = "Change marks by id: colours, opacity, name, move_by [dx,dy], scale and rotate about their centre, or move them to another layer.";
    let page = CataloguePage {
        revision: "r1".into(),
        rows: vec![
            described("edit", edit),
            described("paint", "Paint brush strokes. Each stroke is points."),
        ],
        ..CataloguePage::default()
    };
    let compact = page::compact_first_described(&page, None, 1);
    let actions = &compact["resources"][0]["actions"];
    assert_eq!(
        actions["edit"],
        format!("Label of edit — {}", edit.trim_end_matches('.'))
    );
    assert_eq!(actions["paint"], "Label of paint");
    let long = format!("{}, and then {}", "word ".repeat(40), "more ".repeat(40));
    let page = CataloguePage {
        rows: vec![described("long", &long)],
        ..CataloguePage::default()
    };
    let line = page::compact_described(&page, None, true)["resources"][0]["actions"]["long"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(line.ends_with("word…"), "cut at the clause: {line}");
}

#[test]
fn cursors_travel_as_text_and_come_back_as_the_same_bytes() {
    for cursor in [
        b"tilo-dir-4325025d".to_vec(),
        7u64.to_le_bytes().to_vec(),
        b"b64:looks-encoded".to_vec(),
        b"has space".to_vec(),
        Vec::new(),
    ] {
        let text = page::cursor_text(&cursor);
        assert_eq!(page::cursor_bytes(&text).unwrap(), cursor, "{text}");
    }
    assert_eq!(page::cursor_text(b"tilo-dir-1"), "tilo-dir-1");
    assert!(page::cursor_bytes("b64:!!").is_err());
    let args: QueryArgs =
        parse(json!({"workspace": true, "cursor": [7, 0, 0, 0, 0, 0, 0, 0]})).unwrap();
    assert_eq!(args.cursor.into_bytes().unwrap(), 7u64.to_le_bytes());
    let args: QueryArgs = parse(json!({"workspace": true, "cursor": "b64:BwAAAAAAAAA"})).unwrap();
    assert_eq!(args.cursor.into_bytes().unwrap(), 7u64.to_le_bytes());
}

#[test]
fn every_word_of_a_needle_begins_a_word_of_the_row() {
    let save_as = action_row("doc:1", "save-as", Vec::new());
    let save = action_row("doc:1", "save", Vec::new());
    let words = page::needle_words("Save  AS");
    assert_eq!(words, ["save", "as"]);
    assert!(page::matches_words(&save_as, &words));
    assert!(
        !page::matches_words(&save, &words),
        "\"as\" is not inside \"Label\""
    );
    assert!(page::matches_words(
        &save,
        &page::needle_words("writer save")
    ));
    assert!(page::matches_words(
        &save,
        &page::needle_words("document, sav")
    ));
    let format = action_row(
        "sheet:1",
        "format-cells",
        vec![tilo_actions::ActionParam::optional(
            "bold",
            tilo_actions::ParamType::Boolean,
            "Heavier text",
        )],
    );
    assert!(page::matches_words(
        &format,
        &page::needle_words("bold cells")
    ));
    assert!(page::matches_words(
        &format,
        &page::needle_words("heavier text")
    ));
}

#[test]
fn a_running_app_is_named_by_its_id_or_its_last_part() {
    let running = [
        "so.tilo.canvas",
        "so.tilo.word-processor",
        "org.example.canvas",
    ];
    assert_eq!(
        page::app_named("so.tilo.word-processor", &running).unwrap(),
        "so.tilo.word-processor"
    );
    assert_eq!(
        page::app_named("Word-Processor", &running).unwrap(),
        "so.tilo.word-processor"
    );
    let both = page::app_named("canvas", &running).unwrap_err();
    assert!(both.contains("several"), "{both}");
    let none = page::app_named("sheets", &running[..1]).unwrap_err();
    assert!(none.contains("running: so.tilo.canvas"), "{none}");
}

#[test]
fn a_succeeded_batch_step_is_its_output_alone() {
    let step = succeeded_step(json!({
        "invocation_id": "a",
        "update": {"state": "succeeded", "invocation": "a", "output": {
            "value": {"content": "tilo.content:ab"}, "resources": ["tilo.content:ab"]
        }},
        "unchanged": "note"
    }));
    assert_eq!(
        step,
        json!({"value": {"content": "tilo.content:ab"}, "unchanged": "note"})
    );
    let empty = succeeded_step(
        json!({"invocation_id": "b", "update": {"state": "succeeded", "invocation": "b", "output": {}}}),
    );
    assert_eq!(empty, json!({}));
}
