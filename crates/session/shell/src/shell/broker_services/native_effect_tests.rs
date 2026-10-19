//! Real native clock, certified local action, and durable terminal owner barrier.
#[path = "native_checkpoint_tests.rs"]
mod checkpoints;
use crate::shell::{
    broker_services::NativeCertificates, certified_action_session::ActionSessionSource,
    collab_broker, native_workspace_owner, state::WorkspaceTree,
};
use std::time::{Duration, Instant};
use tilo_action_broker::peer_actions::CertificateAuthority;
use tilo_actions::InvocationUpdate;
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

struct NativeFixture {
    governor: tilo_joined_room::testing::governor::TestGovernor,
    home: tilo_test_env::TestHome,
    owner: tilo_joined_room::NativeWorkspaceOwner,
    broker: collab_broker::BrokerHandle,
    certificate: tilo_user_identity::DeviceCert,
    room: crate::shell::state::RoomId,
    path: std::path::PathBuf,
}

#[test]
fn native_member_terminal_audit_uses_installed_governor_and_original_retry_receipt() {
    use tilo_owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService as _};
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
    let fixture = fixture();
    let actions = fixture.broker.app_action_broker().unwrap();
    wait_for_membership_surface(&actions, fixture.room);
    let check = |expected| {
        fixture
            .owner
            .with_current(fixture.room, |view| {
                let now = std::time::SystemTime::now();
                view.peer()
                    .workspace()
                    .check_workspace_administration(fixture.certificate.device_public_key, now)?;
                let member = view
                    .peer()
                    .permissions(fixture.certificate.device_public_key, now)?;
                assert_eq!(
                    member
                        .policy
                        .app_authors("so.tilo.chat", &tilo_action_policy::Context::at_time(now)),
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
    check(false);
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
    check(true);
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
            let member = view
                .peer()
                .permissions(fixture.certificate.device_public_key, now)?;
            assert!(
                member
                    .policy
                    .app_authors("so.tilo.chat", &tilo_action_policy::Context::at_time(now))
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

#[test]
fn native_legacy_facade_retains_certified_job_ownership_across_clones() {
    use tilo_actions::remote::{
        ActionHost as _, Caller, CatalogueQuery, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };

    let fixture = fixture();
    let broker = fixture.broker.app_action_broker().unwrap();
    let surface = wait_for_membership_surface(&broker, fixture.room);
    let person = hex::encode(fixture.certificate.user_public_key);
    let caller = Caller::local_person(&person)
        .in_room(fixture.room.to_string())
        .in_new_execution();
    let host = crate::shell::remote_actions::BrokerActionHost::new(broker.clone());
    let page = host
        .query(
            &caller,
            CatalogueQuery {
                provider: Some(surface.peer_id.to_string()),
                ..Default::default()
            },
        )
        .wait_timeout(Duration::from_secs(3))
        .ok()
        .flatten()
        .expect("catalogue resolves")
        .unwrap();
    assert!(
        !page.rows.is_empty(),
        "the native facade lists after checking certified seat custody"
    );
    let seat = Caller::local_person(&person).in_room(fixture.room.to_string());
    assert!(
        host.query(&seat, CatalogueQuery::default())
            .wait_timeout(Duration::from_secs(3))
            .ok()
            .flatten()
            .expect("a native seat without a transport lease resolves")
            .is_ok()
    );
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: surface.peer_id.to_string(),
            address: tilo_actions::ActionAddress::new("desktop:workspace", "add-agent"),
        },
        argument: InvokeArgument::Json(
            serde_json::json!({"agent":"scribe", "preset":"operator"})
                .as_object()
                .unwrap()
                .clone(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        appearance: None,
        wait_ms: None,
    };
    let (job, receipt) = host
        .invoke_job(&caller, request.clone())
        .wait_timeout(Duration::from_secs(3))
        .ok()
        .flatten()
        .expect("admission resolves")
        .unwrap();
    let cloned = host.clone();
    let retry = cloned
        .invoke_job(&caller, request.clone())
        .wait_timeout(Duration::from_secs(3))
        .ok()
        .flatten()
        .expect("retry resolves")
        .unwrap();
    assert_eq!(
        retry.0, job,
        "a cloned facade must reuse the original certified execution"
    );
    assert_eq!(retry.1.invocation, receipt.invocation);
    assert!(cloned.observe(&caller, &request.invocation).is_some());
    let separate = Caller::local_person(&person)
        .in_room(fixture.room.to_string())
        .in_new_execution();
    assert!(cloned.observe(&separate, &request.invocation).is_none());
    // A separately opened certified owner sees this job. Actor-text admission
    // would not have produced certificate-owned lifecycle evidence.
    let lifecycle = ActionSessionSource::new(broker)
        .open(&fixture.room.to_string(), Some(&person))
        .unwrap()
        .lifecycle()
        .unwrap();
    assert!(lifecycle.status(job).is_some());
    caller.retire_execution();
    assert!(cloned.observe(&caller, &request.invocation).is_none());
    let fresh = |request: &InvokeRequest| InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        ..request.clone()
    };
    let (forged_request, unscoped_request) = (fresh(&request), fresh(&request));
    assert!(
        cloned
            .invoke(&caller, request)
            .wait_timeout(Duration::from_secs(3))
            .ok()
            .flatten()
            .expect("retired execution refusal resolves")
            .is_err()
    );
    // Discovery is ordinary visibility and never resolves the seat's
    // identity; admission is where a displayed Person must be the seat's
    // certified one. Neither a forged label nor a Person without a room
    // acquires native custody.
    let forged =
        Caller::local_person("display-label-is-not-authority").in_room(fixture.room.to_string());
    assert!(
        host.invoke(&forged, forged_request)
            .wait_timeout(Duration::from_secs(3))
            .ok()
            .flatten()
            .expect("forged person refusal resolves")
            .is_err()
    );
    let unscoped = Caller::local_person(&person);
    assert!(
        host.invoke(&unscoped, unscoped_request)
            .wait_timeout(Duration::from_secs(3))
            .ok()
            .flatten()
            .expect("unscoped person refusal resolves")
            .is_err()
    );
}

#[test]
fn native_facade_admits_workspace_actions_under_certified_ownership() {
    use tilo_actions::remote::{
        ActionHost as _, Caller, CatalogueQuery, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };
    let fixture = fixture();
    let broker = fixture.broker.app_action_broker().unwrap();
    wait_for_membership_surface(&broker, fixture.room);
    let person = hex::encode(fixture.certificate.user_public_key);
    let caller = Caller::local_person(&person)
        .in_room(fixture.room.to_string())
        .in_new_execution();
    let host = crate::shell::remote_actions::BrokerActionHost::new(broker.clone());
    // Native governance, the shortcut library and keyboard settings stay in
    // the native catalogue, and invoking them retains the seat's signed
    // admission like every other native action.
    let workspace_query = CatalogueQuery {
        provider: Some(tilo_actions::remote::workspace_provider(
            &fixture.room.to_string(),
        )),
        max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
        ..Default::default()
    };
    let legacy_page = broker
        .state()
        .action_query(&caller, &workspace_query)
        .wait_timeout(Duration::from_secs(3))
        .ok()
        .flatten()
        .unwrap()
        .unwrap();
    assert!(legacy_page.rows.iter().any(|row| {
        row.action
            .as_ref()
            .is_some_and(|action| action.address.action == "grant")
    }));
    let projected_page = host
        .query(&caller, workspace_query)
        .wait_timeout(Duration::from_secs(3))
        .ok()
        .flatten()
        .unwrap()
        .unwrap();
    let addresses = |page: &tilo_actions::remote::CataloguePage| {
        page.rows
            .iter()
            .filter_map(|row| row.action.as_ref().map(|action| action.address.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(addresses(&projected_page), addresses(&legacy_page));
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(&fixture.room.to_string()),
            address: tilo_actions::ActionAddress::new("shortcuts", "list"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        appearance: None,
        wait_ms: None,
    };
    let (job, _) = host
        .invoke_job(&caller, request.clone())
        .wait_timeout(Duration::from_secs(3))
        .ok()
        .flatten()
        .unwrap()
        .unwrap();
    assert!(host.observe(&caller, &request.invocation).is_some());
    let lifecycle = ActionSessionSource::new(broker)
        .open(&fixture.room.to_string(), Some(&person))
        .unwrap()
        .lifecycle()
        .unwrap();
    assert!(
        lifecycle.status(job).is_some(),
        "workspace services keep the native seat's certified ownership"
    );
}

fn fixture() -> NativeFixture {
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
    let governor =
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
        governor,
        home,
        owner,
        broker,
        certificate,
        room,
        path,
    }
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
        ..
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
        ..
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
    let request = tilo_owner_commit::OwnerCommitRequest {
        room: room.to_string(),
        operation: tilo_owner_commit::CommitOperation::EffectReceipt {
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
            assert!(
                view.peer()
                    .workspace()
                    .workspace_effect_history_root()?
                    .is_some()
            );
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

fn await_membership_source(
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
                && surface.route.record_id == "membership"
        })
        .unwrap();
    let request = InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: surface.peer_id.to_string(),
            address: tilo_actions::ActionAddress::new("desktop:workspace", "add-agent"),
        },
        argument: InvokeArgument::Json(
            serde_json::json!({"agent":"scribe", "preset":"not-a-preset"})
                .as_object()
                .unwrap()
                .clone(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
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
    use crate::shell::state::{Access, ResourceRef, Scope};
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
            .sandbox
            .iter()
            .any(|cap| cap.scope == Scope::FsPath(home.clone()))
    );
    let grants = GrantStoreHandle::new(tree);
    let caller = PortalCaller::new(workspace, FILES);
    let resource = ResourceRef::FsPath(format!("{home}/source/report.txt"));
    assert!(!grants.permits(&caller, &resource, Access::Read));
    assert!(!grants.permits(&caller, &resource, Access::Write));
    // Keep the isolated identity/profile active through the final refusal.
    drop(fixture);
}

#[test]
fn native_files_home_consent_survives_authorship_and_revocation_does_not_resurrect() {
    use crate::portal::{GrantStoreHandle, PortalCaller};
    use crate::shell::state::{Access, ResourceRef, Scope};
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
        .sandbox
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
            .sandbox
            .contains(&saved)
    );
    assert!(grants.permits(&caller, &resource, Access::Read));
    assert!(grants.permits(&caller, &resource, Access::Write));
    assert!(!grants.permits(
        &PortalCaller::new(workspace, "so.tilo.chat"),
        &resource,
        Access::Read
    ));
    let person = crate::shell::state::local_user_id_for_room(&fixture.room);
    assert!(
        tree.write()
            .revoke_sandbox_in_workspace(workspace, &person, &saved.id)
    );
    assert!(!grants.permits(&caller, &resource, Access::Read));
    publish("so.tilo.chat");
    assert!(
        !tree
            .read()
            .desktop
            .active_workspace()
            .unwrap()
            .sandbox
            .contains(&saved)
    );
    assert!(!grants.permits(&caller, &resource, Access::Read));
    assert!(!grants.permits(&caller, &resource, Access::Write));
    // Keep the isolated identity/profile active through the final refusal.
    drop(fixture);
}

/// A document the person opens from Files is read by the app that owns its
/// type under a session grant this device issued. The room's next signed
/// projection must keep that read: withdrawing it interrupts the app that
/// was just handed the document, which is how every file opened from Files
/// closed its app again a moment after it appeared.
#[test]
fn a_document_opened_in_its_app_keeps_its_read_across_the_rooms_projection() {
    use crate::portal::{GrantStoreHandle, PortalCaller};
    use crate::shell::state::{Access, ResourceRef};
    const CODE: &str = "so.tilo.code-editor";
    let fixture = fixture();
    let tree = native_workspace_owner::test_tree_of(&fixture.owner);
    let actions = fixture.broker.app_action_broker().unwrap();
    wait_for_membership_surface(&actions, fixture.room);
    let workspace = tree.read().desktop.active;
    let script = fixture.home.home().join("greet.py");
    std::fs::write(&script, "def greet():\n    pass\n").unwrap();
    let chrome = crate::shell::chrome_state::ChromeState::new_with_handle(tree.clone());
    crate::shell::doc_open::grant_read_for_open(CODE, &script, &chrome).unwrap();
    let read = tree
        .read()
        .desktop
        .active_workspace()
        .unwrap()
        .sandbox
        .iter()
        .find(|cap| cap.scope == crate::shell::state::Scope::FsPath(script.display().to_string()))
        .unwrap()
        .clone();

    let job = actions
        .change_workspace_membership(
            &fixture.room.to_string(),
            &WorkspaceMembershipCommand::GrantAppAuthorship { app: CODE.into() },
            &fixture.certificate,
        )
        .unwrap();
    let completed = job.completion.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(completed.code.is_empty(), "{}", completed.message);
    drop(actions);

    assert!(
        tree.read()
            .desktop
            .active_workspace()
            .unwrap()
            .sandbox
            .contains(&read),
        "the projection kept the document's read"
    );
    let grants = GrantStoreHandle::new(tree);
    let resource = ResourceRef::FsPath(script.display().to_string());
    assert!(grants.permits(&PortalCaller::new(workspace, CODE), &resource, Access::Read));
    assert!(
        !grants.permits(
            &PortalCaller::new(workspace, CODE),
            &resource,
            Access::Write
        ),
        "opening a document hands over its read, nothing more"
    );
    assert!(!grants.permits(
        &PortalCaller::new(workspace, "so.tilo.chat"),
        &resource,
        Access::Read
    ));
    drop(fixture);
}

#[test]
fn native_files_home_consent_is_not_reconstructed_when_session_reopens() {
    use crate::portal::{GrantStoreHandle, PortalCaller};
    use crate::shell::state::{Access, ResourceRef};
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
    assert!(GrantStoreHandle::new(tree.clone()).permits(&caller, &resource, Access::Read));
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
            let now = std::time::SystemTime::now();
            let member = view
                .peer()
                .permissions(fixture.certificate.device_public_key, now)?;
            assert!(
                member
                    .policy
                    .app_authors(FILES, &tilo_action_policy::Context::at_time(now))
            );
            Ok(())
        })
        .unwrap();
    let grants = GrantStoreHandle::new(tree);
    assert!(!grants.permits(&caller, &resource, Access::Read));
    assert!(!grants.permits(&caller, &resource, Access::Write));
    // Keep the isolated identity/profile active through the final refusal.
    drop(fixture.home);
}
