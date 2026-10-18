//! Exercise the production person-commit adapters with actual pending tasks.
#[path = "model_start_tests/gateway.rs"]
mod gateway;

use super::{NativeFixture, fixture};
use crate::shell::broker_services::NativeOwnerCommitService;
use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
use tilo_action_broker::services::WorkspaceMemberships;
use tilo_action_policy::{ActionKind, TreePath};
use tilo_desktop_model::{WorkspaceAgentRequest, WorkspaceTaskResource, WorkspaceTaskRoute};
use tilo_joined_room::task_admission::model::NativeModelContract;
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};

fn request(fixture: &NativeFixture, command: WorkspaceTaskCommand) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: fixture.room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority: WorkspaceTaskAuthority::Person {
                caller: fixture.certificate.clone(),
            },
            command,
        },
    }
}

fn settings() -> (
    crate::shell::test_support::InstalledSettings,
    agent_provider_config::ProviderProfile,
) {
    let profile = agent_provider_config::ProviderProfile {
        id: "local-test".into(),
        label: "Local test".into(),
        endpoint: "http://localhost:8000/v1".into(),
        default_model: Some("selected-model".into()),
        models: vec![agent_provider_config::ModelDescriptor {
            id: "selected-model".into(),
            ..Default::default()
        }],
        omit_params: vec!["temperature".into()],
        ..Default::default()
    };
    (install_profile(&profile), profile)
}

fn install_profile(
    profile: &agent_provider_config::ProviderProfile,
) -> crate::shell::test_support::InstalledSettings {
    let config = agent_provider_config::ProviderConfig {
        profiles: vec![profile.clone()],
        default_profile: Some(profile.id.clone()),
    };
    assert!(config.validation_errors().is_empty());
    let document = serde_json::json!({"agent_providers": {"config": config}})
        .as_object()
        .unwrap()
        .clone();
    let source = settings_store::HostDocument::new(document, |_| {
        Err(std::io::Error::other("read-only test Settings"))
    });
    crate::shell::test_support::install_settings(std::sync::Arc::new(source))
}

fn chat_origin(fixture: &NativeFixture) -> WorkspaceTaskResource {
    let tree = crate::shell::native_workspace_owner::test_tree_of(&fixture.owner);
    let mut tree = tree.write();
    let workspace = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|w| w.room == fixture.room)
        .unwrap();
    let strip = workspace.strips[0].id;
    workspace.strips[0].window_groups.push(WindowGroup {
        id: 500,
        active_tab: 501,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
        tabs: [501, 502]
            .into_iter()
            .map(|id| Tab {
                id,
                title: "Chat".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.chat".into(),
                    surface_id: id,
                },
            })
            .collect(),
    });
    tree.sync_room_doc(fixture.room);
    tilo_desktop_model::apps::bind_tab_record(
        tree.room_docs.get_mut(&fixture.room).unwrap(),
        strip,
        500,
        501,
        "conversation",
    )
    .unwrap();
    tilo_desktop_model::apps::bind_tab_record(
        tree.room_docs.get_mut(&fixture.room).unwrap(),
        strip,
        500,
        502,
        "other-conversation",
    )
    .unwrap();
    WorkspaceTaskResource {
        app_id: Some("so.tilo.chat".into()),
        app_label: None,
        id: "message".into(),
        kind: "message".into(),
        label: "Request".into(),
        agent_request: Some(WorkspaceAgentRequest {
            chat_record: "conversation".into(),
            channel_id: "channel".into(),
            message_id: "message".into(),
            instruction: "Review this message".into(),
            invocation: tilo_actions::InvocationId::new(),
        }),
    }
}

#[test]
fn production_person_start_freezes_pending_task_or_chat_origin_without_widening_attachment() {
    let fixture = fixture();
    let (_settings, profile) = settings();
    let service = NativeOwnerCommitService::new(
        crate::shell::native_workspace_owner::test_tree_of(&fixture.owner),
        None,
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(service.execute(OwnerCommitRequest {
            room: fixture.room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: fixture.certificate.clone(),
                command:
                    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::AddAgent {
                        agent: "scribe".into(),
                        preset: tilo_action_policy::roles::AgentPreset::Operator,
                    },
            },
        }))
        .unwrap();
    let attached = chat_origin(&fixture);
    for resource in [None, Some(attached)] {
        let receipt = runtime
            .block_on(service.execute(request(
                &fixture,
                WorkspaceTaskCommand::Queue {
                    title: "Review request".into(),
                    assigned_agent: "scribe".into(),
                    route: WorkspaceTaskRoute::Model,
                    resource: resource.clone().map(Box::new),
                },
            )))
            .unwrap();
        let pending = fixture.owner.workspace_task_snapshot(fixture.room).unwrap();
        let task_id = pending
            .tasks
            .iter()
            .find(|task| {
                pending
                    .controls
                    .get(&task.id)
                    .is_some_and(|control| control.active_run.is_none())
            })
            .unwrap()
            .id
            .clone();
        assert!(pending.controls[&task_id].model_authority.is_none());
        let approval = request(
            &fixture,
            WorkspaceTaskCommand::ApproveRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
        );
        // Cover both production service entry points, not a direct owner call.
        let committed = if resource.is_none() {
            runtime.block_on(service.execute(approval)).unwrap()
        } else {
            runtime.block_on(service.commit(approval)).unwrap()
        };
        assert!(committed.generation > receipt.generation);
        assert_snapshot(&fixture, &task_id, resource.as_ref(), &profile);
    }
    assert_host_start_has_no_model_approval(&fixture, &service, &runtime);
}

fn assert_snapshot(
    fixture: &NativeFixture,
    task_id: &str,
    resource: Option<&WorkspaceTaskResource>,
    profile: &agent_provider_config::ProviderProfile,
) {
    let state = fixture.owner.workspace_task_snapshot(fixture.room).unwrap();
    let control = &state.controls[task_id];
    let frozen = control
        .model_authority
        .as_ref()
        .expect("actual person approval snapshot");
    let run = control.active_run.as_ref().unwrap();
    let binding = tilo_action_broker::services::NativeTaskBinding {
        task_id: task_id.into(),
        agent_id: run.agent_id.clone(),
        epoch: run.epoch,
        revision: run.applied_direction_revision,
        starter: Some(run.approval.as_ref().unwrap().person.clone()),
    };
    let service = NativeOwnerCommitService::new(
        crate::shell::native_workspace_owner::test_tree_of(&fixture.owner),
        None,
    );
    let contract = service
        .read_task_model_contract(&fixture.room.to_string(), &binding)
        .unwrap();
    assert_eq!(frozen.request_model, contract.request_model);
    assert_eq!(frozen.kind, contract.kind);
    assert_eq!(
        frozen.contract,
        tilo_peer_state::workspace_task::private_model_contract_reference(
            &serde_json::to_value(&contract).unwrap()
        )
        .unwrap()
    );
    assert_eq!(contract.kind, ActionKind::Outside);
    let expected = if resource.is_some() {
        let tree = crate::shell::native_workspace_owner::test_tree_of(&fixture.owner);
        let origin = crate::shell::host_window_contracts::published(&tree.read())
            .into_iter()
            .find(|window| window.tab == 501)
            .unwrap();
        serde_json::json!({"profile": profile, "origin": origin})
    } else {
        serde_json::json!({"profile": profile})
    };
    assert_eq!(contract.selection.contract, expected);
    assert_eq!(contract.selection.model, "selected-model");
    let room = TreePath::room(fixture.room.to_string());
    let scope = if let Some(resource) = resource {
        assert_eq!(
            contract.window,
            room.clone()
                .app("so.tilo.shell")
                .kind("host-window")
                .resource("501")
        );
        room.app("so.tilo.chat")
            .kind(&resource.kind)
            .resource(&resource.id)
    } else {
        assert_eq!(
            contract.window,
            NativeModelContract::task_resource(&fixture.room.to_string(), task_id)
        );
        room
    };
    assert_eq!(control.execution_authority.as_ref().unwrap().scope, scope);
    if resource.is_some() {
        assert!(!scope.covers(&contract.request_model));
        assert!(
            !scope.covers(
                &TreePath::room(fixture.room.to_string())
                    .app("so.tilo.writer")
                    .kind("document")
                    .resource("unattached")
            )
        );
    }
    assert_eq!(
        contract.request_model,
        contract.window.clone().action("requestModel")
    );
}

fn assert_host_start_has_no_model_approval(
    fixture: &NativeFixture,
    service: &NativeOwnerCommitService,
    runtime: &tokio::runtime::Runtime,
) {
    runtime
        .block_on(service.execute(request(
            fixture,
            WorkspaceTaskCommand::Queue {
                title: "Host-started request".into(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
                resource: None,
            },
        )))
        .unwrap();
    let pending = fixture.owner.workspace_task_snapshot(fixture.room).unwrap();
    let task_id = pending
        .tasks
        .iter()
        .find(|task| pending.controls[&task.id].active_run.is_none())
        .unwrap()
        .id
        .clone();
    let attestor = fixture
        .owner
        .with_current(fixture.room, |view| {
            Ok(view.host().unwrap_or_else(|| view.governor()).clone())
        })
        .unwrap();
    runtime
        .block_on(service.execute(OwnerCommitRequest {
            room: fixture.room.to_string(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: uuid::Uuid::new_v4(),
                authority: WorkspaceTaskAuthority::Host { attestor },
                command: WorkspaceTaskCommand::StartRun {
                    task_id: task_id.clone(),
                    agent_id: "scribe".into(),
                },
            },
        }))
        .unwrap();
    let state = fixture.owner.workspace_task_snapshot(fixture.room).unwrap();
    assert!(state.controls[&task_id].active_run.is_some());
    assert!(
        state.controls[&task_id].model_authority.is_none(),
        "host activation must not borrow the creator's approval for model use"
    );
}
