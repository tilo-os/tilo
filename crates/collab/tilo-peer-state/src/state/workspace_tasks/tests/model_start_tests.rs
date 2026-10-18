//! Regression coverage for the actual owner reducer entrypoint.
use super::*;
use crate::workspace_task::WorkspaceTaskModelStart;

fn queued(peer: &mut StatePeer, governor: &DeviceCert, now: SystemTime) -> String {
    let (_, result) = peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [81; 32],
            &WorkspaceTaskAuthority::Person {
                caller: governor.clone(),
            },
            &WorkspaceTaskCommand::Queue {
                title: "Review document".into(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
                resource: Some(Box::new(tilo_desktop_model::WorkspaceTaskResource {
                    app_id: Some("notes".into()),
                    app_label: Some("Notes".into()),
                    kind: "document".into(),
                    id: "A".into(),
                    label: "Proposal".into(),
                    agent_request: None,
                })),
            },
            now,
        )
        .unwrap();
    let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = result else {
        panic!("expected queued task")
    };
    task_id
}

fn selected() -> WorkspaceTaskModelStart {
    WorkspaceTaskModelStart {
        request_model: TreePath::room(ROOM)
            .app("desktop")
            .kind("window")
            .resource("actual-window")
            .action("requestModel"),
        kind: ActionKind::Outside,
        contract: serde_json::json!({"provider": "local", "model": "selected", "endpoint": "http://localhost:8000"}),
    }
}

#[test]
fn conscious_start_freezes_model_separately_without_widening_attached_data() {
    let (mut peer, governor, _, now) = peer();
    let task_id = queued(&mut peer, &governor, now);
    let selected = selected();
    peer.workspace_mut()
        .apply_workspace_task_command_with_model_start(
            uuid::Uuid::new_v4(),
            [82; 32],
            &WorkspaceTaskAuthority::Person {
                caller: governor.clone(),
            },
            &WorkspaceTaskCommand::ApproveRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
            now,
            Some(&selected),
        )
        .unwrap();
    let snapshot = crate::workspace_task::snapshot(peer.admitted_room().unwrap()).unwrap();
    let control = &snapshot.controls[&task_id];
    let data = control.execution_authority.as_ref().unwrap();
    let model = control.model_authority.as_ref().unwrap();
    assert_eq!(
        data.scope,
        TreePath::room(ROOM)
            .app("notes")
            .kind("document")
            .resource("A")
    );
    assert!(!data.scope.covers(&selected.request_model));
    assert_eq!(model.request_model, selected.request_model);
    assert_eq!(
        model.contract,
        crate::workspace_task::private_model_contract_reference(&selected.contract).unwrap()
    );
    assert!(!String::from_utf8_lossy(&peer.durable().unwrap()).contains("http://localhost:8000"));
    assert_eq!(model.epoch, control.active_run.as_ref().unwrap().epoch);
    assert_eq!(
        model.starter.person,
        control
            .active_run
            .as_ref()
            .unwrap()
            .approval
            .as_ref()
            .unwrap()
            .person
    );
    assert!(
        !data.scope.covers(
            &TreePath::room(ROOM)
                .app("notes")
                .kind("document")
                .resource("B")
        )
    );
}

#[test]
fn legacy_profile_is_extracted_before_rebuilding_public_history() {
    let (mut peer, governor, _, now) = peer();
    let key = SigningKey::from_bytes(&[2; 32]);
    let task_id = queued(&mut peer, &governor, now);
    let selected = selected();
    peer.workspace_mut()
        .apply_workspace_task_command_with_model_start(
            uuid::Uuid::new_v4(),
            [85; 32],
            &WorkspaceTaskAuthority::Person { caller: governor },
            &WorkspaceTaskCommand::ApproveRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
            now,
            Some(&selected),
        )
        .unwrap();
    // Reproduce the older persistence bug, including a historical change with
    // the complete provider snapshot rather than merely a public DTO.
    let mut controls = crate::workspace_task::controls_of(peer.admitted_room().unwrap()).unwrap();
    controls
        .get_mut(&task_id)
        .unwrap()
        .model_authority
        .as_mut()
        .unwrap()
        .contract = selected.contract.clone();
    crate::workspace_task::project_controls(peer.core.room_doc.as_deref_mut().unwrap(), &controls)
        .unwrap();
    peer.core.room_doc.as_deref_mut().unwrap().commit();
    let (private, baseline) = peer
        .extract_native_task_private_baseline(&key, now, &std::collections::BTreeMap::new())
        .unwrap();
    assert_eq!(
        private[&task_id].model_authority.as_ref().unwrap().contract,
        selected.contract
    );
    let mut public = baseline
        .verify_anchored_document(&baseline.room, baseline.governor, &baseline.anchor())
        .unwrap();
    let snapshot = crate::workspace_task::snapshot(&public).unwrap();
    assert_eq!(
        snapshot.controls[&task_id]
            .model_authority
            .as_ref()
            .unwrap()
            .contract,
        crate::workspace_task::private_model_contract_reference(&selected.contract).unwrap()
    );
    for change in public.get_changes(&[]) {
        assert!(
            !change
                .raw_bytes()
                .windows(b"http://localhost:8000".len())
                .any(|bytes| bytes == b"http://localhost:8000")
        );
    }
}

#[test]
fn host_start_cannot_mint_model_ceiling_or_borrow_creator_approval() {
    let (mut peer, governor, _, now) = peer();
    let task_id = queued(&mut peer, &governor, now);
    let command = WorkspaceTaskCommand::StartRun {
        task_id: task_id.clone(),
        agent_id: "scribe".into(),
    };
    let host = WorkspaceTaskAuthority::Host { attestor: governor };
    let selected = selected();
    assert!(
        peer.workspace_mut()
            .apply_workspace_task_command_with_model_start(
                uuid::Uuid::new_v4(),
                [83; 32],
                &host,
                &command,
                now,
                Some(&selected),
            )
            .is_err()
    );
    peer.workspace_mut()
        .apply_workspace_task_command(uuid::Uuid::new_v4(), [84; 32], &host, &command, now)
        .unwrap();
    let snapshot = crate::workspace_task::snapshot(peer.admitted_room().unwrap()).unwrap();
    assert!(snapshot.controls[&task_id].model_authority.is_none());
}
