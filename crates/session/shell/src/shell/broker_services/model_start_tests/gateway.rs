//! Production shell selection, source custody, typed admission and HTTP dispatch.
//! Only the delivered lease is trusted fixture plumbing; it derives its binding
//! from the real person-approved run, never from inference request JSON.
use super::*;
use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tilo_action_broker::native_model::{NativeModelHost, NativeModelLease};
use tilo_action_broker::services::{NativeTaskActionRequirement, NativeTaskBinding};
use tilo_model_gateway::service::Gateway;
use tilo_model_gateway::{GatewaySource, NativeGatewaySource, Request};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct DeliveredLease {
    room: String,
    binding: NativeTaskBinding,
}
impl NativeModelLease for DeliveredLease {
    fn with_current(
        &self,
        view: &tilo_joined_room::NativeCommittedView,
        contract: NativeModelContract,
        now: SystemTime,
        enqueue: &mut dyn FnMut(
            tilo_action_broker::native_model::NativeModelContext,
        ) -> Result<(), String>,
    ) -> Result<(), String> {
        enqueue(self.current(view, contract, now)?)
    }
    fn room(&self) -> &str {
        &self.room
    }
    fn binding(&self) -> &NativeTaskBinding {
        &self.binding
    }
    fn live(&self) -> bool {
        true
    }
    fn current(
        &self,
        view: &tilo_joined_room::NativeCommittedView,
        contract: NativeModelContract,
        _now: SystemTime,
    ) -> Result<tilo_action_broker::native_model::NativeModelContext, String> {
        tilo_action_broker::native_model::current_context(view, &self.binding, contract)
    }
}

struct Protected(NativeGatewaySource);
impl tilo_model_gateway::caller::Session for Protected {
    fn caller(&self) -> tilo_model_gateway::caller::Caller {
        tilo_model_gateway::caller::Caller::Agent
    }
    fn active(&self) -> bool {
        true
    }
    fn gateway_source(&self) -> GatewaySource<'_> {
        GatewaySource::Native(&self.0)
    }
}

async fn membership(
    caller: &CommitCaller,
    service: &NativeOwnerCommitService,
    command: WorkspaceMembershipCommand,
) {
    service
        .execute(OwnerCommitRequest {
            room: caller.room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: caller.certificate.clone(),
                command,
            },
        })
        .await
        .unwrap();
}

async fn approved(
    owner: &tilo_joined_room::NativeWorkspaceOwner,
    caller: &CommitCaller,
    service: &NativeOwnerCommitService,
    resource: WorkspaceTaskResource,
) -> Arc<DeliveredLease> {
    service
        .execute(request(
            caller,
            WorkspaceTaskCommand::Queue {
                title: "Review attached resource".into(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
                resource: Some(Box::new(resource.clone())),
            },
        ))
        .await
        .unwrap();
    let pending = owner.workspace_task_snapshot(caller.room).unwrap();
    let task = pending
        .tasks
        .iter()
        .find(|task| task.resource.as_ref().is_some_and(|r| r.id == resource.id))
        .unwrap();
    let task_id = task.id.clone();
    assert!(pending.controls[&task_id].active_run.is_none());
    assert!(pending.controls[&task_id].model_authority.is_none());
    // Permit only the actual MODEL action, not a room-wide outside-effects
    // grant competing with the delegation's Editor cap at the same specificity.
    let model_node = if let Some(origin) = resource.agent_request.as_ref() {
        let tree = crate::shell::native_workspace_owner::test_tree_of(owner);
        crate::shell::host_window_contracts::published(&tree.read())
            .into_iter()
            .find(|window| window.record.as_deref() == Some(origin.chat_record.as_str()))
            .unwrap()
            .request_model
    } else {
        NativeModelContract::task_resource(&caller.room.to_string(), &task_id)
            .action("requestModel")
    };
    // Person approval below still supplies independent frozen model authority.
    membership(
        caller,
        service,
        WorkspaceMembershipCommand::Grant {
            grant: tilo_action_policy::Grant::new(
                "model-source-capability",
                tilo_action_policy::Subject::agent("scribe"),
                model_node,
                tilo_action_policy::KindSet::ALL,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Subject::person(hex::encode(
                    caller.certificate.user_public_key,
                )),
            ),
        },
    )
    .await;
    service
        .execute(request(
            caller,
            WorkspaceTaskCommand::ApproveRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
        ))
        .await
        .unwrap();
    let state = owner.workspace_task_snapshot(caller.room).unwrap();
    let control = &state.controls[&task_id];
    let run = control.active_run.as_ref().unwrap();
    let scope = &control.execution_authority.as_ref().unwrap().scope;
    assert_eq!(
        scope,
        &TreePath::room(caller.room.to_string())
            .app(resource.app_id.as_deref().unwrap())
            .kind(&resource.kind)
            .resource(&resource.id)
    );
    assert!(!scope.covers(&control.model_authority.as_ref().unwrap().request_model));
    Arc::new(DeliveredLease {
        room: caller.room.to_string(),
        binding: NativeTaskBinding {
            task_id,
            agent_id: run.agent_id.clone(),
            epoch: run.epoch,
            revision: run.applied_direction_revision,
            starter: Some(run.approval.as_ref().unwrap().person.clone()),
        },
    })
}

fn inference(invocation: uuid::Uuid, profile: &agent_provider_config::ProviderProfile) -> Request {
    Request::NativePlan {
        invocation: serde_json::json!(invocation),
        profile_id: profile.id.clone(),
        model_id: "selected-model".into(),
        body: serde_json::json!({"model":"selected-model", "messages":[{"role":"user","content":"private draft"}], "max_tokens":16}),
        timeout_ms: 2000,
        stream: false,
        deltas: false,
        trace: None,
    }
}

fn completion(invocation: uuid::Uuid, profile: &agent_provider_config::ProviderProfile) -> Request {
    use agent_provider_config::protocol::{Message, ModelRequest, Role, ToolChoice};
    Request::NativeComplete {
        invocation: serde_json::json!(invocation),
        profile_id: profile.id.clone(),
        model_id: "selected-model".into(),
        request: ModelRequest {
            messages: vec![Message::text(Role::User, "private draft")],
            tools: vec![],
            tool_choice: ToolChoice::None,
            max_output_tokens: 16,
        },
        timeout_ms: 2000,
        stream: false,
    }
}

async fn read_post(socket: &mut tokio::net::TcpStream) -> serde_json::Value {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(socket.read_u8().await.unwrap());
        assert!(head.len() < 65536);
    }
    let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
    assert!(head.starts_with("post "));
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(length < 1024 * 1024);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn no_http(listener: &tokio::net::TcpListener) {
    while let Ok(accepted) =
        tokio::time::timeout(Duration::from_millis(100), listener.accept()).await
    {
        let (mut socket, _) = accepted.unwrap();
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(
            bytes.is_empty(),
            "denied shell MODEL source sent HTTP bytes"
        );
    }
}

#[test]
fn attached_windowless_task_claim_reaches_protected_gateway_without_data_scope_widening() {
    let fixture = fixture();
    let (_initial_settings, mut profile) = settings();
    let executor = executor::start(&fixture);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        profile.endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let _settings = install_profile(&profile);
        let tree = crate::shell::native_workspace_owner::test_tree_of(&fixture.owner);
        assert!(crate::shell::host_window_contracts::published(&tree.read()).is_empty());
        let service = Arc::new(executor.service.clone());
        let lease = approved(&fixture.owner, &CommitCaller::of(&fixture), &service, WorkspaceTaskResource {
            app_id: Some("so.tilo.notes".into()), app_label: None,
            id: "attached".into(), kind: "document".into(), label: "Proposal".into(),
            agent_request: None,
        }).await;
        let contract = service.read_task_model_contract(&lease.room, &lease.binding).unwrap();
        assert_eq!(contract.window, NativeModelContract::task_resource(&lease.room, &lease.binding.task_id));
        // A broad live agent grant cannot bypass the real frozen data ceiling.
        fixture.owner.with_current(fixture.room, |view| {
            let mut invocation = tilo_joined_room::task_policy::NativeTaskInvocation::new(
                uuid::Uuid::new_v4(), "notes", None,
                &tilo_actions::ActionContract { kind: ActionKind::Read, ..Default::default() }, "",
            ).unwrap();
            invocation.private_run_digest = tilo_joined_room::NativeWorkspaceOwner::current_private_run_digest(view, &lease.binding)?;
            let mut requirement = NativeTaskActionRequirement {
                binding: Some(lease.binding.clone()), app: "so.tilo.notes".into(),
                resource_kind: "document".into(), resource: "attached".into(),
                action: "read".into(), need: tilo_actions::ActionNeed::of(ActionKind::Read),
                consequential: false, confirmation: None, invocation: Some(invocation),
            };
            view.check_native_task_preflight(&requirement, "scribe", SystemTime::now())?;
            requirement.resource = "unattached".into();
            let denial = view.check_native_task_preflight(&requirement, "scribe", SystemTime::now())
                .unwrap_err();
            assert_eq!(denial, "native task starter or frozen execution scope changed");
            Ok(())
        }).unwrap();
        let models =
            crate::shell::broker_services::native_model::NativeModels::new(service.clone());
        let invocation = uuid::Uuid::new_v4();
        let dispatch = models.claim(lease, invocation).unwrap();
        drop(models);
        let session = Protected(NativeGatewaySource::new(dispatch).unwrap());
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let body = read_post(&mut socket).await;
            assert_eq!(body["model"], "selected-model");
            assert!(body.to_string().contains("private draft"));
            let reply = r#"{"id":"answer","object":"chat.completion","created":1,"model":"selected-model","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
            listener
        });
        let response = Gateway::new().unwrap().execute(&session, inference(invocation, &profile), &mut |_| true).await;
        assert!(response.ok, "{:?}", response.error);
        let listener = tokio::time::timeout(Duration::from_secs(3), peer).await.unwrap().unwrap();
        no_http(&listener).await;
        drop(service);
    });
}

#[test]
fn prepared_model_uses_desired_frontier_and_fences_stale_final_dispatch() {
    let fixture = fixture();
    let (_settings, profile) = settings();
    let executor = executor::start(&fixture);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let service = Arc::new(executor.service.clone());
        let caller = CommitCaller::of(&fixture);
        let initial = approved(&fixture.owner, &caller, &service, chat_origin(&fixture)).await;
        let steer = |after_revision| {
            request(
                &caller,
                WorkspaceTaskCommand::DirectionForRun {
                    task_id: initial.binding.task_id.clone(),
                    run_epoch: initial.binding.epoch,
                    after_revision,
                    text: format!("Review the next section {after_revision}"),
                },
            )
        };
        service.execute(steer(0)).await.unwrap();
        assert!(
            service
                .read_task_model_contract(&initial.room, &initial.binding)
                .is_err()
        );
        let mut binding = initial.binding.clone();
        binding.revision = 1;
        let lease = Arc::new(DeliveredLease {
            room: initial.room.clone(),
            binding,
        });
        let contract = service
            .read_task_model_contract(&lease.room, &lease.binding)
            .unwrap();
        assert_lease_follows_desired_frontier(&fixture.owner, fixture.room, &lease, &contract);
        let models =
            crate::shell::broker_services::native_model::NativeModels::new(service.clone());
        let attempt = tilo_model_gateway::FrozenNativeAttempt {
            profile: profile.clone(),
            model: "selected-model".into(),
            endpoint: profile.endpoint.clone(),
            body: br#"{"model":"selected-model","messages":[]}"#.to_vec(),
        };
        let invocation = uuid::Uuid::new_v4();
        let dispatch = models.claim(lease.clone(), invocation).unwrap();
        let id = serde_json::json!(invocation);
        dispatch.prepare(&id, &attempt).await.unwrap();
        let mut enqueued = false;
        dispatch
            .dispatch(&id, &attempt, &mut || {
                enqueued = true;
                Ok(())
            })
            .unwrap();
        assert!(enqueued, "MODEL must prepare before DirectionsApplied");
        let state = fixture.owner.workspace_task_snapshot(fixture.room).unwrap();
        assert_eq!(
            state.controls[&lease.binding.task_id]
                .active_run
                .as_ref()
                .unwrap()
                .applied_direction_revision,
            0
        );
        assert_stale_plan_is_fenced(&service, &models, lease, &attempt, &steer).await;
        drop(models);
        drop(service);
    });
}

/// The delivered lease must resolve the desired frontier and refuse any binding
/// whose revision, epoch, agent or starter no longer matches it.
fn assert_lease_follows_desired_frontier(
    owner: &tilo_joined_room::NativeWorkspaceOwner,
    room: crate::shell::state::RoomId,
    lease: &DeliveredLease,
    contract: &NativeModelContract,
) {
    owner
        .with_current(room, |view| {
            let context = tilo_action_broker::native_model::current_context(
                view,
                &lease.binding,
                contract.clone(),
            )?;
            assert_eq!(context.revision, 1);
            assert_ne!(context.private_run_digest, [0; 32]);
            for field in ["revision", "epoch", "agent", "starter"] {
                let mut wrong = lease.binding.clone();
                match field {
                    "revision" => wrong.revision = 0,
                    "epoch" => wrong.epoch = wrong.epoch.checked_add(1).unwrap(),
                    "agent" => wrong.agent_id = "another-agent".into(),
                    _ => wrong.starter = None,
                }
                assert!(
                    tilo_action_broker::native_model::current_context(
                        view,
                        &wrong,
                        contract.clone(),
                    )
                    .is_err(),
                    "{field}"
                );
            }
            Ok(())
        })
        .unwrap();
}

/// A dispatch prepared against the previous plan cannot enqueue after a steer.
async fn assert_stale_plan_is_fenced(
    service: &Arc<NativeOwnerCommitService>,
    models: &crate::shell::broker_services::native_model::NativeModels,
    lease: Arc<DeliveredLease>,
    attempt: &tilo_model_gateway::FrozenNativeAttempt,
    steer: &(impl Fn(u64) -> OwnerCommitRequest + Sync),
) {
    let invocation = uuid::Uuid::new_v4();
    let dispatch = models.claim(lease, invocation).unwrap();
    let id = serde_json::json!(invocation);
    dispatch.prepare(&id, attempt).await.unwrap();
    service.execute(steer(1)).await.unwrap();
    let mut enqueued = false;
    assert!(
        dispatch
            .dispatch(&id, attempt, &mut || {
                enqueued = true;
                Ok(())
            })
            .is_err()
    );
    assert!(
        !enqueued,
        "a frozen lease cannot follow a newer desired plan"
    );
}

#[test]
fn claimed_chat_source_refuses_profile_change_and_same_tab_record_retarget() {
    let fixture = fixture();
    let (_initial_settings, mut profile) = settings();
    let executor = executor::start(&fixture);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        profile.endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let settings = install_profile(&profile);
        let service = Arc::new(executor.service.clone());
        let lease = approved(
            &fixture.owner,
            &CommitCaller::of(&fixture),
            &service,
            chat_origin(&fixture),
        )
        .await;
        let models =
            crate::shell::broker_services::native_model::NativeModels::new(service.clone());
        let invocation = uuid::Uuid::new_v4();
        let session = Protected(
            NativeGatewaySource::new(models.claim(lease.clone(), invocation).unwrap()).unwrap(),
        );
        let mut changed = profile.clone();
        changed.omit_params.push("top_p".into());
        let changed_settings = install_profile(&changed);
        let denial = models
            .claim(lease.clone(), uuid::Uuid::new_v4())
            .err()
            .expect("changed profile must deny claim");
        assert!(denial.contains("selection changed"), "{denial}");
        let response = Gateway::new()
            .unwrap()
            .execute(&session, completion(invocation, &changed), &mut |_| true)
            .await;
        assert!(
            !response.ok,
            "a claim must not retain authority across Settings changes"
        );
        assert_eq!(
            response.model_error.unwrap().kind,
            agent_provider_config::protocol::ErrorKind::PermissionDenied
        );
        no_http(&listener).await;
        drop(changed_settings);
        drop(settings);
        let _restored_settings = install_profile(&profile);
        let invocation = uuid::Uuid::new_v4();
        let session = Protected(
            NativeGatewaySource::new(models.claim(lease.clone(), invocation).unwrap()).unwrap(),
        );
        let tree = crate::shell::native_workspace_owner::test_tree_of(&fixture.owner);
        retarget_cached_record(&tree, fixture.room);
        let denial = models
            .claim(lease, uuid::Uuid::new_v4())
            .err()
            .expect("same tab with a different record must deny claim");
        drop(models);
        assert!(denial.contains("originating window changed"), "{denial}");
        let response = Gateway::new()
            .unwrap()
            .execute(&session, completion(invocation, &profile), &mut |_| true)
            .await;
        assert!(
            !response.ok,
            "retargeted Chat record must deny an already claimed dispatch"
        );
        assert_eq!(
            response.model_error.unwrap().kind,
            agent_provider_config::protocol::ErrorKind::PermissionDenied
        );
        no_http(&listener).await;
        drop(service);
    });
}

/// Canonical routes cannot be rebound. Corrupt only the host's publication cache
/// to prove that even an already claimed dispatch refuses a mismatched live
/// source, rather than inventing canonical state.
fn retarget_cached_record(
    tree: &crate::shell::state::WorkspaceTreeHandle,
    room: crate::shell::state::RoomId,
) {
    let mut tree = tree.write();
    let doc = tree.room_docs.get_mut(&room).unwrap();
    assert!(matches!(
        tilo_desktop_model::apps::bind_tab_record(
            doc,
            CHAT_STRIP,
            CHAT_GROUP,
            CHAT_TAB,
            "retargeted-conversation",
        ),
        Err(tilo_desktop_model::apps::BindRecordError::AlreadyBound)
    ));
    let mut object = automerge::ROOT;
    for key in [
        "contents".to_owned(),
        format!("{:016x}", CHAT_STRIP.raw()),
        "groups".into(),
        format!("{CHAT_GROUP:016x}"),
        "tabs".into(),
        format!("{CHAT_TAB:016x}"),
    ] {
        object = doc.get(&object, key).unwrap().unwrap().1;
    }
    doc.put(&object, "record_id", "retargeted-conversation")
        .unwrap();
    drop(tree);
}
