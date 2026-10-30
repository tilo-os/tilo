//! Directory answers as the wire carries them: typed values in, the same
//! typed values out, and a malformed or out-of-bounds answer refused.

use super::*;
use crate::{SHAPE_ID_BYTES, ShapeId, SurfaceDelta, SurfaceResource, SurfaceTable};
use tilo_actions::{
    ActionContract, ActionKind, ActionParam, ConfirmationPolicy, ExecutionMode, OperationKind,
    ParamType, ResourcePresentation, ResourceViewDescriptor, ResourceViewRangeSpec, RiskLevel,
};

fn contract() -> ActionContract {
    ActionContract {
        kind: ActionKind::Outside,
        risk: RiskLevel::Destructive,
        confirmation: ConfirmationPolicy::Phrase {
            phrase: "erase everything".into(),
        },
        execution: ExecutionMode::Background { cancellable: true },
        tags: vec!["bulk".into(), "irreversible".into()],
        ..ActionContract::default()
    }
}

fn action_row() -> ActionDirectoryRow {
    let contract = contract();
    let descriptor = ExportedAction {
        state: tilo_actions::ActionState::default(),
        resource: "files.folder:inbox".into(),
        semantic_identity: Some(ResourceSemanticIdentity {
            kind: "files.folder".into(),
            key: "source-folder:inbox".into(),
        }),
        action: "erase".into(),
        resource_kind: "files.folder".into(),
        resource_label: "Inbox".into(),
        action_label: "Erase".into(),
        keywords: Vec::new(),
        icon: None,
        description: Some("Erase every file in the folder".into()),
        operation_kind: OperationKind::Effect,
        params: vec![
            ActionParam::required("reason", ParamType::Text, "Why"),
            ActionParam::optional("limit", ParamType::Integer, "At most this many"),
        ],
        contract: contract.clone(),
        view: Some(ResourceViewDescriptor {
            id: "contents".into(),
            label: "Contents".into(),
            description: None,
            open_action: "erase".into(),
            primary: true,
            mount: None,
            range: Some(ResourceViewRangeSpec {
                action: "page".into(),
                start_param: "start".into(),
                end_param: "end".into(),
                unit_param: "unit".into(),
            }),
        }),
        input: ExportedInput::Text {
            prompt: "Why?".into(),
            placeholder: "reason".into(),
            wants_content: false,
        },
    };
    ActionDirectoryRow {
        parent_resource: "files.root".into(),
        resource: descriptor.resource.clone(),
        resource_kind: descriptor.resource_kind.clone(),
        resource_label: descriptor.resource_label.clone(),
        presentation: Some(ResourcePresentation {
            detail: Some("12 files".into()),
            badge: Some("new".into()),
            preview: Some("a.txt\nb.txt".into()),
        }),
        action: Some(descriptor.action.clone()),
        has_children: false,
        visibility: Some(RowVisibility {
            governing_kind: Some(OperationKind::Effect),
            action: descriptor.action.clone(),
            contract,
        }),
        descriptor: Some(descriptor),
    }
}

fn container_row() -> ActionDirectoryRow {
    ActionDirectoryRow {
        parent_resource: String::new(),
        resource: "files.root".into(),
        resource_kind: "files.folder".into(),
        resource_label: "Files".into(),
        presentation: None,
        action: None,
        descriptor: None,
        has_children: true,
        visibility: Some(RowVisibility {
            governing_kind: Some(tilo_actions::OperationKind::View),
            action: "open".into(),
            contract: ActionContract::view(),
        }),
    }
}

fn page(rows: Vec<ActionDirectoryRow>) -> ActionDirectoryPage {
    ActionDirectoryPage {
        revision: "v5:rev".into(),
        rows,
        next_cursor: vec![1, 2, 3],
        exhausted: false,
        stale_cursor: false,
        budget_exhausted: true,
    }
}

/// Send `page` as the wire carries it, let `tamper` edit the sent rows, then
/// read it back as the broker reads an app's answer.
fn round_trip(
    page: &ActionDirectoryPage,
    tamper: impl FnOnce(&mut Vec<serde_json::Value>),
) -> Result<ActionDirectoryPage, String> {
    let mut sent = serde_json::to_value(page).unwrap();
    let rows = sent["rows"].as_array_mut().unwrap();
    tamper(rows);
    read_directory_answer(
        &sent.to_string(),
        u64::MAX / 8,
        |page: &ActionDirectoryPage| page.rows.iter().collect(),
    )
}

#[test]
fn a_directory_row_round_trips_typed() {
    let sent = page(vec![container_row(), action_row()]);
    let received = round_trip(&sent, |_| {}).unwrap();
    assert_eq!(received, sent);
}

#[test]
fn an_exact_resolution_round_trips_typed() {
    for sent in [
        ActionDirectoryResolution::Found {
            row: Box::new(action_row()),
            revision: Some("r9".into()),
        },
        // An owner built before the resolved revision existed names none.
        ActionDirectoryResolution::Found {
            row: Box::new(action_row()),
            revision: None,
        },
        ActionDirectoryResolution::NotFound,
        ActionDirectoryResolution::StaleRevision,
        ActionDirectoryResolution::Unavailable,
    ] {
        let json = serde_json::to_string(&sent).unwrap();
        let received = read_directory_answer(&json, 1 << 20, |resolution| match resolution {
            ActionDirectoryResolution::Found { row, .. } => vec![&**row],
            _ => Vec::new(),
        })
        .unwrap();
        assert_eq!(received, sent);
    }
}

#[test]
fn a_malformed_contract_refuses_the_page() {
    round_trip(&page(vec![action_row()]), |rows| {
        rows[0]["descriptor"]["contract"] = serde_json::json!({"kind": "everyone"});
    })
    .unwrap_err();
}

#[test]
fn a_directory_descriptor_without_a_contract_is_refused() {
    round_trip(&page(vec![action_row()]), |rows| {
        rows[0]["descriptor"]
            .as_object_mut()
            .unwrap()
            .remove("contract");
    })
    .unwrap_err();
}

#[test]
fn an_oversized_directory_contract_is_refused() {
    let mut row = action_row();
    if let Some(descriptor) = &mut row.descriptor {
        descriptor.contract.tags = vec!["x".repeat(MAX_DIRECTORY_CONTRACT_WIRE_BYTES)];
    }
    let error = round_trip(&page(vec![row]), |_| {}).unwrap_err();
    assert!(
        error.contains("`files.folder:inbox`/`erase`") && error.contains("contract"),
        "{error}"
    );
}

#[test]
fn a_malformed_visibility_contract_refuses_the_page() {
    round_trip(&page(vec![container_row()]), |rows| {
        rows[0]["visibility"]["contract"] = serde_json::json!("not json");
    })
    .unwrap_err();
}

#[test]
fn visibility_governing_kind_is_optional_but_never_guessed() {
    let decoded = round_trip(&page(vec![container_row()]), |rows| {
        rows[0]["visibility"]["governing_kind"] = serde_json::Value::Null;
    })
    .unwrap();
    assert_eq!(
        decoded.rows[0].visibility.as_ref().unwrap().governing_kind,
        None
    );
    round_trip(&page(vec![container_row()]), |rows| {
        rows[0]["visibility"]["governing_kind"] = serde_json::json!("public-read");
    })
    .unwrap_err();
}

#[test]
fn a_visibility_action_without_its_contract_refuses_the_page() {
    round_trip(&page(vec![container_row()]), |rows| {
        rows[0]["visibility"]
            .as_object_mut()
            .unwrap()
            .remove("contract");
    })
    .unwrap_err();
    let error = round_trip(&page(vec![container_row()]), |rows| {
        rows[0]["visibility"]["action"] = serde_json::json!("");
    })
    .unwrap_err();
    assert!(error.contains("visibility"), "{error}");
}

#[test]
fn a_malformed_or_invalid_presentation_refuses_the_page() {
    for presentation in [
        serde_json::json!("{\"detail\":"),
        serde_json::json!({"unknown": "field"}),
        serde_json::json!({"detail": "two\nlines"}),
    ] {
        round_trip(&page(vec![action_row()]), |rows| {
            rows[0]["presentation"] = presentation.clone();
        })
        .unwrap_err();
    }
}

#[test]
fn an_unknown_directory_operation_kind_is_refused() {
    for kind in ["", "teleport"] {
        round_trip(&page(vec![action_row()]), |rows| {
            rows[0]["descriptor"]["operation_kind"] = serde_json::json!(kind);
        })
        .unwrap_err();
    }
}

#[test]
fn a_semantic_identity_naming_another_kind_is_refused() {
    let mut row = action_row();
    if let Some(descriptor) = &mut row.descriptor {
        descriptor.semantic_identity = Some(ResourceSemanticIdentity {
            kind: "chat.message".into(),
            key: "m1".into(),
        });
    }
    let error = round_trip(&page(vec![row]), |_| {}).unwrap_err();
    assert!(error.contains("semantic identity"), "{error}");
}

fn rows(page: &ActionDirectoryPage) -> Vec<&ActionDirectoryRow> {
    page.rows.iter().collect()
}

/// The byte budget an app pages its directory by holds whatever the JSON
/// says: rows over it refuse the answer.
#[test]
fn a_page_over_its_byte_budget_is_refused() {
    let sent = page(vec![action_row(), action_row()]);
    let json = serde_json::to_string(&sent).unwrap();
    let size = u64::try_from(action_directory_row_wire_bytes(&action_row()) * 2).unwrap();
    assert_eq!(read_directory_answer(&json, size, rows).unwrap(), sent);
    let error = read_directory_answer(&json, size - 1, rows).unwrap_err();
    assert!(error.contains("budget"), "{error}");
}

#[test]
fn a_surface_delta_round_trips_and_expands_to_the_published_actions() {
    let original = action_row().descriptor.unwrap();
    let mut second = original.clone();
    second.resource = "files.folder:archive".into();
    second.resource_label = "Archive".into();
    second.semantic_identity = Some(ResourceSemanticIdentity {
        kind: "files.folder".into(),
        key: "source-folder:archive".into(),
    });
    let shape = original.shape();
    assert_eq!(shape, second.shape(), "one catalogue entry for both");
    let id: ShapeId = [7; SHAPE_ID_BYTES];
    let template: ShapeId = [8; SHAPE_ID_BYTES];
    let delta = SurfaceDelta {
        reset: true,
        shapes: vec![(id, shape)],
        templates: vec![(template, vec![id])],
        resources: [&original, &second]
            .into_iter()
            .map(|action| SurfaceResource {
                resource: action.resource.clone(),
                fields: action.resource_fields(),
                template,
            })
            .collect(),
        removed: vec!["gone".into()],
    };

    let json = serde_json::to_string(&delta).unwrap();
    let decoded: SurfaceDelta = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, delta);

    let mut table = SurfaceTable::default();
    table.apply(&decoded).unwrap();
    assert_eq!(table.actions(), vec![original, second]);
}

#[test]
fn a_surface_delta_with_a_malformed_shape_id_is_refused() {
    let delta = SurfaceDelta {
        reset: true,
        shapes: vec![(
            [1; SHAPE_ID_BYTES],
            action_row().descriptor.unwrap().shape(),
        )],
        templates: Vec::new(),
        resources: Vec::new(),
        removed: Vec::new(),
    };
    let mut json = serde_json::to_value(&delta).unwrap();
    json["shapes"][0][0] = serde_json::json!([1, 2, 3]);
    assert!(serde_json::from_value::<SurfaceDelta>(json).is_err());
}
