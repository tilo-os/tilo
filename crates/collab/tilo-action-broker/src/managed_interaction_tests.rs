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

#[tokio::test(flavor = "current_thread")]
async fn actual_capnp_managed_action_publishes_only_sanitized_exact_agent_lifecycle() {
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
    let mut foreign = state.subscribe_presence(
        &CrdtRoute::record("so.tilo.chat", &route.cluster_id, "other").channel_key(),
        100,
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
    let mut request = client.invoke_task_action_request();
    {
        let mut params = request.get();
        params.set_run_id(7);
        params.set_lease("lease-7");
        params.set_peer_id(41);
        params.set_resource("chat.person:tide");
        params.set_action("open-agent-conversation");
        params.set_argument("private model argument");
        params.set_has_argument(true);
        params.set_confirmation_json("");
        params.set_invocation("");
    }
    let response = request.send().promise.await.unwrap();
    let id = response.get().unwrap().get_id();
    let invocation = delivered.recv().await.unwrap();
    assert_eq!(invocation.id, id);
    assert_eq!(invocation.actor, "agent:tide");
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

    let accepted = exact.try_recv().unwrap();
    assert_eq!(accepted.user_id, "agent:tide");
    let PresencePayload::Interaction(accepted) = accepted.payload else {
        panic!("managed action must publish semantic interaction presence");
    };
    assert_eq!(accepted.actor.id, "agent:tide");
    assert_eq!(accepted.action.id, "open-agent-conversation");
    assert_eq!(accepted.resource.id, "chat.person:tide");
    assert_eq!(accepted.group, None);
    assert!(matches!(
        accepted.phase,
        tilo_actions::InteractionPhase::Applying
    ));
    let public = serde_json::to_string(&accepted).unwrap();
    assert!(!public.contains("private model argument"));
    assert!(foreign.try_recv().is_err());

    let update = tilo_actions::InvocationUpdate::Failed {
        invocation: accepted.invocation,
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
    let failed = exact.try_recv().unwrap();
    let PresencePayload::Interaction(failed) = failed.payload else {
        panic!("terminal action must publish semantic interaction presence");
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
    assert!(foreign.try_recv().is_err());
    assert!(
        state
            .apply_invocation_update(
                41,
                id,
                tilo_actions::InvocationUpdate::Progress {
                    invocation: failed.invocation,
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
    let invoke = |confirmation: &str| {
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
        params.set_invocation("");
        request.send().promise
    };
    assert!(invoke("caller-controlled-confirmation").await.is_err());

    state.surfaces.lock().unwrap().get_mut(&41).unwrap().actions[0]
        .contract
        .confirmation = tilo_actions::ConfirmationPolicy::Confirm;
    assert!(invoke("").await.is_err());
    assert!(
        exact.try_recv().is_err(),
        "refused admissions publish no marker"
    );
    state.surfaces.lock().unwrap().get_mut(&41).unwrap().actions[0]
        .contract
        .confirmation = tilo_actions::ConfirmationPolicy::None;

    state.surfaces.lock().unwrap().get_mut(&41).unwrap().actions[0].action_label =
        "Replacement action".into();
    let reply = invoke("").await.unwrap();
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
    assert!(invoke("").await.is_err());
    assert!(
        exact.try_recv().is_err(),
        "a revoked Agent publishes nothing"
    );
    membership.write().workspaces = retained_workspaces;

    let reply = invoke("").await.unwrap();
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
