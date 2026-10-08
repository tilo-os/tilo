use crate::{ChipDesignShell, ProjectAction};
use ui::framework::testing::TestApp;
use ui::tilo_actions::{Action, ChangeKind, InvocationId};
use ui::tilo_resource::Invoke;

fn binding() -> serde_json::Value {
    serde_json::json!({"name":"test experiment","check_id":"test-check","source":{
        "route":{"room":"test-room","app_id":"so.tilo.code-editor","record_id":"test-file-record"},
        "address":{"resource":"test-file","action":"read-text-snapshot"},
        "resource":{"id":"test-file","kind":"code_editor.file"},
        "semantic_identity":{"kind":"code_editor.file","key":"test-file"}
    },"check":{
        "route":{"room":"test-room","app_id":"so.tilo.git-client","record_id":"test-repo-record"},
        "address":{"resource":"test-repository","action":"run-approved-check"},
        "resource":{"id":"test-repository","kind":"git_client.repo"},
        "semantic_identity":{"kind":"git_client.repo","key":"test-repository"}
    }})
}
fn bind(
    app: &mut TestApp<ChipDesignShell>,
    value: &serde_json::Value,
) -> Result<Option<serde_json::Value>, ui::tilo_actions::DoorError> {
    app.call(
        "bind-experiment",
        serde_json::json!({"json":value.to_string()}),
    )
}

#[test]
fn named_project_links_use_the_public_action_and_undo_without_seeding() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.state().experiments.is_empty());
    bind(&mut app, &binding()).unwrap();
    assert!(app.harness().a11y_node_named("Run experiment").is_some());
    assert!(
        app.harness()
            .a11y_node_named("Open original Code file")
            .is_some()
    );
    assert!(
        app.harness()
            .a11y_node_named("Open original Git repository")
            .is_some()
    );
    let content = app
        .call_on(
            "chip_design.experiment:test experiment",
            "read",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(content["check_id"], "test-check");
    assert!(content["invocations"].as_array().unwrap().is_empty());
    assert!(app.undo());
    assert!(app.state().experiments.is_empty());
}

#[test]
fn binding_rejects_default_records_cross_workspace_and_wrong_semantic_kind() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (path, replacement) in [
        ("/source/route/record_id", ""),
        ("/check/route/room", "other-room"),
        ("/source/semantic_identity/kind", "git_client.repo"),
        ("/check/address/action", "execute-shell"),
    ] {
        let mut value = binding();
        *value.pointer_mut(path).unwrap() = serde_json::Value::String(replacement.into());
        assert!(bind(&mut app, &value).is_err());
        assert!(app.state().experiments.is_empty());
    }
    let mut forged = binding();
    forged["references_truncated"] = serde_json::json!(true);
    assert!(bind(&mut app, &forged).is_err());
    assert!(app.state().experiments.is_empty());
}

#[test]
fn reported_reference_cannot_replace_original_identity_or_authorize_execution() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    bind(&mut app, &binding()).unwrap();
    let invocation = InvocationId::new();
    let original = serde_json::json!({"parent":InvocationId::new(),"invocation":invocation,"action":binding()["check"],"resources":[]});
    let action = ProjectAction::RetainInvocation {
        name: "test experiment".into(),
        json: original.to_string(),
    };
    assert_eq!(action.change_kind(), ChangeKind::External);
    assert_eq!(
        Invoke::<crate::ChipProject>::checked_apply(&action)
            .unwrap()
            .id,
        ui::tilo_resource::APPLY_LOW
    );
    app.invoke(Invoke::checked_apply(&action).unwrap()).unwrap();
    let mut forged = original;
    forged["parent"] = serde_json::to_value(InvocationId::new()).unwrap();
    assert!(
        app.invoke(
            Invoke::checked_apply(&ProjectAction::RetainInvocation {
                name: "test experiment".into(),
                json: forged.to_string()
            })
            .unwrap()
        )
        .is_err()
    );
    assert_eq!(
        app.state().experiments["test experiment"].invocations[0].invocation,
        invocation
    );
    let error = app
        .call_on(
            "chip_design.experiment:test experiment",
            "run-experiment",
            serde_json::json!({}),
        )
        .unwrap_err();
    assert!(
        matches!(error, ui::tilo_actions::DoorError::Failed(ref reason)
        if reason.contains("original invocation custody")),
        "{error:?}"
    );
    assert_eq!(
        app.state().experiments["test experiment"].invocations.len(),
        1
    );
}

#[test]
fn detached_framework_run_refuses_without_issuing_a_child_or_copying_status() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    bind(&mut app, &binding()).unwrap();
    let error = app
        .call(
            "run-experiment",
            serde_json::json!({"name":"test experiment"}),
        )
        .unwrap_err();
    assert!(
        matches!(error, ui::tilo_actions::DoorError::Failed(ref reason)
        if reason.contains("original invocation custody")),
        "{error:?}"
    );
    assert!(
        app.state().experiments["test experiment"]
            .invocations
            .is_empty()
    );
    let original = app.call("read", serde_json::json!({})).unwrap().unwrap();
    assert!(
        original["experiments"]["test experiment"]
            .get("status")
            .is_none()
    );
}

#[test]
fn concurrent_original_references_survive_existing_crdt_sync() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    bind(&mut app, &binding()).unwrap();
    let mut peer = app.peer_document();
    let first = InvocationId::new();
    let second = InvocationId::new();
    let make = |invocation| {
        ProjectAction::RetainInvocation {
        name: "test experiment".into(),
        json: serde_json::json!({"parent":InvocationId::new(),"invocation":invocation,"action":binding()["check"],"resources":[]}).to_string(),
    }
    };
    app.invoke(Invoke::checked_apply(&make(first)).unwrap())
        .unwrap();
    make(second).apply(&mut peer).unwrap();
    app.merge_peer(&mut peer).unwrap();
    let state = app.state();
    let links = &state.experiments["test experiment"].invocations;
    assert_eq!(links.len(), 2);
    assert!(links.iter().any(|link| link.invocation == first));
    assert!(links.iter().any(|link| link.invocation == second));
}

#[test]
fn permission_revocation_blocks_both_binding_and_audited_reference_mutations() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    bind(&mut app, &binding()).unwrap();
    app.set_role(ui::permissions::Role::Viewer);
    let mut other = binding();
    other["name"] = serde_json::json!("another experiment");
    assert!(bind(&mut app, &other).is_err());
    let action = ProjectAction::RetainInvocation {
        name: "test experiment".into(),
        json: serde_json::json!({"parent":InvocationId::new(),"invocation":InvocationId::new(),"action":binding()["check"],"resources":[]}).to_string(),
    };
    assert!(app.invoke(Invoke::checked_apply(&action).unwrap()).is_err());
    assert!(
        app.state().experiments["test experiment"]
            .invocations
            .is_empty()
    );
}

#[test]
fn human_setup_fields_use_shared_collaborative_text_and_refuse_oversized_edits() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.state().experiment_name.is_empty());
    assert!(app.state().experiment_check_id.is_empty());
    app.call(
        "set-experiment-name",
        serde_json::json!({"value":"my experiment"}),
    )
    .unwrap();
    app.call(
        "set-experiment-check",
        serde_json::json!({"value":"existing-check"}),
    )
    .unwrap();
    app.read_document(|doc| {
        use ui::automerge::{ObjType, ROOT, ReadDoc, Value};
        assert!(matches!(
            doc.get(ROOT, "experiment-name").unwrap(),
            Some((Value::Object(ObjType::Text), _))
        ));
        assert!(matches!(
            doc.get(ROOT, "experiment-check").unwrap(),
            Some((Value::Object(ObjType::Text), _))
        ));
    });
    assert!(
        app.call(
            "set-experiment-name",
            serde_json::json!({"value":"x".repeat(257)})
        )
        .is_err()
    );
    assert_eq!(app.state().experiment_name, "my experiment");
    assert!(
        app.harness()
            .a11y_node_named("Choose Code file and Git repository")
            .is_some()
    );
    let error = app.call("link-project", serde_json::json!({})).unwrap_err();
    assert!(
        matches!(error, ui::tilo_actions::DoorError::Failed(ref reason)
        if reason.contains("original invocation custody")),
        "{error:?}"
    );
    assert!(app.state().experiments.is_empty());
}

#[test]
fn removing_project_links_uses_the_same_public_door_and_can_be_undone() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    bind(&mut app, &binding()).unwrap();
    let invocation = InvocationId::new();
    let action = ProjectAction::RetainInvocation {
        name: "test experiment".into(),
        json: serde_json::json!({"parent":InvocationId::new(),"invocation":invocation,"action":binding()["check"],"resources":[]}).to_string(),
    };
    app.invoke(Invoke::checked_apply(&action).unwrap()).unwrap();
    app.call_on(
        "chip_design.experiment:test experiment",
        "remove-experiment",
        serde_json::json!({}),
    )
    .unwrap();
    assert!(app.state().experiments.is_empty());
    assert!(app.undo());
    assert_eq!(
        app.state().experiments["test experiment"].invocations[0].invocation,
        invocation
    );
}

#[test]
fn undoing_binding_with_reported_references_cannot_silently_retarget_them() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    bind(&mut app, &binding()).unwrap();
    app.invoke(Invoke::checked_apply(&ProjectAction::RetainInvocation {
        name: "test experiment".into(),
        json: serde_json::json!({"parent":InvocationId::new(),"invocation":InvocationId::new(),"action":binding()["check"],"resources":[]}).to_string(),
    }).unwrap()).unwrap();
    assert!(app.undo());
    assert!(app.state().experiments.is_empty());
    let mut other = binding();
    other["check"]["semantic_identity"]["key"] = serde_json::json!("other-repository");
    assert!(bind(&mut app, &other).is_err());
    assert!(app.state().experiments.is_empty());
    app.call(
        "remove-experiment",
        serde_json::json!({"name":"test experiment"}),
    )
    .unwrap();
    bind(&mut app, &other).unwrap();
    assert!(
        app.state().experiments["test experiment"]
            .invocations
            .is_empty()
    );
}

#[test]
fn concurrent_reference_boundary_keeps_project_live_and_reports_truncation() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    bind(&mut app, &binding()).unwrap();
    let make = || ProjectAction::RetainInvocation {
        name: "test experiment".into(),
        json: serde_json::json!({
            "parent": InvocationId::new(),
            "invocation": InvocationId::new(),
            "action": binding()["check"],
            "resources": []
        })
        .to_string(),
    };
    for _ in 0..15 {
        app.invoke(Invoke::checked_apply(&make()).unwrap()).unwrap();
    }
    let mut peer = app.peer_document();
    app.invoke(Invoke::checked_apply(&make()).unwrap()).unwrap();
    make().apply(&mut peer).unwrap();
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(
        app.state().experiments["test experiment"].invocations.len(),
        16
    );
    let read = app
        .call_on(
            "chip_design.experiment:test experiment",
            "read",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(read["references_truncated"], true);
    let nodes = app.harness().a11y_nodes();
    let notice = nodes
        .iter()
        .find(|(_, node)| node.label() == Some("Additional reported links"))
        .unwrap();
    assert_eq!(
        notice.1.value(),
        Some("Some reported links exceed display bounds; inspect original Jobs.")
    );
    assert!(app.invoke(Invoke::checked_apply(&make()).unwrap()).is_err());
    assert_eq!(app.state().experiments.len(), 1);
}

#[test]
fn concurrent_reference_bytes_are_bounded_without_deleting_original_links() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    bind(&mut app, &binding()).unwrap();
    let mut peer = app.peer_document();
    let make = || ProjectAction::RetainInvocation {
        name: "test experiment".into(),
        json: serde_json::json!({
            "parent": InvocationId::new(),
            "invocation": InvocationId::new(),
            "action": binding()["check"],
            "resources": vec!["x".repeat(4096); 4]
        })
        .to_string(),
    };
    app.invoke(Invoke::checked_apply(&make()).unwrap()).unwrap();
    make().apply(&mut peer).unwrap();
    app.merge_peer(&mut peer).unwrap();
    let read = app
        .call_on(
            "chip_design.experiment:test experiment",
            "read",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(read["references_truncated"], true);
    assert_eq!(read["invocations"].as_array().unwrap().len(), 1);
    assert!(serde_json::to_vec(&read).unwrap().len() <= crate::project_links::MAX_LINK_BYTES);
    app.read_document(|doc| {
        use ui::automerge::{ROOT, ReadDoc};
        assert_eq!(
            doc.keys(ROOT)
                .filter(|key| key.starts_with("experiment-reference:"))
                .count(),
            2
        );
    });
}

#[test]
fn oversized_synced_reference_is_bounded_before_json_parsing() {
    use ui::automerge::{ROOT, ReadDoc, transaction::Transactable};
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    bind(&mut app, &binding()).unwrap();
    let action = ProjectAction::RetainInvocation {
        name: "test experiment".into(),
        json: serde_json::json!({
            "parent": InvocationId::new(),
            "invocation": InvocationId::new(),
            "action": binding()["check"],
            "resources": []
        })
        .to_string(),
    };
    app.invoke(Invoke::checked_apply(&action).unwrap()).unwrap();
    let mut peer = app.peer_document();
    let key = peer
        .keys(ROOT)
        .find(|key| key.starts_with("experiment-reference:"))
        .unwrap();
    // An oversized untrusted synced scalar is rejected before JSON allocation.
    peer.put(
        ROOT,
        &key,
        "x".repeat(crate::project_links::MAX_LINK_BYTES + 1),
    )
    .unwrap();
    app.merge_peer(&mut peer).unwrap();
    assert!(
        app.state().experiments["test experiment"]
            .invocations
            .is_empty()
    );
    let read = app
        .call_on(
            "chip_design.experiment:test experiment",
            "read",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(read["references_truncated"], true);
    assert!(read["invocations"].as_array().unwrap().is_empty());
    app.read_document(|doc| {
        let (value, _) = doc.get(ROOT, &key).unwrap().unwrap();
        assert_eq!(
            value.as_str().unwrap().len(),
            crate::project_links::MAX_LINK_BYTES + 1
        );
    });
}
