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
            sensitivity: tilo_actions::Sensitivity::Public,
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
            sensitivity: tilo_actions::Sensitivity::Public,
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
        descriptor.params[0].schema = Some(json!({"description": "x".repeat(4096)}));
    }
    let browsed = browsed_row_json(&large);
    assert!(browsed["descriptor"]["params"][0].get("schema").is_none());
    assert!(browsed["descriptor"]["params"][0]["schema_omitted"].is_string());
    assert!(
        row_json(&large)["descriptor"]["params"][0]["schema"].is_object(),
        "resolve keeps the full schema"
    );
    let schema = &row_json(&row)["descriptor"]["params"][0]["schema"];
    let operations = schema["properties"]["op"]["oneOf"].as_array().unwrap();
    let rename = operations
        .iter()
        .find(|operation| operation["properties"]["action"]["const"] == "rename")
        .unwrap();
    assert_eq!(rename["description"], "Rename the note.");
    assert_eq!(
        rename["properties"]["args"]["properties"]["title"],
        json!({"type": "string", "description": "New title"})
    );
    assert_eq!(rename["properties"]["args"]["required"], json!(["title"]));
    assert!(
        operations
            .iter()
            .any(|operation| operation["properties"]["action"]["const"] == "pin")
    );
    // What an `apply` invocation carries validates against what the row shows.
    for action in [
        Note::RENAME.with("Groceries".into()).build(0).unwrap(),
        // An absent option is written as `null`.
        tilo_resource::Stamped {
            operation: schema_rows::NoteAction::Pin { pinned: None },
            now_ms: 0,
        },
    ] {
        let invocation = tilo_resource::Invoke::<Note>::apply(&action).unwrap();
        let value = &invocation.arguments[tilo_resource::APPLY_PARAM];
        tilo_actions::validate_json_schema(value, schema).unwrap();
    }
}
