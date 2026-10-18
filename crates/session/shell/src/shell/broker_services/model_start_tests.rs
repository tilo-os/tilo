//! Exercise the production person-commit adapters with actual pending tasks.
use super::executor;
#[path = "model_start_tests/gateway.rs"]
mod gateway;

const CHAT_GROUP: u64 = (1 << 48) | 0x01f4;
const CHAT_TAB: u64 = (1 << 48) | 0x01f5;
const OTHER_TAB: u64 = (1 << 48) | 0x01f6;
const CHAT_STRIP: tilo_desktop_model::StripId = tilo_desktop_model::StripId((1 << 48) | 0x01f3);

use super::{NativeFixture, fixture};
use crate::shell::broker_services::NativeOwnerCommitService;
use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
use tilo_action_broker::services::WorkspaceMemberships;
use tilo_action_policy::{ActionKind, TreePath};
use tilo_desktop_model::{WorkspaceAgentRequest, WorkspaceTaskResource, WorkspaceTaskRoute};
use tilo_joined_room::task_admission::model::NativeModelContract;
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};

/// The person a fixture commits as, without its thread-bound environment lock, so
/// a commit built from it can be awaited on any thread.
struct CommitCaller {
    room: crate::shell::state::RoomId,
    certificate: tilo_user_identity::DeviceCert,
}
impl CommitCaller {
    fn of(fixture: &NativeFixture) -> Self {
        Self {
            room: fixture.room,
            certificate: fixture.certificate.clone(),
        }
    }
}

fn request(caller: &CommitCaller, command: WorkspaceTaskCommand) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: caller.room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority: WorkspaceTaskAuthority::Person {
                caller: caller.certificate.clone(),
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
        location: agent_provider_config::EndpointLocation::ThisDevice,
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
    publish_chat_topology(fixture);
    instantiate_canonical_tabs(fixture);
    let invocation = tilo_actions::InvocationId::new();
    WorkspaceTaskResource {
        app_id: Some("so.tilo.chat".into()),
        app_label: None,
        id: "message".into(),
        kind: "message".into(),
        label: "Request".into(),
        agent_request: Some(WorkspaceAgentRequest {
            chat_record: "conversation".into(),
            channel_id: "channel".into(),
            message_id: format!("m_{}", invocation.0),
            instruction: "Review this message".into(),
            invocation,
        }),
    }
}

/// Publish topology through admitted owner ingress, not a cache-only tab.
fn publish_chat_topology(fixture: &NativeFixture) {
    let mut doc = fixture
        .owner
        .with_current(fixture.room, |view| {
            Ok(view.peer().admitted_room().unwrap().clone())
        })
        .unwrap();
    let heads = doc.get_heads();
    let group = WindowGroup {
        id: CHAT_GROUP,
        active_tab: CHAT_TAB,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
        tabs: [CHAT_TAB, OTHER_TAB]
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
    };
    let strip = tilo_desktop_model::Strip {
        id: CHAT_STRIP,
        name: "Chat".into(),
        window_groups: vec![group],
    };
    tilo_desktop_model::projection::project_owned_contents(
        &mut doc,
        &[strip],
        1,
        None,
        tilo_desktop_model::projection::EmptyStrips::Skip,
    )
    .unwrap();
    for (tab, record) in [
        (CHAT_TAB, "conversation"),
        (OTHER_TAB, "other-conversation"),
    ] {
        tilo_desktop_model::apps::bind_tab_record(&mut doc, CHAT_STRIP, CHAT_GROUP, tab, record)
            .unwrap();
    }
    let changes = doc
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    fixture
        .owner
        .receive(
            fixture.room,
            tilo_peer_state::TransportOrigin::RelayDelta(fixture.certificate.device_public_key),
            neighbour_wire::NeighbourMsg::SyncResp {
                room: fixture.room.to_string(),
                changes,
            },
            std::time::SystemTime::now(),
        )
        .unwrap();
}

/// Instantiate only the actual canonical UUIDs projected by the owner.
fn instantiate_canonical_tabs(fixture: &NativeFixture) {
    let tree = crate::shell::native_workspace_owner::test_tree_of(&fixture.owner);
    let mut tree = tree.write();
    let workspace = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|w| w.room == fixture.room)
        .unwrap();
    let strip = workspace
        .strips
        .iter_mut()
        .find(|s| s.id == CHAT_STRIP)
        .unwrap();
    let group = strip
        .window_groups
        .iter_mut()
        .find(|g| g.id == CHAT_GROUP)
        .unwrap();
    for tab in &mut group.tabs {
        tab.tile = TileContent::Foreign {
            app_id: "so.tilo.chat".into(),
            surface_id: tab.id,
        };
    }
    assert_eq!(
        tree.room_tab_record(fixture.room, CHAT_TAB).as_deref(),
        Some("conversation")
    );
    drop(tree);
}

#[test]
fn production_person_start_freezes_pending_task_or_chat_origin_without_widening_attachment() {
    let fixture = fixture();
    let caller = CommitCaller::of(&fixture);
    let (_settings, profile) = settings();
    let executor = executor::start(&fixture);
    let service = executor.service.clone();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let attached = chat_origin(&fixture);
    for resource in [None, Some(attached)] {
        let receipt = runtime
            .block_on(service.execute(request(
                &caller,
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
            &caller,
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
    // The service only borrows the fixture's owner; the fixture owns the broker
    // and the thread-bound environment lock, so it is released last.
    drop(service);
    drop(fixture);
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
    let contract = NativeOwnerCommitService::new(
        crate::shell::native_workspace_owner::test_tree_of(&fixture.owner),
        None,
    )
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
            .find(|window| window.tab == CHAT_TAB)
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
                .resource(CHAT_TAB.to_string())
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
    let caller = CommitCaller::of(fixture);
    runtime
        .block_on(service.execute(request(
            &caller,
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
