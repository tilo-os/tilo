//! Real native clock, certified local action, and durable terminal owner barrier.
use crate::shell::{
    broker_services::NativeCertificates, certified_action_session::ActionSessionSource,
    collab_broker, native_workspace_owner, state::WorkspaceTree,
};
use std::time::{Duration, Instant};
use tilo_action_broker::peer_actions::CertificateAuthority;
use tilo_actions::InvocationUpdate;
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

pub(super) struct NativeFixture {
    pub(super) home: tilo_test_env::TestHome,
    pub(super) owner: tilo_joined_room::NativeWorkspaceOwner,
    pub(super) broker: collab_broker::BrokerHandle,
    pub(super) certificate: tilo_user_identity::DeviceCert,
    pub(super) room: crate::shell::state::RoomId,
    pub(super) path: std::path::PathBuf,
}

fn ready_remote<T>(mut pending: tilo_actions::remote::Pending<T>) -> T {
    use tilo_actions::remote::PendingPoll;

    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    loop {
        match pending.poll_ready() {
            PendingPoll::Ready(result) => return result,
            PendingPoll::Abandoned => panic!("certified governance request was abandoned"),
            PendingPoll::Waiting => {
                assert!(
                    Instant::now() < deadline,
                    "certified governance request timed out"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn wait_for_invocation(actions: &collab_broker::AppActionBroker, job: u64) {
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    loop {
        match actions.status(job) {
            Some(InvocationUpdate::Succeeded { .. }) => return,
            Some(InvocationUpdate::Failed { error, .. }) => {
                panic!("certified governance action failed: {error:?}")
            }
            _ => {
                assert!(
                    Instant::now() < deadline,
                    "governance action did not settle"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn assert_governance_adversaries(
    actions: &collab_broker::AppActionBroker,
    session: &crate::shell::certified_action_session::DesktopActionSession,
    room: &str,
    provider: &str,
    revision: &str,
) {
    use tilo_actions::remote::{
        CatalogueQuery, Confirmation, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };

    let nonowner = ActionSessionSource::fixture(actions.clone())
        .open(room, Some("not-an-administrator"))
        .expect("the local policy fixture retains the distinct Person caller");
    let denied = ready_remote(nonowner.query(CatalogueQuery {
        provider: Some(provider.into()),
        max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    }))
    .expect("the service hides rather than errors for a denied Person");
    assert!(denied.rows.is_empty());
    let request = |provider: String, revision: Option<String>| InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider,
            address: tilo_actions::ActionAddress::new("workspace", "profile.save"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: Some(Confirmation::Confirmed),
        revision,
        fresh_binding: None,
        wait_ms: None,
        on_behalf_of: None,
    };
    assert!(
        ready_remote(nonowner.invoke_job(request(provider.into(), Some(revision.into())))).is_err(),
        "a distinct non-owner cannot invoke a hidden Admin action"
    );
    drop(nonowner);
    let foreign = tilo_actions::remote::workspace_provider(&uuid::Uuid::new_v4().to_string());
    let foreign_page = ready_remote(session.query(CatalogueQuery {
        provider: Some(foreign.clone()),
        max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    }))
    .expect("a valid foreign provider is classified before exact-room policy");
    assert!(foreign_page.rows.is_empty());
    assert!(
        ready_remote(session.invoke_job(request(foreign, None))).is_err(),
        "owner authority cannot be rebound to a foreign workspace provider"
    );
    assert!(
        ready_remote(session.query(CatalogueQuery {
            provider: Some("workspace:not-a-room".into()),
            max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        }))
        .is_err()
    );
}

#[test]
fn certified_owner_queries_and_invokes_native_workspace_governance() {
    use tilo_actions::remote::{
        CatalogueQuery, Confirmation, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };

    let fixture = fixture();
    let actions = fixture.broker.app_action_broker().unwrap();
    let room = fixture.room.to_string();
    let person = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
        fixture.certificate.user_public_key,
    );
    let session = ActionSessionSource::new(actions.clone())
        .open(&room, Some(&person))
        .expect("the certified current owner opens its exact action session");
    let provider = tilo_actions::remote::workspace_provider(&room);
    let page = ready_remote(session.query(CatalogueQuery {
        provider: Some(provider.clone()),
        max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    }))
    .expect("the certified workspace service catalogue is available");
    let descriptor = page
        .rows
        .iter()
        .find_map(|row| {
            row.action
                .as_ref()
                .filter(|action| {
                    row.provider == provider
                        && row.app_id == crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID
                        && action.address.action == "profile.save"
                })
                .cloned()
        })
        .expect("the certified owner sees profile.save on the broker-minted provider");

    let invocation = tilo_actions::InvocationId::new();
    let (job, receipt) = ready_remote(session.invoke_job(InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: provider.clone(),
            address: descriptor.address,
        },
        argument: InvokeArgument::Json(
            serde_json::json!({
                "name": "Certified owner profile",
                "description": "Production certified workspace service proof",
                "rules": [{
                    "scope": {"Action": {"app": "so.tilo.chat", "resource": "chat.person:tide", "action": "open-agent-conversation"}},
                    "effect": "Grant",
                    "verb": "Write"
                }]
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
        context: None,
        confirmation: Some(Confirmation::Confirmed),
        revision: Some(page.revision.clone()),
        fresh_binding: None,
        wait_ms: None,
        on_behalf_of: None,
    }))
    .expect("the certified owner invokes the retained governance descriptor");
    assert_eq!(receipt.invocation, invocation);
    wait_for_invocation(&actions, job);

    assert_governance_adversaries(&actions, &session, &room, &provider, &page.revision);
    drop(session);
    drop(actions);
    drop(fixture);
}

#[test]
fn certified_native_profile_assignment_advances_local_policy_revision() {
    use collab_types::caps::CapDecision;
    use tilo_action_broker::services::WorkspaceAuthority;

    let fixture = fixture();
    let actions = fixture.broker.app_action_broker().unwrap();
    let room = fixture.room.to_string();
    let person = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
        fixture.certificate.user_public_key,
    );
    let session = ActionSessionSource::new(actions.clone())
        .open(&room, Some(&person))
        .unwrap();
    let authority =
        super::NativeWorkspaceAuthority(native_workspace_owner::test_tree_of(&fixture.owner));
    let before = authority.snapshot().unwrap();
    let decision = |tree: &tilo_desktop_model::DesktopTree, agent: &str, action: &str| {
        profile_action_decision(tree, &room, agent, action)
    };
    assert_eq!(
        decision(&before, "tide", "open-agent-conversation"),
        CapDecision::DeniedByRole
    );
    invoke_certified_profile_action(
        &session,
        &actions,
        &room,
        "profile.save",
        &serde_json::json!({
            "name": "Exact Agent view", "rules": [{
                "scope": {"Action": {"app": "so.tilo.chat", "resource": "chat.person:tide", "action": "open-agent-conversation"}},
                "effect": "Grant", "verb": "Write"
            }]
        }),
    );
    invoke_certified_profile_action(
        &session,
        &actions,
        &room,
        "profile.assign",
        &serde_json::json!({
            "profile": "Exact Agent view", "subject_kind": "agent", "subject": "tide"
        }),
    );
    let after = authority.snapshot().unwrap();
    assert_eq!(
        decision(&after, "tide", "open-agent-conversation"),
        CapDecision::Allow
    );
    assert_eq!(
        decision(&after, "other", "open-agent-conversation"),
        CapDecision::DeniedByRole
    );
    assert_eq!(
        decision(&after, "tide", "message"),
        CapDecision::DeniedByRole
    );
    assert!(
        after.room_epoch() > before.room_epoch(),
        "actual signed owner governance advances its room policy witness"
    );
    publish_unrelated_owner_receipt(&fixture);
    let projected = authority.snapshot().unwrap();
    assert_eq!(
        decision(&projected, "tide", "open-agent-conversation"),
        CapDecision::Allow,
        "a subsequent owner publication must retain the certified exact grant"
    );
    assert_eq!(
        decision(&projected, "other", "open-agent-conversation"),
        CapDecision::DeniedByRole
    );
    assert_eq!(
        decision(&projected, "tide", "message"),
        CapDecision::DeniedByRole
    );
    drop(authority);
    drop(session);
    drop(actions);
    drop(fixture);
}

#[test]
fn native_person_profile_commands_retain_original_and_survive_owner_publication() {
    use collab_types::caps::CapDecision;
    use tilo_action_broker::services::WorkspaceAuthority;
    use tilo_actions::remote::Caller;

    let fixture = fixture();
    assert_native_profile_original_certificate(&fixture);
    let actions = fixture.broker.app_action_broker().unwrap();
    let host = crate::shell::remote_actions::BrokerActionHost::new(actions.clone());
    let room = fixture.room.to_string();
    let person = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
        fixture.certificate.user_public_key,
    );
    // No wire certificate or caller device labels: the actual installed seat
    // service must retain this original Person before the owner command.
    let caller = Caller::local_person(&person).in_room(&room);
    let authority =
        super::NativeWorkspaceAuthority(native_workspace_owner::test_tree_of(&fixture.owner));
    assert_eq!(
        profile_action_decision(
            &authority.snapshot().unwrap(),
            &room,
            "tide",
            "open-agent-conversation"
        ),
        CapDecision::DeniedByRole
    );
    for (operation, arguments) in [
        (
            "profile.save",
            serde_json::json!({
                "name": "Native Person exact view", "rules": [{
                    "scope": {"Action": {"app": "so.tilo.chat", "resource": "chat.person:tide", "action": "open-agent-conversation"}},
                    "effect": "Grant", "verb": "Read"
                }]
            }),
        ),
        (
            "profile.assign",
            serde_json::json!({
                "profile": "Native Person exact view", "subject_kind": "agent", "subject": "tide"
            }),
        ),
    ] {
        let request = native_profile_request(&host, &caller, &room, operation, &arguments);
        let (job, receipt) = ready_remote(host.invoke_local_person_job(&caller, &request)).unwrap();
        assert_eq!(receipt.invocation, request.invocation);
        wait_for_invocation(&actions, job);
        let generation = fixture
            .owner
            .with_current(fixture.room, |view| Ok(view.generation()))
            .unwrap();
        let replay = ready_remote(host.invoke_local_person_job(&caller, &request)).unwrap();
        assert_eq!(replay, (job, receipt));
        assert_eq!(
            fixture
                .owner
                .with_current(fixture.room, |view| Ok(view.generation()))
                .unwrap(),
            generation
        );
        for denied in [
            Caller::local_agent("tide").in_room(&room),
            caller.clone().from_foreign_device(true),
            Caller::local_person("forged-person").in_room(&room),
            Caller::authenticated(fixture.certificate.user_public_key, [97; 32]).in_room(&room),
            caller
                .clone()
                .in_room(crate::shell::state::RoomId::new().to_string()),
        ] {
            let mut fresh = request.clone();
            fresh.invocation = tilo_actions::InvocationId::new();
            assert!(ready_remote(host.invoke_local_person_job(&denied, &fresh)).is_err());
        }
        assert_eq!(
            fixture
                .owner
                .with_current(fixture.room, |view| Ok(view.generation()))
                .unwrap(),
            generation
        );
    }
    drop(host);
    drop(actions);
    publish_unrelated_owner_receipt(&fixture);
    let projected = authority.snapshot().unwrap();
    assert_eq!(
        profile_action_decision(&projected, &room, "tide", "open-agent-conversation"),
        CapDecision::Allow
    );
    assert_eq!(
        profile_action_decision(&projected, &room, "other", "open-agent-conversation"),
        CapDecision::DeniedByRole
    );
    assert_eq!(
        profile_action_decision(&projected, &room, "tide", "message"),
        CapDecision::DeniedByRole
    );
    drop(authority);
    drop(fixture);
}

fn assert_native_profile_original_certificate(fixture: &NativeFixture) {
    use tilo_action_broker::services::LocalPersonServices;
    let tree = native_workspace_owner::test_tree_of(&fixture.owner);
    let service = crate::shell::local_person::NativeLocalPerson {
        tree: tree.downgrade(),
    };
    let person = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
        fixture.certificate.user_public_key,
    );
    assert!(
        service
            .certificate(&fixture.room.to_string(), &person)
            .unwrap()
            == fixture.certificate,
        "the native origin must retain the exact enrolled room certificate"
    );
}

fn native_profile_request(
    host: &crate::shell::remote_actions::BrokerActionHost,
    caller: &tilo_actions::remote::Caller,
    room: &str,
    action: &str,
    arguments: &serde_json::Value,
) -> tilo_actions::remote::InvokeRequest {
    use tilo_actions::remote::{
        ActionHost, CatalogueQuery, Confirmation, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };
    let provider = tilo_actions::remote::workspace_provider(room);
    let page = ready_remote(host.query(
        caller,
        CatalogueQuery {
            provider: Some(provider.clone()),
            max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
            ..Default::default()
        },
    ))
    .unwrap();
    let descriptor = page
        .rows
        .iter()
        .find_map(|row| {
            row.action.as_ref().filter(|descriptor| {
                row.provider == provider && descriptor.address.action == action
            })
        })
        .expect("the installed native owner offers the exact governance command");
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider,
            address: descriptor.address.clone(),
        },
        argument: InvokeArgument::Json(arguments.as_object().unwrap().clone()),
        confirmation: Some(Confirmation::Confirmed),
        revision: Some(page.revision),
        fresh_binding: None,
        wait_ms: None,
        context: None,
        on_behalf_of: None,
    }
}

fn profile_action_decision(
    tree: &tilo_desktop_model::DesktopTree,
    room: &str,
    agent: &str,
    action: &str,
) -> collab_types::caps::CapDecision {
    use collab_types::caps::{ResourceRef, Verb};
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == room)
        .unwrap();
    workspace.decide_at(
        &workspace.actor_for_agent(agent),
        &ResourceRef::Action {
            app: "so.tilo.chat".into(),
            resource: "chat.person:tide".into(),
            action: action.into(),
        },
        Verb::Read,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
}

fn publish_unrelated_owner_receipt(fixture: &NativeFixture) {
    use tilo_peer_state::owner_commit::{
        CommitOperation, OwnerCommitRequest, OwnerCommitService as _,
    };
    let service = super::NativeOwnerCommitService::new(
        native_workspace_owner::test_tree_of(&fixture.owner),
        None,
    );
    let invocation = tilo_actions::InvocationId::new();
    let effect = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        invocation,
        &hex::encode(fixture.certificate.user_public_key),
        tilo_actions::ActionContext::default(),
        tilo_actions::EffectReceiptStatus::Succeeded,
        tilo_clock::unix_now_i64(),
    );
    let request = OwnerCommitRequest {
        room: fixture.room.to_string(),
        operation: CommitOperation::EffectReceipt {
            invocation: invocation.0,
            redacted: serde_json::to_vec(&effect).unwrap(),
        },
    };
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(service.commit_member_effect_receipt(request, fixture.certificate.clone()))
        .unwrap();
}

fn invoke_certified_profile_action(
    session: &crate::shell::certified_action_session::DesktopActionSession,
    actions: &collab_broker::AppActionBroker,
    room: &str,
    action: &str,
    arguments: &serde_json::Value,
) {
    use tilo_actions::remote::{
        CatalogueQuery, Confirmation, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };
    let provider = tilo_actions::remote::workspace_provider(room);
    let page = ready_remote(session.query(CatalogueQuery {
        provider: Some(provider.clone()),
        max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
        ..Default::default()
    }))
    .unwrap();
    let descriptor = page
        .rows
        .iter()
        .find_map(|row| {
            row.action.as_ref().filter(|descriptor| {
                row.provider == provider && descriptor.address.action == action
            })
        })
        .unwrap();
    let (job, _) = ready_remote(session.invoke_job(InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider,
            address: descriptor.address.clone(),
        },
        argument: InvokeArgument::Json(arguments.as_object().unwrap().clone()),
        confirmation: Some(Confirmation::Confirmed),
        revision: Some(page.revision),
        fresh_binding: None,
        wait_ms: None,
        context: None,
        on_behalf_of: None,
    }))
    .unwrap();
    wait_for_invocation(actions, job);
}

#[test]
fn native_member_terminal_audit_uses_installed_governor_and_original_retry_receipt() {
    use tilo_peer_state::owner_commit::{
        CommitOperation, OwnerCommitRequest, OwnerCommitService as _,
    };
    let fixture = fixture();
    let service = super::NativeOwnerCommitService::new(
        native_workspace_owner::test_tree_of(&fixture.owner),
        None,
    );
    let invocation = tilo_actions::InvocationId::new();
    let effect = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        invocation,
        &hex::encode(fixture.certificate.user_public_key),
        tilo_actions::ActionContext::default(),
        tilo_actions::EffectReceiptStatus::Succeeded,
        tilo_clock::unix_now_i64(),
    );
    let request = OwnerCommitRequest {
        room: fixture.room.to_string(),
        operation: CommitOperation::EffectReceipt {
            invocation: invocation.0,
            redacted: serde_json::to_vec(&effect).unwrap(),
        },
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let committed = runtime
        .block_on(
            service.commit_member_effect_receipt(request.clone(), fixture.certificate.clone()),
        )
        .unwrap();
    let persisted = std::fs::read(&fixture.path).unwrap();
    let retry = runtime
        .block_on(
            service.commit_member_effect_receipt(request.clone(), fixture.certificate.clone()),
        )
        .unwrap();
    assert_eq!(retry.binding, committed.binding);
    assert_eq!(retry.generation, committed.generation);
    assert_eq!(retry.aggregate_hash, committed.aggregate_hash);
    let mut stale = fixture.certificate.clone();
    stale.device_label = "changed caller evidence".into();
    assert!(
        runtime
            .block_on(service.commit_member_effect_receipt(request, stale))
            .is_err()
    );
    drop(service);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), persisted);
    drop(fixture);
}

#[test]
fn native_published_authorship_requires_admin_and_durably_enables_the_actual_app() {
    use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
    let fixture = fixture();
    let actions = fixture.broker.app_action_broker().unwrap();
    wait_for_membership_surface(&actions, fixture.room);
    let check = |expected| {
        fixture
            .owner
            .with_current(fixture.room, |view| {
                let now = std::time::SystemTime::now();
                view.peer()
                    .check_workspace_administration(fixture.certificate.device_public_key, now)?;
                let (_, caps, _) = view
                    .peer()
                    .permissions(fixture.certificate.device_public_key, now)?;
                let seconds = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                assert_eq!(
                    decide_capability(
                        &Actor::app("so.tilo.chat").in_room(fixture.room.to_string()),
                        &ResourceRef::Workspace,
                        Verb::Write,
                        &caps,
                        Some(seconds)
                    ),
                    expected
                );
                assert!(
                    view.peer().document("so.tilo.chat", "").is_none(),
                    "authorship must not create sample or synthetic app data"
                );
                Ok(())
            })
            .unwrap();
    };
    check(CapDecision::DeniedByRole);
    let job = actions
        .change_workspace_membership(
            &fixture.room.to_string(),
            &WorkspaceMembershipCommand::GrantAppAuthorship {
                app: "so.tilo.chat".into(),
            },
            &fixture.certificate,
        )
        .unwrap();
    let completed = job.completion.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(completed.code.is_empty(), "{}", completed.message);
    assert!(matches!(
        actions.status(job.id),
        Some(InvocationUpdate::Succeeded { .. })
    ));
    check(CapDecision::Allow);
    drop(actions);
    drop(fixture.broker);
    crate::shell::persist::wait_for_saves();
    drop(fixture.owner);
    let mut stored = crate::shell::persist::load_tree_from(&fixture.path).unwrap();
    stored.persist_path = Some(fixture.path);
    let reopened = native_workspace_owner::open(stored.shared()).unwrap();
    reopened
        .with_current(fixture.room, |view| {
            let now = std::time::SystemTime::now();
            let (_, caps, _) = view
                .peer()
                .permissions(fixture.certificate.device_public_key, now)?;
            assert_eq!(
                decide_capability(
                    &Actor::app("so.tilo.chat").in_room(fixture.room.to_string()),
                    &ResourceRef::Workspace,
                    Verb::Write,
                    &caps,
                    Some(now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs())
                ),
                CapDecision::Allow
            );
            assert!(view.peer().document("so.tilo.chat", "").is_none());
            Ok(())
        })
        .unwrap();
}

fn wait_for_membership_surface(
    actions: &tilo_action_broker::collab_broker::AppActionBroker,
    room: crate::shell::state::RoomId,
) -> tilo_action_broker::collab_broker_capnp::PublishedSurface {
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    loop {
        if let Some(surface) = actions
            .state()
            .published_surfaces()
            .into_iter()
            .find(|surface| {
                surface.route.cluster_id == room.to_string()
                    && surface.app_id == "so.tilo.desktop"
                    && surface.route.record_id == "membership"
            })
        {
            return surface;
        }
        assert!(
            Instant::now() < deadline,
            "normal native membership source was not published"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn desktop_action_session_retains_the_selected_certified_person_and_original_custody() {
    use crate::shell::search::providers::live_app_actions::LiveAppActionsProvider;
    use tilo_actions::remote::PendingPoll;

    let fixture = fixture();
    let actions = fixture.broker.app_action_broker().unwrap();
    let surface = wait_for_membership_surface(&actions, fixture.room);
    let sessions = ActionSessionSource::new(actions.clone());
    let expected = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
        fixture.certificate.user_public_key,
    );
    assert!(
        sessions
            .open(
                &fixture.room.to_string(),
                Some("display-label-is-not-authority")
            )
            .is_err(),
        "a displayed Person label cannot choose action authority"
    );
    assert!(
        sessions
            .open(&uuid::Uuid::new_v4().to_string(), Some(&expected))
            .is_err(),
        "selected identity evidence cannot be rebound to another room"
    );
    let action = surface
        .actions
        .iter()
        .find(|action| action.resource == "desktop:workspace" && action.action == "add-agent")
        .expect("membership surface publishes Add Agent");
    let provider = LiveAppActionsProvider::new(actions.clone());
    let mut pending = provider.begin_published_for_test(
        &surface,
        action,
        expected,
        &serde_json::json!({"agent":"scribe", "preset":"operator"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    let job = loop {
        match pending.poll_ready() {
            PendingPoll::Ready(Ok((job, receipt))) => {
                assert!(!receipt.invocation.0.is_nil());
                break job;
            }
            PendingPoll::Ready(Err(error)) => panic!("certified action refused: {error}"),
            PendingPoll::Abandoned => panic!("certified action admission was abandoned"),
            PendingPoll::Waiting => {
                assert!(
                    Instant::now() < deadline,
                    "certified action admission timed out"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    loop {
        match actions.status(job) {
            Some(InvocationUpdate::Succeeded { .. }) => break,
            Some(InvocationUpdate::Failed { error, .. }) => {
                panic!("certified owner action lost original custody: {error:?}")
            }
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "certified desktop action did not reach durable completion"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(sessions);
    drop(provider);
    drop(actions);
    drop(fixture);
}

pub(super) fn fixture() -> NativeFixture {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let system = home.config().join("test-system");
    home.env()
        .set("TILO_IDENTITY_ROOT", &system)
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1")
        .set(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent-tilo-test-bus",
        )
        .remove("TILO_USER_IDENTITY")
        .remove("TILO_IDENTITY_BROKER_FD");
    let identity = runtime_identity::identity_dir_from_env().unwrap();
    crate::firstboot::ensure_throwaway_test_identity(
        &identity,
        &crate::firstboot::collaboration_identity_paths(),
    )
    .unwrap();
    let room = crate::shell::state::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    let registry = tilo_identity_host::registry::IdentityRegistryStore::at(&identity);
    let selected = registry
        .load_reconciled()
        .unwrap()
        .default_identity
        .unwrap();
    registry
        .bind_workspace(&room.to_string(), &selected)
        .unwrap();
    let (key, certificate) =
        tilo_identity_host::broker::trusted_current_device(&room.to_string()).unwrap();
    assert!(!certificate.capabilities.can_act_as_host);
    let mut initial = WorkspaceTree::bootstrap_default();
    assert!(initial.set_room(initial.desktop.active, room));
    let path = home.config().join("state.json");
    initial.persist_path = Some(path.clone());
    let tree = initial.shared();
    let owner = native_workspace_owner::open(tree.clone()).unwrap();
    let _governor =
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate).unwrap();
    // The normal service factory installs NativeCertificates, not a recording
    // authority or a test PolicyTime. Its real clock must retain the original.
    let authority = NativeCertificates::default();
    authority
        .authorize_join(
            uuid::Uuid::parse_str(&room.to_string()).unwrap(),
            &certificate,
        )
        .unwrap();
    let broker =
        collab_broker::spawn_native(home.runtime().join("collab.sock"), &None, tree).unwrap();
    NativeFixture {
        home,
        owner,
        broker,
        certificate,
        room,
        path,
    }
}

#[test]
fn native_certified_agent_start_refuses_unadmitted_target_with_original_receipt() {
    let NativeFixture {
        home,
        owner,
        broker,
        certificate,
        room,
        path: _path,
    } = fixture();
    let actions = broker.app_action_broker().unwrap();
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    while !actions.state().published_surfaces().iter().any(|surface| {
        surface.live
            && surface.app_id == "so.tilo.desktop"
            && surface.route.cluster_id == room.to_string()
            && surface.route.record_id == "tasks"
            && surface
                .actions
                .iter()
                .any(|action| action.action == "start-agent")
    }) {
        assert!(
            Instant::now() < deadline,
            "native Agent startup door was not published"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let public = failed_certified_effect(
        &actions,
        room,
        &certificate,
        ("tasks", "desktop:tasks", "start-agent"),
        serde_json::json!({"agent":"not-admitted"}),
    );
    let InvocationUpdate::Failed { invocation, error } = public else {
        panic!("unadmitted Agent startup must fail")
    };
    assert_eq!(error.code, "workspace_task_refused");
    assert_eq!(
        error.message,
        "agent startup target is not a current room member"
    );
    owner
        .with_current(room, |view| {
            let receipts = tilo_desktop_model::projection::workspace_effect_receipts_of(
                view.peer().admitted_room().unwrap(),
            );
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0].invocation, invocation);
            Ok(())
        })
        .unwrap();
    drop(actions);
    drop(broker);
    drop(owner);
    drop(home);
}

#[test]
fn native_certified_local_membership_retains_original_and_settles_failed_and_successful_effects() {
    let NativeFixture {
        home,
        owner,
        broker,
        certificate,
        room,
        path,
    } = fixture();
    let discovery = broker.app_action_broker().unwrap().downgrade();
    let actions = discovery.upgrade().unwrap();
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    while !actions.state().published_surfaces().iter().any(|surface| {
        surface.live
            && surface.app_id == "so.tilo.desktop"
            && surface.route.cluster_id == room.to_string()
            && surface.route.record_id == "membership"
    }) {
        assert!(
            Instant::now() < deadline,
            "normal native membership source was not published"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for index in 0..2usize {
        let public = if index == 0 {
            failed_effect(&actions, room, &certificate)
        } else {
            let job = actions
                .change_workspace_membership(
                    &room.to_string(),
                    &WorkspaceMembershipCommand::GrantAppAuthorship {
                        app: "so.tilo.chat".into(),
                    },
                    &certificate,
                )
                .unwrap();
            let terminal = job
                .completion
                .recv_timeout(Duration::from_secs(3))
                .expect("native effect must settle after its owner receipt");
            assert!(
                terminal.code.is_empty(),
                "native author grant refused: {}",
                terminal.message
            );
            actions
                .status(job.id)
                .expect("public terminal receipt remains observable")
        };
        if index == 0 {
            assert!(matches!(public, InvocationUpdate::Failed { .. }));
        } else {
            assert!(matches!(public, InvocationUpdate::Succeeded { .. }));
        }
        let receipts = owner
            .with_current(room, |view| {
                Ok(
                    tilo_desktop_model::projection::workspace_effect_receipts_of(
                        view.peer().admitted_room().unwrap(),
                    ),
                )
            })
            .unwrap();
        assert_eq!(receipts.len(), index.checked_add(1).unwrap());
        assert!(
            receipts
                .iter()
                .any(|receipt| receipt.invocation == public.invocation())
        );
    }
    drop(actions);
    drop(broker);
    assert!(discovery.upgrade().is_err());
    crate::shell::persist::wait_for_saves();
    drop(owner);
    let mut stored = crate::shell::persist::load_tree_from(&path).unwrap();
    stored.persist_path = Some(path);
    let reopened = native_workspace_owner::open(stored.shared()).unwrap();
    reopened
        .with_current(room, |view| {
            assert_eq!(
                tilo_desktop_model::projection::workspace_effect_receipts_of(
                    view.peer().admitted_room().unwrap()
                )
                .len(),
                2
            );
            Ok(())
        })
        .unwrap();
    drop(home);
}

#[test]
fn native_published_effects_cross_hot_boundary_and_restore_cold_exact_receipt() {
    let NativeFixture {
        home,
        owner,
        broker,
        certificate,
        room,
        path,
    } = fixture();
    let discovery = broker.app_action_broker().unwrap().downgrade();
    let actions = discovery.upgrade().unwrap();
    await_membership_source(&actions, room);
    let first = failed_effect(&actions, room, &certificate);
    assert!(matches!(first, InvocationUpdate::Failed { .. }));
    let effect = owner
        .with_current(room, |view| {
            tilo_desktop_model::projection::workspace_effect_receipts_of(
                view.peer().admitted_room().unwrap(),
            )
            .into_iter()
            .find(|receipt| receipt.invocation == first.invocation())
            .ok_or_else(|| "actual native effect receipt missing".into())
        })
        .unwrap();
    let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
        room: room.to_string(),
        operation: tilo_peer_state::owner_commit::CommitOperation::EffectReceipt {
            invocation: effect.invocation.0,
            redacted: serde_json::to_vec(&effect).unwrap(),
        },
    };
    let (key, governor) =
        tilo_identity_host::broker::trusted_current_device(&room.to_string()).unwrap();
    assert_eq!(governor, certificate);
    let original = owner
        .commit_effect_receipt(&request, &governor, &key, std::time::SystemTime::now())
        .unwrap();
    for _ in 1..130 {
        assert!(matches!(
            failed_effect(&actions, room, &certificate),
            InvocationUpdate::Failed { .. }
        ));
    }
    owner
        .with_current(room, |view| {
            assert_eq!(
                tilo_desktop_model::projection::workspace_effect_receipts_of(
                    view.peer().admitted_room().unwrap()
                )
                .len(),
                130
            );
            assert!(view.peer().workspace_effect_history_root()?.is_some());
            Ok(())
        })
        .unwrap();
    drop(actions);
    drop(broker);
    assert!(discovery.upgrade().is_err());
    crate::shell::persist::wait_for_saves();
    drop(owner);
    let mut stored = crate::shell::persist::load_tree_from(&path).unwrap();
    stored.persist_path = Some(path.clone());
    let reopened = native_workspace_owner::open(stored.shared()).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let retry = reopened
        .commit_effect_receipt(&request, &governor, &key, std::time::SystemTime::now())
        .unwrap();
    assert_eq!(retry.binding, original.binding);
    assert_eq!(retry.generation, original.generation);
    assert_eq!(retry.aggregate_hash, original.aggregate_hash);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    reopened
        .with_current(room, |view| {
            let restored = tilo_desktop_model::projection::workspace_effect_receipts_of(
                view.peer().admitted_room().unwrap(),
            )
            .into_iter()
            .find(|receipt| receipt.invocation == effect.invocation)
            .ok_or_else(|| "reopened native effect receipt missing".to_string())?;
            assert_eq!(restored, effect);
            Ok(())
        })
        .unwrap();
    drop(home);
}

pub(super) fn await_membership_source(
    actions: &tilo_action_broker::collab_broker::AppActionBroker,
    room: crate::shell::state::RoomId,
) {
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    while !actions.state().published_surfaces().iter().any(|surface| {
        surface.live
            && surface.app_id == "so.tilo.desktop"
            && surface.route.cluster_id == room.to_string()
            && surface.route.record_id == "membership"
    }) {
        assert!(
            Instant::now() < deadline,
            "native membership source missing"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn failed_effect(
    actions: &tilo_action_broker::collab_broker::AppActionBroker,
    room: crate::shell::state::RoomId,
    certificate: &tilo_user_identity::DeviceCert,
) -> InvocationUpdate {
    failed_certified_effect(
        actions,
        room,
        certificate,
        ("membership", "desktop:workspace", "add-agent"),
        serde_json::json!({"agent":"scribe", "preset":"not-a-preset"}),
    )
}

fn failed_certified_effect(
    actions: &tilo_action_broker::collab_broker::AppActionBroker,
    room: crate::shell::state::RoomId,
    certificate: &tilo_user_identity::DeviceCert,
    target: (&str, &str, &str),
    arguments: serde_json::Value,
) -> InvocationUpdate {
    use ed25519_dalek::Signer as _;
    use tilo_actions::remote::{
        Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
    };
    let room = room.to_string();
    let (key, _) = tilo_identity_host::broker::trusted_current_device(&room).unwrap();
    let evidence = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.clone(),
        device_certificate: serde_json::to_vec(certificate).unwrap(),
        proof: key
            .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    let caller = Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
        .in_room(&room)
        .in_new_execution();
    let host = actions
        .certified_local_action_host(&caller, &evidence)
        .unwrap();
    let invocation = tilo_actions::InvocationId::new();
    let surfaces = actions.state().published_surfaces();
    let surface = surfaces
        .iter()
        .find(|surface| {
            surface.route.cluster_id == room
                && surface.app_id == "so.tilo.desktop"
                && surface.route.record_id == target.0
        })
        .unwrap();
    let request = InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: surface.peer_id.to_string(),
            address: tilo_actions::ActionAddress::new(target.1, target.2),
        },
        argument: InvokeArgument::Json(match arguments {
            serde_json::Value::Object(arguments) => arguments,
            _ => panic!("certified effect arguments must be an object"),
        }),
        context: None,
        confirmation: None,
        revision: None,
        fresh_binding: None,
        on_behalf_of: None,
        wait_ms: None,
    };
    let mut pending = host.invoke(&caller, request);
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    loop {
        match pending.poll_ready() {
            PendingPoll::Ready(receipt) => {
                receipt.unwrap();
                break;
            }
            PendingPoll::Abandoned => panic!("native invocation admission abandoned"),
            PendingPoll::Waiting => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let job = actions.state().remote_job(&caller, &invocation).unwrap();
    loop {
        if let Some(update) = actions.status(job)
            && matches!(update, InvocationUpdate::Failed { .. })
        {
            return update;
        }
        assert!(
            Instant::now() < deadline,
            "failed native effect did not settle its public waiter"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn native_files_home_consent_refuses_unattested_identity_without_leaving_a_grant() {
    use crate::portal::{GrantStoreHandle, PortalCaller};
    use crate::shell::state::{ResourceRef, Scope, Verb};
    const FILES: &str = crate::shell::search::providers::applications::SYSTEM_FILE_MANAGER;
    let fixture = fixture();
    let tree = native_workspace_owner::test_tree_of(&fixture.owner);
    let workspace = tree.read().desktop.active;
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    std::fs::remove_file(paths.device_cert_for_user(&fixture.certificate.user_public_key)).unwrap();
    assert!(
        tilo_identity_host::broker::trusted_current_device_public(&fixture.room.to_string())
            .is_err()
    );
    assert!(
        !crate::shell::search::providers::applications::grant_file_manager_home(
            &mut tree.write(),
            workspace,
            FILES
        )
    );
    let home = fixture.home.home().to_string_lossy().into_owned();
    assert!(
        !tree
            .read()
            .desktop
            .active_workspace()
            .unwrap()
            .capabilities
            .iter()
            .any(|cap| cap.scope == Scope::FsPath(home.clone()))
    );
    let grants = GrantStoreHandle::new(tree);
    let caller = PortalCaller::new(workspace, FILES);
    let resource = ResourceRef::FsPath(format!("{home}/source/report.txt"));
    assert!(!grants.permits(&caller, &resource, Verb::Read));
    assert!(!grants.permits(&caller, &resource, Verb::Write));
    // Keep the isolated identity/profile active through the final refusal.
    drop(fixture);
}

#[test]
fn native_files_home_consent_survives_authorship_and_revocation_does_not_resurrect() {
    use crate::portal::{GrantStoreHandle, PortalCaller};
    use crate::shell::state::{ResourceRef, Scope, Verb};
    const FILES: &str = crate::shell::search::providers::applications::SYSTEM_FILE_MANAGER;
    let fixture = fixture();
    let tree = native_workspace_owner::test_tree_of(&fixture.owner);
    let actions = fixture.broker.app_action_broker().unwrap();
    wait_for_membership_surface(&actions, fixture.room);
    let workspace = tree.read().desktop.active;
    assert!(
        crate::shell::search::providers::applications::grant_file_manager_home(
            &mut tree.write(),
            workspace,
            FILES
        )
    );
    let home = fixture.home.home().to_string_lossy().into_owned();
    let saved = tree
        .read()
        .desktop
        .active_workspace()
        .unwrap()
        .capabilities
        .iter()
        .find(|cap| cap.scope == Scope::FsPath(home.clone()))
        .unwrap()
        .clone();
    let grants = GrantStoreHandle::new(tree.clone());
    let caller = PortalCaller::new(workspace, FILES);
    let resource = ResourceRef::FsPath(format!("{home}/source/report.txt"));
    let publish = |app: &str| {
        let job = actions
            .change_workspace_membership(
                &fixture.room.to_string(),
                &WorkspaceMembershipCommand::GrantAppAuthorship { app: app.into() },
                &fixture.certificate,
            )
            .unwrap();
        let completed = job.completion.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(completed.code.is_empty(), "{}", completed.message);
        assert!(matches!(
            actions.status(job.id),
            Some(InvocationUpdate::Succeeded { .. })
        ));
    };
    publish(FILES);
    assert!(
        tree.read()
            .desktop
            .active_workspace()
            .unwrap()
            .capabilities
            .contains(&saved)
    );
    assert!(grants.permits(&caller, &resource, Verb::Read));
    assert!(grants.permits(&caller, &resource, Verb::Write));
    assert!(!grants.permits(
        &PortalCaller::new(workspace, "so.tilo.chat"),
        &resource,
        Verb::Read
    ));
    let person = crate::shell::state::local_user_id_for_room(&fixture.room);
    assert!(
        tree.write()
            .revoke_in_workspace(workspace, &person, &saved.id)
    );
    assert!(!grants.permits(&caller, &resource, Verb::Read));
    publish("so.tilo.chat");
    assert!(
        !tree
            .read()
            .desktop
            .active_workspace()
            .unwrap()
            .capabilities
            .contains(&saved)
    );
    assert!(!grants.permits(&caller, &resource, Verb::Read));
    assert!(!grants.permits(&caller, &resource, Verb::Write));
    // Keep the isolated identity/profile active through the final refusal.
    drop(fixture);
}

#[test]
fn native_files_home_consent_is_not_reconstructed_when_session_reopens() {
    use crate::portal::{GrantStoreHandle, PortalCaller};
    use crate::shell::state::{ResourceRef, Verb};
    const FILES: &str = crate::shell::search::providers::applications::SYSTEM_FILE_MANAGER;
    let fixture = fixture();
    let tree = native_workspace_owner::test_tree_of(&fixture.owner);
    let actions = fixture.broker.app_action_broker().unwrap();
    wait_for_membership_surface(&actions, fixture.room);
    let workspace = tree.read().desktop.active;
    assert!(
        crate::shell::search::providers::applications::grant_file_manager_home(
            &mut tree.write(),
            workspace,
            FILES
        )
    );
    let job = actions
        .change_workspace_membership(
            &fixture.room.to_string(),
            &WorkspaceMembershipCommand::GrantAppAuthorship { app: FILES.into() },
            &fixture.certificate,
        )
        .unwrap();
    let completed = job.completion.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(completed.code.is_empty(), "{}", completed.message);
    let resource = ResourceRef::FsPath(fixture.home.home().to_string_lossy().into_owned());
    let caller = PortalCaller::new(workspace, FILES);
    assert!(GrantStoreHandle::new(tree.clone()).permits(&caller, &resource, Verb::Read));
    drop(actions);
    drop(tree);
    drop(fixture.broker);
    crate::shell::persist::wait_for_saves();
    drop(fixture.owner);
    let mut stored = crate::shell::persist::load_tree_from(&fixture.path).unwrap();
    stored.persist_path = Some(fixture.path);
    let tree = stored.shared();
    let reopened = native_workspace_owner::open(tree.clone()).unwrap();
    reopened
        .with_current(fixture.room, |view| {
            use collab_types::caps::{Actor, CapDecision, decide_capability};
            let now = std::time::SystemTime::now();
            let (_, caps, _) = view
                .peer()
                .permissions(fixture.certificate.device_public_key, now)?;
            assert_eq!(
                decide_capability(
                    &Actor::app(FILES).in_room(fixture.room.to_string()),
                    &ResourceRef::Workspace,
                    Verb::Write,
                    &caps,
                    Some(now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs())
                ),
                CapDecision::Allow
            );
            Ok(())
        })
        .unwrap();
    let grants = GrantStoreHandle::new(tree);
    assert!(!grants.permits(&caller, &resource, Verb::Read));
    assert!(!grants.permits(&caller, &resource, Verb::Write));
    // Keep the isolated identity/profile active through the final refusal.
    drop(fixture.home);
}
