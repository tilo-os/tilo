//! Real native clock, certified local action, and durable terminal owner barrier.
use crate::shell::{
    broker_services::NativeCertificates, collab_broker, native_workspace_owner,
    state::WorkspaceTree,
};
use std::time::{Duration, Instant};
use tilo_action_broker::peer_actions::CertificateAuthority;
use tilo_actions::InvocationUpdate;
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

struct NativeFixture {
    home: tilo_test_env::TestHome,
    owner: tilo_joined_room::NativeWorkspaceOwner,
    broker: collab_broker::BrokerHandle,
    certificate: tilo_user_identity::DeviceCert,
    room: crate::shell::state::RoomId,
    path: std::path::PathBuf,
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
