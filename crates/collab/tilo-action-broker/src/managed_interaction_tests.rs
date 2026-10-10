#[derive(Clone)]
struct ManagedTaskOwner(Arc<Mutex<crate::services::WorkspaceTaskSnapshot>>);

impl crate::services::WorkspaceTasks for ManagedTaskOwner {
    fn requires_room(&self, room: &str) -> bool {
        self.0.lock().unwrap().room == room
    }

    fn snapshot(&self, room: &str) -> Result<crate::services::WorkspaceTaskSnapshot, String> {
        let snapshot = self.0.lock().unwrap().clone();
        (snapshot.room == room)
            .then_some(snapshot)
            .ok_or_else(|| "wrong managed task room".into())
    }

    fn host_certificate(&self, _room: &str) -> Result<tilo_user_identity::DeviceCert, String> {
        Err("read-only managed task owner has no host certificate".into())
    }

    fn execute(
        &self,
        _request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
        Box::pin(async { Err("read-only managed task owner cannot mutate".into()) })
    }
}

struct ManagedActionFixture {
    fanout: Arc<FanoutState>,
    state: Arc<CapnpBrokerState>,
    membership: WorkspaceTreeHandle,
    directories: Rc<LocalActionDirectoryRegistry>,
    route: CrdtRoute,
    admitted: AdmittedJoinIdentity,
    delivered: mpsc::UnboundedReceiver<InvocationEnvelope>,
    owner: Arc<Mutex<crate::services::WorkspaceTaskSnapshot>>,
}

fn managed_action_fixture() -> ManagedActionFixture {
    use collab_types::{CapOverride, Scope, Subject, Verb};

    let root = SigningKey::from_bytes(&[109; 32]);
    let principal = principal_id_for_root(root.verifying_key().to_bytes());
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId(109),
        "Managed interaction",
    );
    workspace.set_owner(principal);
    let room = workspace.room.to_string();
    workspace.grant_capability(CapOverride::membership(
        Subject::agent("tide"),
        room.clone(),
        Verb::Write,
    ));
    workspace.grant_capability(CapOverride::grant(
        Subject::agent("tide"),
        Scope::App("so.tilo.chat".into()),
        Verb::Write,
    ));
    let source_route = CrdtRoute::record("so.tilo.agent", &room, "");
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let mut admitted = admit_verified_join(&tree, &source_route, verified_device(&root)).unwrap();
    admit_agent_delegation(&tree, &source_route, &mut admitted, "tide").unwrap();

    let membership = tree.shared();
    let fanout = Arc::new(FanoutState::new());
    let task_id = "task:00000000-0000-4000-8000-000000000109";
    let owner = Arc::new(Mutex::new(crate::services::WorkspaceTaskSnapshot {
        room: room.clone(),
        generation: 1,
        tasks: vec![tilo_desktop_model::WorkspaceTask {
            id: task_id.into(),
            title: "Lifecycle task".into(),
            created_by: "person".into(),
            assigned_agent: Some("tide".into()),
            route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            status: tilo_desktop_model::WorkspaceTaskStatus::InProgress,
            resource: None,
            created_at: 1,
            updated_at: 1,
            audit: Vec::new(),
        }],
        controls: std::collections::BTreeMap::from([(
            task_id.into(),
            tilo_desktop_model::WorkspaceTaskControl {
                run_epoch: 3,
                instruction: Some("private instruction".into()),
                instruction_required: true,
                active_run: Some(tilo_desktop_model::WorkspaceTaskActiveRun {
                    epoch: 3,
                    agent_id: "tide".into(),
                    started_at: 1,
                    applied_direction_revision: 0,
                    approval: None,
                }),
                ..Default::default()
            },
        )]),
    }));
    let (state, bridge_rx) = CapnpBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(membership.authority()),
        None,
        None,
        HostServices {
            workspace_tasks: Some(Arc::new(ManagedTaskOwner(owner.clone()))),
            ..Default::default()
        },
    );
    fanout.attach_capnp(state.clone());
    let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
    let (tx, rx) = mpsc::unbounded_channel();
    state.invocation_subscribers.lock().unwrap().insert(41, tx);
    state.surfaces.lock().unwrap().insert(
        41,
        PublishedSurface {
            peer_id: 41,
            app_id: "so.tilo.chat".into(),
            route: CrdtRoute::record("so.tilo.chat", &room, ""),
            actions: vec![PublishedAction {
                resource: "chat.person:tide".into(),
                semantic_identity: None,
                action: "open-agent-conversation".into(),
                resource_kind: "chat.agent".into(),
                resource_label: "Tide".into(),
                action_label: "Open conversation".into(),
                description: None,
                operation_kind: tilo_actions::OperationKind::View,
                params: vec![],
                contract: tilo_actions::ActionContract::view(),
                view: None,
                input: PublishedInput::None,
            }],
            live: true,
        },
    );
    state.pending_task_runs.lock().unwrap().insert(
        7,
        PendingTaskRun {
            peer_id: 17,
            agent_id: "tide".into(),
            cluster_id: room,
            task_id: task_id.into(),
            lease: "lease-7".into(),
            action_context: tilo_actions::ActionContext::for_resource(
                "Lifecycle task",
                "desktop:task:00000000-0000-4000-8000-000000000109",
                "desktop.task",
                "Lifecycle task",
            ),
            task_epoch: Some(3),
            terminal: false,
            native: None,
        },
    );
    ManagedActionFixture {
        fanout,
        state,
        membership,
        directories,
        route: source_route,
        admitted,
        delivered: rx,
        owner,
    }
}

async fn observe_managed_action(
    client: &channel_handle::Client,
    invocation: tilo_actions::InvocationId,
) -> Option<tilo_actions::remote::InvocationState> {
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};

    let frame = RemoteActionFrame::new(RemoteActionMessage::observe(invocation))
        .encode()
        .unwrap();
    let mut request = client.remote_action_request();
    request.get().set_frame(&frame);
    let response = request.send().promise.await.unwrap();
    let reply = RemoteActionFrame::decode(response.get().unwrap().get_frame().unwrap()).unwrap();
    let RemoteActionMessage::Update {
        invocation: answered,
        state,
    } = reply.message
    else {
        return None;
    };
    assert_eq!(answered, invocation);
    state
}

fn managed_action_client(
    state: &Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    route: &CrdtRoute,
    admitted: AdmittedJoinIdentity,
) -> channel_handle::Client {
    capnp_rpc::new_client(CapnpChannelHandle {
        committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
        state: state.clone(),
        directories,
        channel_id: route.channel_key(),
        peer_id: 17,
        verified_identity: admitted,
        record_kind: None,
        lease: None,
    })
}

struct ManagedActionRequest {
    run_id: u64,
    lease: &'static str,
    argument: Option<&'static str>,
    invocation: String,
    step: Option<u32>,
}

fn managed_action_request(invocation: impl Into<String>) -> ManagedActionRequest {
    ManagedActionRequest {
        run_id: 7,
        lease: "lease-7",
        argument: Some("private model argument"),
        invocation: invocation.into(),
        step: None,
    }
}

async fn invoke_managed_action(
    client: &channel_handle::Client,
    request: &ManagedActionRequest,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    let mut call = client.invoke_task_action_request();
    let mut params = call.get();
    params.set_run_id(request.run_id);
    params.set_lease(request.lease);
    params.set_peer_id(41);
    params.set_resource("chat.person:tide");
    params.set_action("open-agent-conversation");
    params.set_argument(request.argument.unwrap_or(""));
    params.set_has_argument(request.argument.is_some());
    params.set_confirmation_json("");
    params.set_invocation(&request.invocation);
    if let Some(step) = request.step {
        params.set_step_index(step);
        params.set_has_step(true);
    }
    let response = call.send().promise.await?;
    let response = response.get()?;
    let update = serde_json::from_str(response.get_update_json()?.to_str()?)
        .map_err(|_| capnp::Error::failed("invalid task action update".into()))?;
    Ok((response.get_id(), update))
}

fn sanitized_failed_interaction(payload: PresencePayload) -> Option<tilo_actions::InvocationId> {
    let PresencePayload::Interaction(failed) = payload else {
        return None;
    };
    assert!(matches!(
        failed.phase,
        tilo_actions::InteractionPhase::Failed {
            code: Some(ref code),
            ref message,
        } if code == "action_failed" && message == "Action failed"
    ));
    let public = serde_json::to_string(&failed).unwrap();
    for private in [
        "private-provider-code",
        "private provider failure",
        "payload",
    ] {
        assert!(!public.contains(private));
    }
    Some(failed.invocation)
}

fn sanitized_applying_interaction(payload: PresencePayload) -> Option<tilo_actions::InvocationId> {
    let PresencePayload::Interaction(applying) = payload else {
        return None;
    };
    assert_eq!(applying.actor.id, "agent:tide");
    assert_eq!(applying.action.id, "open-agent-conversation");
    assert_eq!(applying.resource.id, "chat.person:tide");
    assert_eq!(applying.group, None);
    assert!(matches!(
        applying.phase,
        tilo_actions::InteractionPhase::Applying
    ));
    let public = serde_json::to_string(&applying).unwrap();
    assert!(!public.contains("private model argument"));
    Some(applying.invocation)
}

async fn receive_managed_presence(
    receiver: &mut mpsc::UnboundedReceiver<PresenceEnvelope>,
) -> PresenceEnvelope {
    tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
        .await
        .expect("managed presence delivery must not stall")
        .expect("managed presence subscription must remain open")
}

#[tokio::test(flavor = "current_thread")]
async fn actual_capnp_managed_action_refuses_invalid_identity_and_unsupported_step() {
    let ManagedActionFixture {
        fanout: _fanout,
        state,
        membership: _membership,
        directories,
        route,
        admitted,
        mut delivered,
        owner: _owner,
    } = managed_action_fixture();
    let mut exact = state.subscribe_presence(
        &CrdtRoute::record("so.tilo.chat", &route.cluster_id, "").channel_key(),
        99,
    );
    let client = managed_action_client(&state, directories, &route, admitted);
    for refused in [
        "not-an-invocation-uuid",
        "00000000-0000-0000-0000-000000000000",
    ] {
        assert!(
            invoke_managed_action(&client, &managed_action_request(refused))
                .await
                .is_err()
        );
        assert!(delivered.try_recv().is_err());
        assert!(state.pending_invocations.lock().unwrap().is_empty());
        assert!(exact.try_recv().is_err());
    }
    let mut unsupported = managed_action_request("");
    unsupported.argument = None;
    unsupported.step = Some(1);
    let error = invoke_managed_action(&client, &unsupported)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("durable task steps are not served by this host"));
    assert!(delivered.try_recv().is_err());
    assert!(state.pending_invocations.lock().unwrap().is_empty());
    assert!(exact.try_recv().is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn actual_capnp_managed_action_preserves_requested_identity_and_observation() {
    let ManagedActionFixture {
        fanout: _fanout,
        state,
        directories,
        route,
        admitted,
        mut delivered,
        ..
    } = managed_action_fixture();
    let admitted_caller = admitted.caller(&route.cluster_id);
    let client = managed_action_client(&state, directories, &route, admitted);
    let requested = tilo_actions::InvocationId(uuid::Uuid::from_u128(
        0x1020_3040_5060_4780_90a0_b0c0_d0e0_f001,
    ));
    let request = managed_action_request(requested.0.to_string());
    let (id, initial) = invoke_managed_action(&client, &request).await.unwrap();
    assert_eq!(initial.invocation(), requested);
    let invocation = delivered.recv().await.unwrap();
    assert_eq!(invocation.id, id);
    assert_eq!(invocation.invocation, requested);
    assert_eq!(invocation.actor, "agent:tide");
    assert_eq!(state.remote_job(&admitted_caller, &requested), Some(id));
    assert_eq!(
        state.remote_job(
            &tilo_actions::remote::Caller::local_agent("reed").in_room(route.cluster_id.clone()),
            &requested,
        ),
        None,
        "another Agent cannot observe the caller-bound invocation",
    );
    let (replayed, _) = invoke_managed_action(&client, &request).await.unwrap();
    assert_eq!(replayed, id);
    assert!(
        delivered.try_recv().is_err(),
        "an exact retry must not redispatch"
    );
    let mut changed = managed_action_request(requested.0.to_string());
    changed.argument = Some("changed argument");
    assert!(
        invoke_managed_action(&client, &changed).await.is_err(),
        "the same UUID cannot name changed action input"
    );
    assert!(delivered.try_recv().is_err());
    {
        let mut runs = state.pending_task_runs.lock().unwrap();
        let mut replacement = runs.get(&7).unwrap().clone();
        replacement.lease = "lease-8".into();
        runs.insert(8, replacement);
    }
    let mut replacement = managed_action_request(requested.0.to_string());
    replacement.run_id = 8;
    replacement.lease = "lease-8";
    assert!(
        invoke_managed_action(&client, &replacement).await.is_err(),
        "a new owner run cannot inherit an earlier run's UUID"
    );
    assert!(delivered.try_recv().is_err());
    let observed = observe_managed_action(&client, requested)
        .await
        .expect("the exact requested invocation must be observable");
    assert_eq!(observed.invocation(), requested);
    assert!(!observed.is_terminal());
    state
        .apply_invocation_update(
            41,
            id,
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: requested,
                output: tilo_actions::ActionOutput::default(),
            },
            None,
        )
        .await
        .unwrap();
    let observed = observe_managed_action(&client, requested).await.unwrap();
    assert_eq!(observed.invocation(), requested);
    assert!(observed.is_terminal());
}

#[tokio::test(flavor = "current_thread")]
async fn actual_capnp_managed_action_publishes_only_sanitized_exact_agent_lifecycle() {
    let ManagedActionFixture {
        fanout: _fanout,
        state,
        directories,
        route,
        admitted,
        mut delivered,
        ..
    } = managed_action_fixture();
    let mut exact = state.subscribe_presence(
        &CrdtRoute::record("so.tilo.chat", &route.cluster_id, "").channel_key(),
        99,
    );
    let mut foreign = state.subscribe_presence(
        &CrdtRoute::record("so.tilo.chat", &route.cluster_id, "other").channel_key(),
        100,
    );
    let client = managed_action_client(&state, directories, &route, admitted);
    let requested = tilo_actions::InvocationId(uuid::Uuid::from_u128(
        0x1020_3040_5060_4780_90a0_b0c0_d0e0_f004,
    ));
    let (id, initial) =
        invoke_managed_action(&client, &managed_action_request(requested.0.to_string()))
            .await
            .unwrap();
    assert_eq!(initial.invocation(), requested);
    let invocation = delivered.recv().await.unwrap();
    assert_eq!(invocation.invocation, requested);
    let target = state
        .pending_invocations
        .lock()
        .unwrap()
        .get(&id)
        .unwrap()
        .managed_interaction
        .clone()
        .unwrap();
    assert!(state.managed_interaction_is_current(&target));

    let accepted = receive_managed_presence(&mut exact).await;
    assert_eq!(accepted.user_id, "agent:tide");
    let accepted = sanitized_applying_interaction(accepted.payload)
        .expect("managed action must publish semantic interaction presence");
    assert!(foreign.try_recv().is_err());

    let update = tilo_actions::InvocationUpdate::Failed {
        invocation: accepted,
        error: tilo_actions::ActionFailure {
            code: "private-provider-code".into(),
            message: "private provider failure and output".into(),
            details: Some(serde_json::json!({"private":"payload"})),
            retryable: false,
        },
    };
    state
        .apply_invocation_update(41, id, update, None)
        .await
        .unwrap();
    let observed = observe_managed_action(&client, requested)
        .await
        .expect("the terminal requested invocation must remain observable");
    assert_eq!(observed.invocation(), requested);
    assert!(observed.is_terminal());
    let failed = sanitized_failed_interaction(receive_managed_presence(&mut exact).await.payload)
        .expect("terminal action must publish semantic interaction presence");
    assert!(foreign.try_recv().is_err());
    assert!(
        state
            .apply_invocation_update(
                41,
                id,
                tilo_actions::InvocationUpdate::Progress {
                    invocation: failed,
                    sequence: 2,
                    phase: "private late progress".into(),
                    completed: None,
                    total: None,
                    message: Some("private late payload".into()),
                },
                None
            )
            .await
            .is_err()
    );
    assert!(
        exact.try_recv().is_err(),
        "late duplicate updates publish nothing"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn managed_action_refuses_confirmation_and_stale_or_replaced_publication() {
    let ManagedActionFixture {
        fanout: _fanout,
        state,
        membership,
        directories,
        route,
        admitted,
        mut delivered,
        owner,
    } = managed_action_fixture();
    let mut exact = state.subscribe_presence(
        &CrdtRoute::record("so.tilo.chat", &route.cluster_id, "").channel_key(),
        99,
    );
    let client: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
        committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
        state: state.clone(),
        directories,
        channel_id: route.channel_key(),
        peer_id: 17,
        verified_identity: admitted,
        record_kind: None,
        lease: None,
    });
    let invoke = |confirmation: &str, invocation: &str| {
        let mut request = client.invoke_task_action_request();
        let mut params = request.get();
        params.set_run_id(7);
        params.set_lease("lease-7");
        params.set_peer_id(41);
        params.set_resource("chat.person:tide");
        params.set_action("open-agent-conversation");
        params.set_argument("");
        params.set_has_argument(false);
        params.set_confirmation_json(confirmation);
        params.set_invocation(invocation);
        request.send().promise
    };
    let first = "10203040-5060-4780-90a0-b0c0d0e0f002";
    let second = "10203040-5060-4780-90a0-b0c0d0e0f003";
    assert!(
        invoke("caller-controlled-confirmation", first)
            .await
            .is_err()
    );

    state.surfaces.lock().unwrap().get_mut(&41).unwrap().actions[0]
        .contract
        .confirmation = tilo_actions::ConfirmationPolicy::Confirm;
    assert!(invoke("", first).await.is_err());
    assert!(
        exact.try_recv().is_err(),
        "refused admissions publish no marker"
    );
    state.surfaces.lock().unwrap().get_mut(&41).unwrap().actions[0]
        .contract
        .confirmation = tilo_actions::ConfirmationPolicy::None;

    state.surfaces.lock().unwrap().get_mut(&41).unwrap().actions[0].action_label =
        "Replacement action".into();
    let reply = invoke("", first).await.unwrap();
    let id = reply.get().unwrap().get_id();
    delivered.recv().await.unwrap();
    let target = state
        .pending_invocations
        .lock()
        .unwrap()
        .get(&id)
        .unwrap()
        .managed_interaction
        .clone()
        .unwrap();
    exact.try_recv().unwrap();
    assert_eq!(target.action.label, "Replacement action");
    owner
        .lock()
        .unwrap()
        .controls
        .get_mut("task:00000000-0000-4000-8000-000000000109")
        .unwrap()
        .active_run
        .as_mut()
        .unwrap()
        .epoch = 4;
    assert!(!state.managed_interaction_is_current(&target));
    let invocation = state.invocation_status(id).unwrap().invocation();
    state
        .apply_invocation_update(
            41,
            id,
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "private-stale-code".into(),
                    message: "private stale failure".into(),
                    details: None,
                    retryable: false,
                },
            },
            None,
        )
        .await
        .unwrap();
    assert!(
        exact.try_recv().is_err(),
        "a replaced native owner epoch receives no lifecycle"
    );

    owner
        .lock()
        .unwrap()
        .controls
        .get_mut("task:00000000-0000-4000-8000-000000000109")
        .unwrap()
        .active_run
        .as_mut()
        .unwrap()
        .epoch = 3;
    let retained_workspaces = membership.read().workspaces.clone();
    membership.write().workspaces.clear();
    assert!(!state.managed_interaction_is_current(&target));
    assert!(invoke("", first).await.is_err());
    assert!(
        exact.try_recv().is_err(),
        "a revoked Agent publishes nothing"
    );
    membership.write().workspaces = retained_workspaces;

    let reply = invoke("", second).await.unwrap();
    let id = reply.get().unwrap().get_id();
    delivered.recv().await.unwrap();
    let target = state
        .pending_invocations
        .lock()
        .unwrap()
        .get(&id)
        .unwrap()
        .managed_interaction
        .clone()
        .unwrap();
    exact.try_recv().unwrap();
    state.pending_task_runs.lock().unwrap().remove(&7);
    let invocation = state.invocation_status(id).unwrap().invocation();
    state
        .apply_invocation_update(
            41,
            id,
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "private-retired-code".into(),
                    message: "private retired failure".into(),
                    details: None,
                    retryable: false,
                },
            },
            None,
        )
        .await
        .unwrap();
    assert!(exact.try_recv().is_err(), "a retired run publishes nothing");
    state.surfaces.lock().unwrap().remove(&41);
    assert!(!state.managed_interaction_is_current(&target));
}
