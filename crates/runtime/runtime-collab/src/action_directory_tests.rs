//! The action-directory codec: typed values in, the same typed values out,
//! and a malformed payload refused at decode with its row named.

use super::*;
use crate::collab_capnp;
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
        reversible: false,
        execution: ExecutionMode::Background { cancellable: true },
        tags: vec!["bulk".into(), "irreversible".into()],
        ..ActionContract::default()
    }
}

fn action_row() -> ActionDirectoryRow {
    let contract = contract();
    let descriptor = ExportedAction {
        resource: "files.folder:inbox".into(),
        semantic_identity: Some(ResourceSemanticIdentity {
            kind: "files.folder".into(),
            key: "source-folder:inbox".into(),
        }),
        action: "erase".into(),
        resource_kind: "files.folder".into(),
        resource_label: "Inbox".into(),
        action_label: "Erase".into(),
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

/// Encode `page` with the production writer, let `tamper` edit the encoded
/// rows, then decode it with the production reader.
fn round_trip(
    page: &ActionDirectoryPage,
    tamper: impl FnOnce(capnp::struct_list::Builder<'_, collab_capnp::action_directory_row::Owned>),
) -> Result<ActionDirectoryPage, capnp::Error> {
    let mut message = capnp::message::Builder::new_default();
    {
        let mut root = message.init_root::<collab_capnp::action_directory_page::Builder<'_>>();
        write_action_directory_page(root.reborrow(), page).unwrap();
        tamper(root.get_rows().unwrap());
    }
    let reader = message
        .get_root_as_reader::<collab_capnp::action_directory_page::Reader<'_>>()
        .unwrap();
    decode_action_directory_page(reader)
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
        let mut message = capnp::message::Builder::new_default();
        write_action_directory_resolution(
            message.init_root::<collab_capnp::action_directory_resolution::Builder<'_>>(),
            &sent,
        )
        .unwrap();
        let received = decode_action_directory_resolution(
            message
                .get_root_as_reader::<collab_capnp::action_directory_resolution::Reader<'_>>()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(received, sent);
    }
}

#[test]
fn a_malformed_contract_is_rejected_at_decode_with_the_row_named() {
    let error = round_trip(&page(vec![action_row()]), |mut rows| {
        rows.reborrow()
            .get(0)
            .get_descriptor()
            .unwrap()
            .set_contract_json("{\"kind\":\"everyone\"}");
    })
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("`files.folder:inbox`/`erase`") && message.contains("contract"),
        "{message}"
    );
}

#[test]
fn a_directory_descriptor_without_a_contract_is_refused() {
    let error = round_trip(&page(vec![action_row()]), |mut rows| {
        rows.reborrow()
            .get(0)
            .get_descriptor()
            .unwrap()
            .set_contract_json("");
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("`files.folder:inbox`/`erase`"),
        "{error}"
    );
}

#[test]
fn an_oversized_directory_contract_is_refused() {
    let oversized = format!(
        "{{\"tags\":[\"{}\"]}}",
        "x".repeat(MAX_DIRECTORY_CONTRACT_WIRE_BYTES)
    );
    let error = round_trip(&page(vec![action_row()]), |mut rows| {
        rows.reborrow()
            .get(0)
            .get_descriptor()
            .unwrap()
            .set_contract_json(&oversized);
    })
    .unwrap_err();
    assert!(error.to_string().contains("bounded"), "{error}");
}

#[test]
fn a_malformed_visibility_contract_refuses_the_page() {
    let error = round_trip(&page(vec![container_row()]), |mut rows| {
        rows.reborrow()
            .get(0)
            .set_visibility_contract_json("not json");
    })
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("`files.root`/``") && message.contains("visibility contract"),
        "{message}"
    );
}

#[test]
fn a_visibility_action_without_its_contract_refuses_the_page() {
    let error = round_trip(&page(vec![container_row()]), |mut rows| {
        rows.reborrow().get(0).set_visibility_contract_json("");
    })
    .unwrap_err();
    assert!(error.to_string().contains("set together"), "{error}");
}

#[test]
fn a_malformed_or_invalid_presentation_refuses_the_page() {
    for presentation in [
        "{\"detail\":",
        "{\"unknown\":\"field\"}",
        "{\"detail\":\"two\\nlines\"}",
    ] {
        let error = round_trip(&page(vec![action_row()]), |mut rows| {
            rows.reborrow()
                .get(0)
                .set_resource_presentation_json(presentation);
        })
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("`files.folder:inbox`/`erase`") && message.contains("presentation"),
            "{presentation}: {message}"
        );
    }
}

#[test]
fn an_unknown_directory_operation_kind_is_refused() {
    for kind in ["", "teleport"] {
        let error = round_trip(&page(vec![action_row()]), |mut rows| {
            rows.reborrow()
                .get(0)
                .get_descriptor()
                .unwrap()
                .set_operation_kind(kind);
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("operation kind"),
            "{kind}: {error}"
        );
    }
}

/// `publishActions` from a client that predates contracts and parameters
/// keeps its conservative compatibility defaults.
#[test]
fn an_older_published_action_decodes_with_compatibility_defaults() {
    let mut sent = action_row().descriptor.unwrap();
    sent.semantic_identity = None;
    sent.view = None;
    for (kind, expected_kind, expected_contract) in [
        ("", OperationKind::Document, ActionContract::default()),
        ("view", OperationKind::View, ActionContract::view()),
    ] {
        let mut message = capnp::message::Builder::new_default();
        {
            let mut out = message.init_root::<collab_capnp::exported_action::Builder<'_>>();
            write_exported_action(out.reborrow(), &sent).unwrap();
            out.set_operation_kind(kind);
            out.set_params_json("");
            out.set_contract_json("");
        }
        let decoded = decode_exported_action(
            message
                .get_root_as_reader::<collab_capnp::exported_action::Reader<'_>>()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.operation_kind, expected_kind);
        assert!(decoded.params.is_empty());
        assert_eq!(decoded.contract, expected_contract);
    }
}

#[test]
fn a_published_action_round_trips_typed() {
    let sent = action_row().descriptor.unwrap();
    let mut message = capnp::message::Builder::new_default();
    write_exported_action(
        message.init_root::<collab_capnp::exported_action::Builder<'_>>(),
        &sent,
    )
    .unwrap();
    let received = decode_exported_action(
        message
            .get_root_as_reader::<collab_capnp::exported_action::Reader<'_>>()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(received, sent);
}

#[test]
fn a_semantic_identity_naming_another_kind_is_not_written() {
    let mut action = action_row().descriptor.unwrap();
    action.semantic_identity = Some(ResourceSemanticIdentity {
        kind: "chat.message".into(),
        key: "m1".into(),
    });
    let mut message = capnp::message::Builder::new_default();
    let result = write_exported_action(
        message.init_root::<collab_capnp::exported_action::Builder<'_>>(),
        &action,
    );
    assert!(result.is_err());
}

/// The byte budget an app pages its directory by is checked against the
/// encoded page, so the per-row estimate must never fall below a row's
/// Cap'n Proto size, however its fields are sized.
#[test]
fn directory_row_estimates_bound_their_encoded_size() {
    let text = |length: usize| "x".repeat(length);
    for length in [0, 1, 7, 8, 9, 63, 64, 1000] {
        let mut row = action_row();
        row.parent_resource = text(length);
        row.resource_label = text(length);
        row.presentation = Some(ResourcePresentation {
            detail: Some(text(length.min(ResourcePresentation::MAX_DETAIL_BYTES))),
            badge: None,
            preview: Some(text(length.min(ResourcePresentation::MAX_PREVIEW_BYTES))),
        });
        if let Some(descriptor) = &mut row.descriptor {
            descriptor.resource_label = text(length);
            descriptor.action_label = text(length);
            descriptor.description = Some(text(length));
            descriptor.contract.tags = vec![text(length)];
            descriptor.input = ExportedInput::Text {
                prompt: text(length),
                placeholder: text(length),
                wants_content: false,
            };
        }
        let rows = 40u32;
        let mut message = capnp::message::Builder::new_default();
        {
            let mut page = message.init_root::<collab_capnp::action_directory_page::Builder<'_>>();
            page.set_revision("v5:00000000-0000-0000-0000-000000000000");
            let mut list = page.reborrow().init_rows(rows);
            for index in 0..rows {
                write_action_directory_row(list.reborrow().get(index), &row).unwrap();
            }
        }
        let encoded = message
            .get_root_as_reader::<collab_capnp::action_directory_page::Reader<'_>>()
            .unwrap()
            .total_size()
            .unwrap()
            .word_count
            .saturating_mul(8);
        let estimated =
            action_directory_row_wire_bytes(&row).saturating_mul(usize::try_from(rows).unwrap());
        assert!(
            u64::try_from(estimated).unwrap() >= encoded,
            "field length {length}: estimated {estimated} < encoded {encoded}"
        );
    }
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

    let mut message = capnp::message::Builder::new_default();
    write_surface_delta(
        message
            .init_root::<collab_capnp::channel_handle::publish_action_surface_params::Builder<'_>>(
            ),
        "so.tilo.files",
        &delta,
    )
    .unwrap();
    let reader = message
        .get_root_as_reader::<collab_capnp::channel_handle::publish_action_surface_params::Reader<'_>>()
        .unwrap();
    assert_eq!(
        reader.get_app_id().unwrap().to_str().unwrap(),
        "so.tilo.files"
    );
    let decoded = decode_surface_delta(reader).unwrap();
    assert_eq!(decoded, delta);

    let mut table = SurfaceTable::default();
    table.apply(&decoded).unwrap();
    assert_eq!(table.actions(), vec![original, second]);
}

#[test]
fn a_surface_delta_with_a_malformed_shape_id_is_refused() {
    let mut message = capnp::message::Builder::new_default();
    {
        let mut root = message
            .init_root::<collab_capnp::channel_handle::publish_action_surface_params::Builder<'_>>(
            );
        let mut shapes = root.reborrow().init_shapes(1);
        let mut row = shapes.reborrow().get(0);
        row.set_id(&[1, 2, 3]);
        write_exported_action(row.init_action(), &action_row().descriptor.unwrap().shape())
            .unwrap();
    }
    let reader = message
        .get_root_as_reader::<collab_capnp::channel_handle::publish_action_surface_params::Reader<'_>>()
        .unwrap();
    assert!(decode_surface_delta(reader).is_err());
}
