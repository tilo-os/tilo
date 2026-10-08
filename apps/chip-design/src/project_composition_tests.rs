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
    assert!(
        app.call_on(
            "chip_design.experiment:test experiment",
            "run-experiment",
            serde_json::json!({"name":"test experiment"})
        )
        .is_err()
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
    assert!(
        app.call(
            "run-experiment",
            serde_json::json!({"name":"test experiment"})
        )
        .is_err()
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
