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

#[tokio::test(flavor = "current_thread")]
async fn a_connection_calls_its_own_person_what_the_member_list_calls_them() {
    struct NativeWorkspace(String);
    impl crate::services::WorkspaceMemberships for NativeWorkspace {
        fn requires_workspace(&self, workspace: &str) -> bool {
            workspace == self.0
        }

        fn execute(
            &self,
            _: String,
            _: tilo_user_identity::DeviceCert,
            _: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        ) -> crate::services::WorkspaceMembershipFuture {
            Box::pin(async { Err("roster fixture never commits owner state".into()) })
        }
    }

    let root = SigningKey::from_bytes(&[47; 32]);
    let person = person_id_for_root(root.verifying_key().to_bytes());
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(47),
        "Studio",
    );
    workspace.policy.set_owner(person.clone());
    workspace.members.push(collab_types::Member::of_person(
        &collab_types::Person::new(person, "Ada"),
        None,
    ));
    // A person nowhere near, whose own device named them and the workspace's governance keeps
    // that name: no member row here names them.
    let absent = "b".repeat(64);
    workspace
        .policy
        .add_member(absent.clone(), crate::test_support::Role::Editor);
    assert!(workspace.named_people([(absent.as_str(), "Ben")]));
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
    let roster = state.native_membership_roster(&route, &admitted).unwrap();
    let me = roster
        .iter()
        .find(|row| row.is_self)
        .expect("the person's own row");
    assert_eq!(me.name, "Ada", "never their key: {roster:?}");
    let row = roster
        .iter()
        .find(|row| row.id == collab_types::MemberId::person(absent.clone()))
        .expect("the absent person's row");
    assert_eq!(
        row.name, "Ben",
        "someone who is not here is called what the governance names them"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_context_projects_canonical_agents_and_withdraws_exactly() {
    struct NativeWorkspace(String);
    impl crate::services::WorkspaceMemberships for NativeWorkspace {
        fn requires_workspace(&self, workspace: &str) -> bool {
            workspace == self.0
        }

        fn execute(
            &self,
            _: String,
            _: tilo_user_identity::DeviceCert,
            _: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        ) -> crate::services::WorkspaceMembershipFuture {
            Box::pin(async { Err("roster fixture never commits owner state".into()) })
        }
    }

    let root = SigningKey::from_bytes(&[46; 32]);
    let person = person_id_for_root(root.verifying_key().to_bytes());
    let tide = "tide";
    let reed = "reed";
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(46),
        "Roster",
    );
    workspace.policy.set_owner(person.clone());
    let workspace_id = workspace.id().to_string();
    let other_person = "other-person";
    workspace
        .policy
        .add_member(other_person, crate::test_support::Role::Viewer);
    workspace.policy.joined.insert(other_person.into(), 7_000);
    workspace.allow_in_workspace(
        crate::test_support::Grantee::person("person-in-other-workspace"),
        &uuid::Uuid::new_v4().to_string(),
        crate::test_support::KindSet::READ,
    );
    let collision = workspace.allow_agent(&person, crate::test_support::KindSet::READ);
    workspace.allow_agent(tide, crate::test_support::KindSet::READ);
    workspace.allow_in_workspace(
        crate::test_support::Grantee::agent("wrong-workspace", "person-in-other-workspace"),
        &uuid::Uuid::new_v4().to_string(),
        crate::test_support::KindSet::READ,
    );
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace.clone());
    let route = CrdtRoute::record("so.tilo.chat", &workspace_id, "main");
    let admitted = admit_verified_join(&tree, &route, verified_device(&root)).unwrap();
    assert_eq!(
        AppBrokerState::member_id(&admitted),
        tilo_actions::MemberId::person(person.clone())
    );
    let membership = tree.shared();
    let fanout = Arc::new(FanoutState::new());
    let (state, _bridge_rx) = AppBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(membership.authority()),
        None,
        None,
        ProcessServices {
            workspace_memberships: Some(Arc::new(NativeWorkspace(workspace_id.clone()))),
            ..Default::default()
        },
    );
    fanout.attach_app_broker(state.clone());

    let roster = state.native_membership_roster(&route, &admitted).unwrap();
    let legacy_route = CrdtRoute::record("so.tilo.chat", uuid::Uuid::new_v4().to_string(), "main");
    assert!(
        state
            .native_roster_for_subscriber(&legacy_route, &admitted)
            .unwrap()
            .is_none(),
        "a workspace the native owner did not select keeps ordinary app context"
    );
    assert_eq!(roster.iter().filter(|row| row.is_self).count(), 1);
    assert!(
        roster
            .iter()
            .any(|row| row.id == collab_types::MemberId::person(person.clone()))
    );
    assert!(
        roster
            .iter()
            .all(|row| row.id != collab_types::MemberId::person("person-in-other-workspace"))
    );
    assert!(
        roster
            .iter()
            .any(|row| { row.id == collab_types::MemberId::person(other_person) && !row.is_self })
    );
    // When each person joined travels with their row, so every replica
    // numbers namesakes alike; the owner holds the workspace from its start.
    let joined =
        |id: collab_types::MemberId| roster.iter().find(|row| row.id == id).unwrap().joined_at;
    assert_eq!(
        joined(collab_types::MemberId::person(other_person)),
        Some(7_000)
    );
    assert_eq!(
        roster.iter().find(|row| row.is_self).unwrap().joined_at,
        Some(0)
    );
    assert_eq!(joined(collab_types::MemberId::agent(tide, "ada")), None);
    assert!(
        roster.iter().any(
            |row| row.id == collab_types::MemberId::agent(person.clone(), "ada") && !row.is_self
        )
    );
    assert!(
        roster
            .iter()
            .any(|row| row.id == collab_types::MemberId::agent(tide, "ada"))
    );
    assert!(
        roster.iter().all(|row| row.id
            != collab_types::MemberId::agent("wrong-workspace", "person-in-other-workspace")),
        "a grant for another workspace is not native membership here"
    );
    assert!(
        state
            .native_membership_roster(
                &CrdtRoute::record("so.tilo.chat", uuid::Uuid::new_v4().to_string(), "main",),
                &admitted,
            )
            .is_err()
    );
    // An agent wears the colour its face wears everywhere, so the card it
    // shows in a window is drawn as the bar draws it.
    for agent in roster.iter().filter(|row| !row.id.is_person()) {
        assert_eq!(
            agent.colour,
            collab_types::presence::PresenceHue::of_member(&agent.id),
            "{}",
            agent.id
        );
    }

    let channel = route.channel_key();
    let native = state
        .native_roster_for_subscriber(&route, &admitted)
        .unwrap()
        .expect("the actual owner selects this native workspace");
    assert!(state.host_workers.lock().unwrap().is_empty());
    let mut parallel = state.subscribe_context(&channel, 46, Some(native.clone()));
    assert!(matches!(
        parallel.try_recv(),
        Ok(ContextUpdate::Snapshot { .. })
    ));
    let mut stream = state.subscribe_context(&channel, 46, Some(native.clone()));
    let mut ordinary = state.subscribe_context(&channel, 47, None);
    assert!(matches!(
        stream.try_recv(),
        Ok(ContextUpdate::Snapshot { .. })
    ));
    let forged = collab_types::Member::of_software(
        collab_types::MemberId::agent("forged", "fixture-owner"),
        "forged",
    );
    {
        let mut channels = state.channels.lock().unwrap();
        AppBrokerState::fanout_context(
            channels.values_mut(),
            u64::MAX,
            std::slice::from_ref(&forged),
            &[],
            &[],
        );
    }
    assert!(
        stream.try_recv().is_err(),
        "app context must not replace an authenticated native roster"
    );
    assert!(
        parallel.try_recv().is_err(),
        "app context must not replace a parallel authenticated native roster"
    );
    let ContextUpdate::Snapshot { members, .. } = ordinary.try_recv().unwrap() else {
        panic!("ordinary context subscribers still receive app context");
    };
    assert_eq!(members.len(), 1);
    assert_eq!(members[0], forged);
    assert!(
        membership.write().workspaces[0]
            .policy
            .revoke_grant(&collision)
    );
    assert!(
        membership.write().workspaces[0]
            .policy
            .dismiss(other_person)
    );
    state.refresh_workspace_context_services().unwrap();
    let ContextUpdate::Snapshot { members, .. } = stream.try_recv().unwrap() else {
        panic!("membership refresh must be a complete snapshot");
    };
    assert!(
        members
            .iter()
            .all(|row| row.id != collab_types::MemberId::agent(person.clone(), "ada"))
    );
    assert!(
        members
            .iter()
            .all(|row| row.id != collab_types::MemberId::person(other_person))
    );
    assert!(
        members
            .iter()
            .any(|row| row.id == collab_types::MemberId::person(person.clone()) && row.is_self)
    );
    let ContextUpdate::Snapshot { members, .. } = parallel.try_recv().unwrap() else {
        panic!("every mounted root must receive membership removal");
    };
    assert!(
        members
            .iter()
            .all(|row| row.id != collab_types::MemberId::agent(person.clone(), "ada"))
    );
    membership.write().workspaces[0].allow_agent(reed, crate::test_support::KindSet::READ);
    state.refresh_workspace_context_services().unwrap();
    let ContextUpdate::Snapshot { members, .. } = stream.try_recv().unwrap() else {
        panic!("membership addition must be a complete snapshot");
    };
    assert!(
        members
            .iter()
            .any(|row| row.id == collab_types::MemberId::agent(reed, "ada"))
    );
    let ContextUpdate::Snapshot { members, .. } = parallel.try_recv().unwrap() else {
        panic!("every mounted root must receive membership addition");
    };
    assert!(
        members
            .iter()
            .any(|row| row.id == collab_types::MemberId::agent("reed", "ada"))
    );

    let mut queued = state.subscribe_context(&channel, 48, Some(native.clone()));
    assert!(matches!(
        queued.try_recv(),
        Ok(ContextUpdate::Snapshot { .. })
    ));
    let queued = Rc::new(AsyncMutex::new(wire::Feed::new(queued).with_native_guard(
        Some(NativeContextReadGuard {
            state: state.clone(),
            route: route.clone(),
            identity: native.admitted,
        }),
    )));
    let reading = Rc::clone(&queued);
    let mut pending = Box::pin(async move { reading.lock().await.next().await });
    // The actual stream read must be waiting before revocation: it holds
    // the queue while it waits for the next envelope.
    tilo_test_env::within("the stream read to wait for an envelope", async {
        while queued.try_lock().is_ok() {
            assert!(futures::poll!(&mut pending).is_pending());
            tokio::task::yield_now().await;
        }
    })
    .await;
    membership.write().workspaces[0].policy.owner = None;
    state.refresh_workspace_context_services().unwrap();
    assert!(
        !matches!(pending.await, Ok(Some(_))),
        "revocation must terminate an already waiting native read"
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

/// The roster an app reads names each member as they set themselves: the picture a person keeps
/// in their profile, and an agent's persona from the workspace's record of its agents. What a
/// window draws as an author's face comes from these rows.
#[tokio::test(flavor = "current_thread")]
async fn the_member_list_draws_a_persons_profile_picture_and_an_agents_persona() {
    /// The image a person set, and the picture an agent's persona keeps: a 1-by-1 PNG.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x1a\n\x00\x00\x00\rIHDR";
    const PERSONA: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAMAAAAoyzS7AAAAA1BMVEX///+nxBvIAAAAAXRSTlMAQObYZgAAAAxJREFUeNoBgAEAAP//AwABWgH+AAAAAElFTkSuQmCC";

    struct Photos([u8; 32]);

    impl crate::services::WorkspaceMemberships for Photos {
        fn requires_workspace(&self, _: &str) -> bool {
            true
        }

        fn execute(
            &self,
            _: String,
            _: tilo_user_identity::DeviceCert,
            _: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        ) -> crate::services::WorkspaceMembershipFuture {
            Box::pin(async { Err("roster fixture never commits owner state".into()) })
        }

        fn person_photo(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
            (person == &self.0).then(|| PNG.to_vec())
        }
    }

    /// Ada's agent Scout.
    struct Scouts(String);

    impl crate::services::AgentRoster for Scouts {
        fn listed(&self, _: &str) -> Vec<crate::services::ListedAgent> {
            vec![crate::services::ListedAgent {
                id: "scout".to_owned(),
                name: "Scout".to_owned(),
                avatar: Some(PERSONA.to_owned()),
                acts_for: self.0.clone(),
            }]
        }
    }

    let root = SigningKey::from_bytes(&[52; 32]);
    let person = person_id_for_root(root.verifying_key().to_bytes());
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(52),
        "Personas",
    );
    workspace.policy.set_owner(person.clone());
    workspace.members.push(collab_types::Member::of_person(
        &collab_types::Person::new(person.clone(), "Ada"),
        None,
    ));
    let cluster_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let route = CrdtRoute::record("so.tilo.lang.chat", &cluster_id, "main");
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
            workspace_memberships: Some(Arc::new(Photos(root.verifying_key().to_bytes()))),
            agents: Some(Arc::new(Scouts(person.clone()))),
            ..Default::default()
        },
    );
    fanout.attach_app_broker(state.clone());
    let roster = state.native_membership_roster(&route, &admitted).unwrap();
    let me = roster.iter().find(|row| row.is_self).expect("Ada's row");
    assert_eq!(
        me.avatar.as_ref().map(|avatar| avatar.0.as_slice()),
        Some(PNG),
        "the picture Ada set in her profile: {me:?}"
    );
    let scout = roster
        .iter()
        .find(|row| row.id == collab_types::MemberId::agent("scout", person.as_str()))
        .expect("Scout's row");
    assert_eq!(scout.name, "Scout", "the audience reads its persona's name");
    assert_eq!(
        scout.avatar,
        collab_types::avatar_of(PERSONA),
        "and the picture its persona keeps"
    );
}

/// The member list a daemon serves pictures a person whose profile the host keeps: another
/// member reading it sees their face, though the two are on different devices and the daemon is
/// the only place the profile lives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_members_picture_reaches_another_member_through_the_daemons_member_list() {
    use tilo_workspace_service::workspaces::PersonNames;
    use tilo_workspace_service::workspaces::fixtures::{hosting_device, test_host};

    /// A picture a profile keeps: a 1-by-1 PNG.
    const PHOTO: &[u8] = b"\x89PNG\r\n\x1a\n\x1a\n\x00\x00\x00\rIHDR";

    struct KeptPhotos([u8; 32]);

    impl PersonNames for KeptPhotos {
        fn name_of(&self, _: &[u8; 32]) -> Option<String> {
            None
        }

        fn keep_name(&self, _: &[u8; 32], _: &str) -> Result<(), String> {
            Ok(())
        }

        fn photo_of(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
            (person == &self.0).then(|| PHOTO.to_vec())
        }
    }

    // The hosted daemon: a workspace service keeping its member's profile.
    let dir = tempfile::tempdir().expect("a state directory");
    let (key, cert) = hosting_device();
    let host = test_host(dir.path(), &key, &cert).await;
    host.workspaces()
        .person_names(Arc::new(KeptPhotos(cert.user_public_key)));

    // Ada holds the workspace here; Bo reads it from another device.
    let ada = person_id_for_root(cert.user_public_key);
    let bo_root = SigningKey::from_bytes(&[53; 32]);
    let bo = hex::encode(bo_root.verifying_key().to_bytes());
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(53),
        "Pictured",
    );
    workspace.policy.set_owner(ada.clone());
    workspace.members.push(collab_types::Member::of_person(
        &collab_types::Person::new(ada, "Ada"),
        None,
    ));
    workspace
        .policy
        .add_member(bo.clone(), crate::test_support::Role::Editor);
    let cluster_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);

    let route = CrdtRoute::record("so.tilo.lang.chat", &cluster_id, "main");
    let admitted = admit_verified_join(&tree, &route, verified_device(&bo_root)).unwrap();
    let membership = tree.shared();
    let fanout = Arc::new(FanoutState::new());
    let (state, _bridge_rx) = AppBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(membership.authority()),
        None,
        None,
        ProcessServices {
            // The service the daemon installs on its broker.
            workspace_memberships: Some(
                tilo_workspace_service::workspaces::ProfileMemberships::new(
                    host.workspaces().clone(),
                    membership.authority(),
                ) as Arc<dyn crate::services::WorkspaceMemberships>,
            ),
            ..Default::default()
        },
    );
    fanout.attach_app_broker(state.clone());
    let members = state.native_membership_roster(&route, &admitted).unwrap();
    let ada = members
        .iter()
        .find(|row| {
            row.id == collab_types::MemberId::person(person_id_for_root(cert.user_public_key))
        })
        .expect("Ada's row");
    assert_eq!(
        ada.avatar.as_ref().map(|avatar| avatar.0.as_slice()),
        Some(PHOTO),
        "the picture her profile holds reaches the member list another member reads: {ada:?}"
    );
    host.shutdown().await;
}
