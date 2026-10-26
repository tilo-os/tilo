//! A workspace from before rooms had hosts: a plain tree workspace no native
//! owner holds yet, which this device's host places, its apps waiting until
//! it has. Its fixture is the broker over that tree alone, with devices
//! certified by a root of its own, as every room was before rooms had hosts.

use std::os::fd::IntoRawFd;
use std::path::Path;

use ed25519_dalek::{Signer as _, SigningKey};
use tilo_action_policy::{Answer, KindSet};
use ui::collaboration::{CertifiedEndpointIdentity, CollaborationManager};

use super::{FIXTURE, FIXTURE_HANDLE, RECORD, SHORTCUTS_HANDLE, launched};

const ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x5c07_c075_0000_4000_8000_0000_0000_0001);
const ROOT_KEY: [u8; 32] = [0x5C; 32];
const BOB: &str = "person:bob";

fn room() -> crate::shell::state::RoomId {
    crate::shell::state::RoomId::parse(&ROOM.to_string()).expect("room id")
}

fn session_person() -> String {
    crate::shell::state::local_user_id_for_room(&room())
}

fn device_key(handle: &str) -> SigningKey {
    let mut seed = [0_u8; 32];
    for (index, byte) in handle.bytes().enumerate() {
        seed[index % 32] ^= byte;
    }
    seed[31] ^= 0x5C;
    SigningKey::from_bytes(&seed)
}

fn endpoint_identity(handle: &str) -> CertifiedEndpointIdentity {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let device = device_key(handle);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: handle.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).expect("root signs the device");
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: ROOM.to_string(),
        device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &ROOM.to_string(),
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("certified join encodes");
    CertifiedEndpointIdentity::new(presentation, device)
}

fn owner_principal() -> String {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    match collab_types::Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(
        root.verifying_key().to_bytes(),
    )) {
        collab_types::Principal::Person(principal) => principal,
        other => panic!("a person key names a person, not {other:?}"),
    }
}

/// The production broker over a tree whose one room the owner holds and
/// no host does, with the fixture application admitted and allowed its own
/// surface.
fn spawn_broker(
    path: &Path,
) -> (
    crate::shell::collab_broker::BrokerHandle,
    crate::shell::state::WorkspaceTreeHandle,
) {
    let owner = owner_principal();
    let mut workspace = crate::shell::state::Workspace::in_room(
        crate::shell::state::WorkspaceId(1),
        room(),
        "Drafts room",
    );
    workspace.set_owner(owner.clone());
    let by = tilo_action_policy::Subject::person(owner.clone());
    workspace.admit_agent_preset(
        FIXTURE_HANDLE,
        tilo_action_policy::roles::AgentPreset::Operator,
        &by,
    );
    let fixture = workspace.node().app(FIXTURE);
    workspace.grant(tilo_action_policy::Grant::new(
        "fixture-surface",
        tilo_action_policy::Subject::agent(FIXTURE_HANDLE),
        fixture,
        KindSet::ALL,
        Answer::Allow,
        by,
    ));
    for member in [BOB.to_string(), session_person()] {
        workspace
            .govern_as_at(
                &owner,
                tilo_action_policy::Change::AddMember {
                    person: member,
                    role: tilo_action_policy::Role::Editor,
                },
                tilo_clock::unix_now(),
            )
            .expect("the owner admits a member who may edit");
    }
    let mut tree = crate::shell::state::WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    tree.persist_path = Some(path.with_file_name("state.json"));
    let membership = tree.shared();
    let broker = crate::shell::collab_broker::spawn_native_harness(
        path.to_path_buf(),
        membership.clone(),
        [FIXTURE_HANDLE, SHORTCUTS_HANDLE]
            .iter()
            .map(|handle| device_key(handle).verifying_key().to_bytes())
            .collect(),
    )
    .expect("bind the collaboration service");
    (broker, membership)
}

/// A broker over a room that waits to be placed on this device's host, as
/// `wait_for_placement` leaves it, with the host's own identity in place.
/// An app launched into it is held, not refused, and joins once the room is
/// placed.
fn an_app_joins_once_its_room_is_placed(
    wait_for_placement: impl FnOnce(&crate::shell::state::WorkspaceTreeHandle),
) {
    let dir = tempfile::tempdir().expect("socket directory");
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let mut host_identity = tilo_test_env::ScopedEnv::new();
    host_identity
        .set("XDG_CONFIG_HOME", dir.path().join("config"))
        .set("TILO_IDENTITY_ROOT", dir.path().join("system"))
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
    crate::firstboot::ensure_throwaway_test_identity(
        &runtime_identity::identity_dir_from_env().expect("the identity directory"),
        &crate::firstboot::collaboration_identity_paths(),
    )
    .expect("the host's own identity");
    let socket = dir.path().join("collab.sock");
    let (_broker, membership) = spawn_broker(&socket);
    wait_for_placement(&membership);
    assert!(
        membership.read().pending_placements.contains(&room()),
        "the room waits to be placed on this device's host"
    );
    assert_eq!(
        membership.read().documents_unavailable(room()),
        Some("workspace-not-ready-no-documents")
    );

    let route = launched(&socket, FIXTURE, &ROOM.to_string()).into_raw_fd();
    let joining = std::thread::spawn(move || {
        CollaborationManager::for_route_fd_with_identity(
            route,
            FIXTURE,
            ROOM,
            Some(RECORD.to_string()),
            Some(FIXTURE_HANDLE.to_string()),
            endpoint_identity(FIXTURE_HANDLE),
        )
        .map(drop)
        .map_err(|error| format!("{error:?}"))
    });
    tilo_test_env::wait_until("the app's join to wait for its room", || {
        membership.room_readiness().watched()
    });
    assert!(!joining.is_finished(), "the join is held, not refused");

    // Placed, the room is held and waits no more.
    membership.write().pending_placements.remove(&room());
    joining
        .join()
        .expect("the joining thread")
        .expect("the app joins its room once it is placed");
}

/// A workspace from before rooms had hosts is this person's, and is placed
/// on this device's host as a new one is: an app launched there waits for
/// it, and is never told it cannot open documents there.
#[test]
fn an_app_in_a_workspace_from_before_hosts_waits_for_its_placement() {
    an_app_joins_once_its_room_is_placed(|membership| {
        let mut tree = membership.write();
        assert!(!tree.native_authority.requires(room()));
        tree.claim_own_unhosted_rooms(|_| owner_principal());
    });
}

/// A workspace still being placed on its host holds no records yet: an app
/// launched there waits for it, never a failure from deeper in.
#[test]
fn an_app_in_a_workspace_still_being_placed_waits_until_it_is() {
    an_app_joins_once_its_room_is_placed(|membership| {
        membership.write().pending_placements.insert(room());
    });
}
