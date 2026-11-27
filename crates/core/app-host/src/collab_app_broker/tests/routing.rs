#![allow(
    clippy::similar_names,
    reason = "sk/pk and owner/editor pairings are the conventional, intentional names for a keypair and a two-role fixture"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::panic_in_result_fn,
    clippy::arithmetic_side_effects,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::wildcard_imports,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    clippy::large_futures,
    clippy::await_holding_lock,
    clippy::await_holding_refcell_ref,
    reason = "test scenarios and fixtures: a failed unwrap is the assertion, and each scenario keeps its setup, guards and awaits in one readable body"
)]

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn ordinary_person_requested_uuid_survives_actual_delivery_and_completion() {
    use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
    tokio::task::LocalSet::new().run_until(async {
        let root = tempfile::tempdir().unwrap();
        let base = local_directory_fixture();
        let membership = base.membership.as_ref().unwrap().clone();
        membership.write().persist_path = Some(root.path().join("state.json"));
        drop(base);
        let mut fixture = local_directory_fixture_with_membership(membership);
        let offered = PublishedSurface {
            peer_id: fixture.peer_id,
            app_id: fixture.route.app_id.clone(),
            route: fixture.route.clone(),
            actions: vec![published_directory_action(&fixture.directory_state.borrow().row()).unwrap()],
            live: true,
        };
        fixture.state.publishers.surfaces.lock().unwrap().insert(fixture.peer_id, offered);
        let caller = Caller::local_person("fixture-owner").in_workspace(fixture.cluster_id.clone());
        assert!(caller.execution_id().is_none(), "no fabricated execution lease");
        let request = InvokeRequest {
            asks: false,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: fixture.peer_id.to_string(),
                address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
            },
            argument: InvokeArgument::None, context: None, confirmation: None,
            revision: None, on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let pending = fixture.state.action_invoke(&caller, None, None, &request);
        let receipt = fixture
            .drive(pending)
            .await
            .expect("published ordinary action must be admitted");
        assert_eq!(receipt.invocation, request.invocation);
        let (job, delivered) = fixture.take_invocation_identity().unwrap();
        assert_eq!(delivered, request.invocation);
        assert_ne!(delivered.0, uuid::Uuid::from_u128(u128::from(job)));
        assert!(matches!(fixture.state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Accepted { invocation }) if invocation == request.invocation));
        let path = root.path().join("state.json.invocations");
        let snapshot = tilo_invocation_journal::durable_snapshot(&path);
        assert_eq!(snapshot["records"][0]["key"]["invocation"], request.invocation.0.to_string());
        fixture.complete_invocation(job, &tilo_actions::InvocationUpdate::Succeeded {
            invocation: delivered, output: tilo_actions::ActionOutput::default(),
        }).await.unwrap();
        assert!(matches!(fixture.state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Succeeded { invocation, .. }) if invocation == request.invocation));
        let snapshot = tilo_invocation_journal::durable_snapshot(&path);
        assert_eq!(snapshot["records"][0]["key"]["invocation"], request.invocation.0.to_string());
        assert!(fixture.take_invocation_identity().is_none());
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_effects_follow_up_names_the_root_its_window_published_under_an_alias() {
    use tilo_actions::remote::{
        Caller, InvokeArgument, InvokeRequest, OWN_PROVIDER, RemoteActionTarget,
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut fixture = local_directory_fixture();
            // A Rust app publishes its root only under the window's transport
            // alias, while the app, its directory and the effect it runs name it
            // by its own id. The broker resolves one to the other.
            let logical = "workspace:atlas";
            let alias = format!(
                "{}fixture-host:root:{}",
                tilo_actions::BOUND_RESOURCE_PREFIX,
                hex::encode(logical)
            );
            let mut published = published_directory_action(&fixture.directory_state.borrow().row())
                .expect("the directory's row names an action");
            published.resource.clone_from(&alias);
            published.semantic_identity = tilo_actions::ResourceSemanticIdentity::new(
                published.resource_kind.clone(),
                logical,
            );
            assert_eq!(published.domain_resource(), logical);
            let surface = PublishedSurface {
                peer_id: fixture.peer_id,
                app_id: fixture.route.app_id.clone(),
                route: fixture.route.clone(),
                actions: vec![published],
                live: true,
            };
            assert_eq!(
                surface.current_resource(logical, "open"),
                Some(alias.as_str()),
                "the app's own id resolves to the alias carrying its action"
            );
            fixture
                .state
                .publishers
                .surfaces
                .lock()
                .unwrap()
                .insert(fixture.peer_id, surface);
            let caller =
                Caller::local_person("fixture-owner").in_workspace(fixture.cluster_id.clone());
            let request = InvokeRequest {
                asks: false,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: OWN_PROVIDER.into(),
                    address: tilo_actions::ActionAddress::new(logical, "open"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            let pending =
                fixture
                    .state
                    .action_invoke(&caller, Some(fixture.peer_id), None, &request);
            let receipt = fixture
                .drive(pending)
                .await
                .expect("the action of the app's own root is admitted");
            assert_eq!(receipt.invocation, request.invocation);
            let delivered = fixture
                .take_invocation()
                .expect("the app's connection is sent the action");
            assert_eq!(
                delivered.resource, alias,
                "the app receives the resource under the alias it published"
            );
            assert_eq!(delivered.action, "open");
        })
        .await;
}

/// The workspace's membership for a native subscriber is its `workspace/members` resource, kept
/// by the store: the context tells it when that resource may have moved and never carries the
/// members itself, and app-authored context never reaches a native subscriber.
#[tokio::test(flavor = "current_thread")]
async fn a_native_context_subscriber_is_told_where_the_members_are() {
    struct NativeWorkspace(String);
    impl crate::services::WorkspaceMemberships for NativeWorkspace {
        fn requires_workspace(&self, workspace: &str) -> bool {
            workspace == self.0
        }
    }

    let root = SigningKey::from_bytes(&[57; 32]);
    let person = person_id_for_root(root.verifying_key().to_bytes());
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(57),
        "Studio",
    );
    workspace.policy.set_owner(person);
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let route = CrdtRoute::record("so.tilo.chat", &workspace_id, "main");
    let admitted = admit_verified_join(&tree, &route, verified_device(&root)).unwrap();
    let tree = tree.shared();
    let fanout = Arc::new(FanoutState::new());
    let (state, _bridge_rx) = AppBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.authority()),
        None,
        None,
        ProcessServices {
            workspace_memberships: Some(Arc::new(NativeWorkspace(workspace_id.clone()))),
            ..Default::default()
        },
    );
    fanout.attach_app_broker(state.clone());

    let elsewhere = CrdtRoute::record("so.tilo.chat", uuid::Uuid::new_v4().to_string(), "main");
    assert!(
        state
            .native_roster_for_subscriber(&elsewhere, &admitted)
            .unwrap()
            .is_none(),
        "a workspace the native owner did not select keeps ordinary app context"
    );
    let native = state
        .native_roster_for_subscriber(&route, &admitted)
        .unwrap()
        .expect("the actual owner selects this native workspace");
    let channel = route.channel_key();
    let mut stream = state.subscribe_context(&channel, 46, Some(native));
    assert!(matches!(
        stream.try_recv(),
        Ok(ContextUpdate::Snapshot { .. })
    ));
    state.refresh_workspace_context_services().unwrap();
    assert_eq!(
        stream.try_recv().unwrap(),
        ContextUpdate::RecordsMoved(vec![tilo_resource_store::WORKSPACE_MEMBERS_ID.to_owned()]),
        "the window is told where the members are, not who they are"
    );
    state.refresh_workspace_context_services().unwrap();
    assert!(
        stream.try_recv().is_err(),
        "re-checked with nothing of its members changed, the window is not told again"
    );
    tree.write().workspaces[0]
        .policy
        .grant(tilo_action_policy::Grant::new(
            "guest-reads",
            tilo_action_policy::Grantee::person("guest"),
            tilo_action_policy::TreePath::workspace(&workspace_id),
            tilo_action_policy::KindSet::READ,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(person_id_for_root(
                root.verifying_key().to_bytes(),
            )),
        ));
    state.refresh_workspace_context_services().unwrap();
    assert_eq!(
        stream.try_recv().unwrap(),
        ContextUpdate::RecordsMoved(vec![tilo_resource_store::WORKSPACE_MEMBERS_ID.to_owned()]),
        "a grant changes who is there: the window reads its members again"
    );
    {
        let mut channels = state.channels.lock().unwrap();
        AppBrokerState::fanout_context(channels.values_mut(), u64::MAX, &[], &[]);
    }
    assert!(
        stream.try_recv().is_err(),
        "app context must not replace an authenticated native subscriber"
    );
}

/// An agent id the workspace's terms forbid admits nobody: a native subscriber is refused
/// rather than shown a membership the workspace may not have.
#[tokio::test(flavor = "current_thread")]
async fn a_native_context_refuses_a_noncanonical_agent_identity() {
    struct NativeWorkspace(String);
    impl crate::services::WorkspaceMemberships for NativeWorkspace {
        fn requires_workspace(&self, workspace: &str) -> bool {
            workspace == self.0
        }
    }

    let root = SigningKey::from_bytes(&[58; 32]);
    let person = person_id_for_root(root.verifying_key().to_bytes());
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(58),
        "Studio",
    );
    workspace.policy.set_owner(person);
    let _ = workspace.allow_agent("Not Canonical", crate::test_support::KindSet::READ);
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let route = CrdtRoute::record("so.tilo.chat", &workspace_id, "main");
    let admitted = admit_verified_join(&tree, &route, verified_device(&root)).unwrap();
    let fanout = Arc::new(FanoutState::new());
    let (state, _bridge_rx) = AppBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.shared().authority()),
        None,
        None,
        ProcessServices {
            workspace_memberships: Some(Arc::new(NativeWorkspace(workspace_id))),
            ..Default::default()
        },
    );
    fanout.attach_app_broker(state.clone());
    assert!(
        state.native_membership_roster(&route, &admitted).is_err(),
        "an agent id the workspace's terms forbid names nobody"
    );
}

#[test]
fn record_scoped_document_fanout_reaches_only_its_targeted_subscriber() {
    let fanout = Arc::new(FanoutState::new());
    let (state, _bridge_rx) = AppBrokerState::new(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        None,
        None,
    );
    fanout.attach_app_broker(Arc::clone(&state));

    let target = CrdtRoute::record("so.tilo.notes", "workspace-1", "note-a");
    let other_document = CrdtRoute::record("so.tilo.notes", "workspace-1", "note-b");
    let other_app = CrdtRoute::record("so.tilo.files", "workspace-1", "note-a");
    let other_workspace = CrdtRoute::record("so.tilo.notes", "workspace-2", "note-a");
    let mut target_rx = state.subscribe_context(&target.channel_key(), 2, None);
    let mut other_document_rx = state.subscribe_context(&other_document.channel_key(), 3, None);
    let mut other_app_rx = state.subscribe_context(&other_app.channel_key(), 4, None);
    let mut other_workspace_rx = state.subscribe_context(&other_workspace.channel_key(), 5, None);
    // Every subscriber opens with the context it shows: the permissions it has and the apps this
    // device installed.
    for (label, stream) in [
        ("the addressed record", &mut target_rx),
        ("another record", &mut other_document_rx),
        ("another app", &mut other_app_rx),
        ("another workspace", &mut other_workspace_rx),
    ] {
        assert!(
            matches!(stream.try_recv(), Ok(ContextUpdate::Snapshot { .. })),
            "{label} opens with its context"
        );
    }

    fanout.record_changed(&target);

    assert_eq!(
        target_rx.try_recv(),
        Ok(ContextUpdate::RecordChanged),
        "the addressed record is told it changed"
    );
    for (label, received) in [
        ("another record", other_document_rx.try_recv()),
        ("another app", other_app_rx.try_recv()),
        ("another workspace", other_workspace_rx.try_recv()),
    ] {
        assert!(
            matches!(received, Err(tokio::sync::mpsc::error::TryRecvError::Empty)),
            "{label} must not receive a record-directed document",
        );
    }
}

tilo_test_env::scenario! {
    fn a_published_action_is_found_on_the_route_that_carries_it_now;

    phase a_task_action_is_found_under_the_alias_its_window_publishes_now;
    // A window mounting a component anew publishes its resource under a new
    // alias: an action named on the old one is found on the one it is on now,
    // unless it is withdrawn or two aliases of the resource carry it.
    {
        let alias = |component: &str| {
            format!(
                "{}host:{component}:{}",
                tilo_actions::BOUND_RESOURCE_PREFIX,
                hex::encode("composer")
            )
        };
        let (before, now) = (alias("1"), alias("2"));
        let mut surface = catalogue_surface(4, "workspace", &now);
        assert_eq!(surface.current_resource(&now, "open"), Some(now.as_str()));
        assert_eq!(
            surface.current_resource(&before, "open"),
            Some(now.as_str())
        );
        assert_eq!(surface.current_resource(&before, "close"), None);
        // The app, its directory and a job effect name the resource by its
        // own id; the one alias carrying the action answers for it.
        assert_eq!(
            surface.current_resource("composer", "open"),
            Some(now.as_str())
        );
        assert_eq!(surface.current_resource("composer", "close"), None);
        assert_eq!(surface.current_resource("elsewhere", "open"), None);
        let mut twice = surface.actions[0].clone();
        twice.resource = alias("3");
        surface.actions.push(twice);
        assert_eq!(surface.current_resource(&before, "open"), None);
        assert_eq!(surface.current_resource("composer", "open"), None);
    }

    phase exact_published_action_uses_current_live_routes_before_archived_descriptors;
    {
        let broker = AppBrokerState::test_broker_with_idle_directory_queue();
        let state = broker.state();
        let retained = catalogue_surface(7, "workspace", "notebook");
        state.launchable.retain(&retained);
        let route = retained.route.clone();
        let (archived, descriptor) = state.published_action(&route, "notebook", "open").unwrap();
        assert!(!archived.live);
        assert!(
            archived.actions.is_empty(),
            "lookup must not clone the source inventory"
        );
        assert_eq!(descriptor.resource, "notebook");

        let mut empty = retained.clone();
        empty.actions.clear();
        state.replace_published_surfaces_for_test(vec![empty.clone()]);
        assert!(state.published_action(&route, "notebook", "open").is_none());

        let mut other_peer = retained;
        other_peer.peer_id = 8;
        other_peer.actions[0].action_label = "Current live action".into();
        state.replace_published_surfaces_for_test(vec![empty, other_peer]);
        let (live, descriptor) = state.published_action(&route, "notebook", "open").unwrap();
        assert!(live.live);
        assert_eq!(live.peer_id, 8);
        assert!(live.actions.is_empty());
        assert_eq!(descriptor.action_label, "Current live action");
        let foreign = CrdtRoute::record(&route.app_id, "another-workspace", &route.document_id);
        assert!(
            state
                .published_action(&foreign, "notebook", "open")
                .is_none()
        );
        state.replace_published_surfaces_for_test(Vec::new());
        assert!(
            !state
                .published_action(&route, "notebook", "open")
                .unwrap()
                .0
                .live
        );
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    clippy::unchecked_time_subtraction,
    reason = "the directed-action test keeps capability setup, delivery, and denial assertions in one scenario"
)]
#[test]
fn live_action_invocation_is_fresh_directed_and_permission_checked() {
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(31),
        "Action workspace",
    );
    workspace.policy.set_owner("owner");
    let route = CrdtRoute::app("so.tilo.files", workspace.id().to_string());
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let membership = tree.shared();
    let fanout = Arc::new(FanoutState::new());
    let (state, _bridge_rx) = AppBrokerState::new(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(membership),
        None,
    );
    state.publishers.surfaces.lock().expect("surfaces").insert(
        7,
        PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.files".into(),
            route,
            actions: vec![
                PublishedAction {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    resource: "files.entry:/tmp/report".into(),
                    action: "reveal".into(),
                    resource_kind: "files.entry".into(),
                    resource_label: "report".into(),
                    action_label: "Reveal".into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    operation_kind: tilo_actions::OperationKind::View,
                    params: Vec::new(),
                    contract: tilo_actions::ActionContract::view(),
                    view: None,
                    input: PublishedInput::None,
                },
                PublishedAction {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    resource: "files.entry:/tmp/report".into(),
                    action: "erase".into(),
                    resource_kind: "files.entry".into(),
                    resource_label: "report".into(),
                    action_label: "Erase".into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    operation_kind: tilo_actions::OperationKind::Effect,
                    params: Vec::new(),
                    contract: tilo_actions::ActionContract {
                        risk: tilo_actions::RiskLevel::Destructive,
                        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                        ..tilo_actions::ActionContract::default()
                    },
                    view: None,
                    input: PublishedInput::None,
                },
                PublishedAction {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    resource: "files.entry:/tmp/report".into(),
                    action: "refresh".into(),
                    resource_kind: "files.entry".into(),
                    resource_label: "report".into(),
                    action_label: "Refresh".into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    operation_kind: tilo_actions::OperationKind::Effect,
                    params: Vec::new(),
                    contract: tilo_actions::ActionContract {
                        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                        result: tilo_actions::ResultSpec::Job,
                        ..tilo_actions::ActionContract::default()
                    },
                    view: None,
                    input: PublishedInput::None,
                },
            ],
            live: true,
        },
    );
    let (tx, mut invocations) = mpsc::unbounded_channel();
    state
        .publishers
        .streams
        .lock()
        .expect("subscribers")
        .insert(7, tx);

    let action_cluster = state
        .publishers
        .surfaces
        .lock()
        .expect("surfaces")
        .get(&7)
        .expect("surface")
        .route
        .cluster_id
        .clone();
    let offered = |actor: &str| {
        let caller =
            tilo_actions::remote::Caller::local_person(actor).in_workspace(&action_cluster);
        state
            .eager_catalogue(&caller, action_provider::RowScope::All)
            .0
            .iter()
            .filter(|row| row.action.is_some())
            .any(|row| row.resource.id == "files.entry:/tmp/report")
    };
    assert!(offered("owner"));
    assert!(
        !offered("stranger"),
        "denied actions must be absent from discovery, not merely refused later"
    );

    let completion = invoke(
        &state,
        7,
        "files.entry:/tmp/report",
        "reveal",
        None,
        "owner",
    )
    .expect("owner can invoke current offer");
    let invocation = invocations.try_recv().expect("directed invocation");
    assert_eq!(invocation.resource, "files.entry:/tmp/report");
    assert!(
        start(&state, 7, "files.entry:/tmp/report", "erase", None, "owner").is_err(),
        "a consequential action cannot bypass the confirmation protocol"
    );
    let token = prepare(&state, 7, "files.entry:/tmp/report", "erase", None, "owner")
        .expect("confirmation grant");
    let erased = start_confirmed(
        &state,
        7,
        "files.entry:/tmp/report",
        "erase",
        None,
        "owner",
        Some(&token),
    )
    .expect("matching one-use grant");
    assert!(
        start_confirmed(
            &state,
            7,
            "files.entry:/tmp/report",
            "erase",
            None,
            "owner",
            Some(&token),
        )
        .is_err(),
        "confirmation grants are single use"
    );
    let erase_invocation = invocations.try_recv().expect("confirmed invocation");
    assert_eq!(erase_invocation.id, erased.id);

    let refresh = start(
        &state,
        7,
        "files.entry:/tmp/report",
        "refresh",
        None,
        "owner",
    )
    .expect("cancellable job");
    let refresh_invocation = invocations.try_recv().expect("job invocation");
    assert_eq!(refresh_invocation.id, refresh.id);
    let retained_cancellation = state
        .pending_invocations
        .lock()
        .expect("pending")
        .get(&refresh.id)
        .expect("refresh")
        .cancellation_fence
        .clone();
    assert!(!retained_cancellation.load(Ordering::Acquire));
    assert!(
        state
            .request_cancellation(
                refresh.id,
                &tilo_actions::Actor::from(tilo_actions::MemberId::person("stranger"))
            )
            .is_err()
    );
    state
        .request_cancellation(
            refresh.id,
            &tilo_actions::Actor::from(tilo_actions::MemberId::person("owner")),
        )
        .expect("owner can request cooperative cancellation")
        .wait_blocking()
        .expect("the request is durable");
    assert!(
        state
            .cancellation_requests
            .lock()
            .expect("cancellation registry")
            .contains(&refresh.id)
    );
    let now = tilo_clock::now();
    {
        let mut pending = state.pending_invocations.lock().expect("pending");
        pending
            .get_mut(&refresh.id)
            .expect("refresh remains pending")
            .cancellation_requested_at = Some(now - std::time::Duration::from_secs(2));
        pending
            .get_mut(&erased.id)
            .expect("erase remains pending")
            .last_activity = now - std::time::Duration::from_secs(2);
    }
    state.expire_stale_invocations_at(
        now,
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(1),
    );
    assert!(
        retained_cancellation.load(Ordering::Acquire),
        "the retained worker observes cancellation even after expiry removes the pending job"
    );
    assert!(matches!(
        state.invocation_status(refresh.id),
        Some(tilo_actions::InvocationUpdate::Failed { error, .. })
            if error.code == "action_cancellation_timeout"
    ));
    assert_eq!(
        refresh
            .completion
            .recv()
            .expect("cancel timeout completion")
            .code,
        "action_cancellation_timeout"
    );
    assert!(matches!(
        state.invocation_status(erased.id),
        Some(tilo_actions::InvocationUpdate::Failed { error, .. })
            if error.code == "action_timeout"
    ));
    assert_eq!(
        erased
            .completion
            .recv()
            .expect("idle timeout completion")
            .code,
        "action_timeout"
    );
    assert!(
        invoke(&state, 7, "files.entry:/tmp/report", "gone", None, "owner").is_err(),
        "an address absent from the current level-triggered surface is stale"
    );
    assert!(
        invoke(
            &state,
            7,
            "files.entry:/tmp/report",
            "reveal",
            None,
            "stranger"
        )
        .is_err(),
        "broker authorization is checked again at delivery time"
    );
    let pending = state
        .pending_invocations
        .lock()
        .expect("pending")
        .remove(&invocation.id)
        .expect("completion slot");
    pending
        .tx
        .send(InvocationCompletion {
            code: String::new(),
            message: String::new(),
        })
        .expect("caller still waiting");
    assert_eq!(
        completion.recv().expect("completion"),
        InvocationCompletion {
            code: String::new(),
            message: String::new(),
        }
    );

    let launchable = PublishedSurface {
        peer_id: 99,
        app_id: "so.tilo.files".into(),
        route: CrdtRoute::app(
            "so.tilo.files",
            state
                .publishers
                .surfaces
                .lock()
                .expect("surfaces")
                .get(&7)
                .expect("live surface")
                .route
                .cluster_id
                .clone(),
        ),
        actions: vec![PublishedAction {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: "files.app".into(),
            action: "new-window".into(),
            resource_kind: "files.app".into(),
            resource_label: "Files".into(),
            action_label: "New window".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::View,
            params: Vec::new(),
            contract: tilo_actions::ActionContract::view(),
            view: None,
            input: PublishedInput::None,
        }],
        live: false,
    };
    state.launchable.retain(&launchable);
    state
        .publishers
        .surfaces
        .lock()
        .expect("surfaces")
        .remove(&7);
    let archived = state.published_surfaces();
    assert_eq!(archived.len(), 1);
    assert!(!archived[0].live);
    assert_eq!(archived[0].actions[0].action, "new-window");
}

tilo_test_env::scenario! {
    fn a_directed_delivery_reaches_exactly_its_addressee;

    phase a_save_is_asked_of_exactly_the_addressed_window_and_ends_with_its_answer;
    {
        use collab_types::handoff::{HandoffReport, HandoffRequest};
        use tilo_actions::remote::{PendingPoll, pending};

        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = AppBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let workspace = uuid::Uuid::new_v4().to_string();
        let window = |app: &str, record: &str| CrdtRoute::record(app, workspace.clone(), record);
        let mut addressed =
            state.subscribe_context(&window("dev.tilo.notes", "continuation:5").channel_key(), 7, None);
        let mut sibling =
            state.subscribe_context(&window("dev.tilo.notes", "continuation:6").channel_key(), 8, None);
        let mut other_app =
            state.subscribe_context(&window("dev.tilo.sheets", "continuation:5").channel_key(), 9, None);
        let shown = window("dev.tilo.notes", "continuation:5");
        // Every window opens with the context it shows: the permissions it has and the apps this
        // device installed.
        for (label, stream) in [
            ("the addressed window", &mut addressed),
            ("another window of the app", &mut sibling),
            ("another app", &mut other_app),
        ] {
            assert!(
                matches!(stream.try_recv(), Ok(ContextUpdate::Snapshot { .. })),
                "{label} opens with its context"
            );
        }
        let asked = |token| {
            let (answer, saved) = pending();
            state.ask_save(&shown, token, answer);
            saved
        };

        let mut saved = asked(3);
        assert!(matches!(
            addressed.try_recv(),
            Ok(ContextUpdate::Handoff(message))
                if HandoffRequest::decode(&message) == Some(HandoffRequest::Checkpoint { token: 3 })
        ));
        assert!(
            sibling.try_recv().is_err(),
            "another window of the app is not asked"
        );
        assert!(other_app.try_recv().is_err());
        assert_eq!(saved.poll_ready(), PendingPoll::Waiting, "the save waits for the app");

        // Another window's answer, or one under another number, is no answer to it.
        assert!(state
            .answered_save(
                &window("dev.tilo.notes", "continuation:6"),
                &HandoffReport::Done { token: 3 }.encode()
            )
            .is_err());
        assert!(state.answered_save(&shown, &HandoffReport::Done { token: 4 }.encode()).is_err());
        assert_eq!(saved.poll_ready(), PendingPoll::Waiting);
        state
            .answered_save(&shown, &HandoffReport::Done { token: 3 }.encode())
            .expect("the window answers the save it was asked");
        assert_eq!(saved.poll_ready(), PendingPoll::Ready(HandoffReport::Done { token: 3 }));

        // The connection asked closing before it answered ends the save unanswered; another
        // connection closing leaves it waiting.
        let mut saved = asked(5);
        state.remove_connection(8);
        assert_eq!(saved.poll_ready(), PendingPoll::Waiting);
        state.remove_connection(7);
        assert_eq!(saved.poll_ready(), PendingPoll::Abandoned);

        // A window nobody shows is asked nothing: the save ends at once.
        let (answer, mut saved) = pending();
        state.ask_save(&window("dev.tilo.notes", "continuation:99"), 6, answer);
        assert_eq!(saved.poll_ready(), PendingPoll::Abandoned);
    }

}
