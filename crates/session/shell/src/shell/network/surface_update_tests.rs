//! A room's write that names a newer surface of an app than this device
//! has is that app's update: the device runs the app's update, the same
//! `app update` an app behind takes as it starts, and once it is in admits
//! the write, decided against the surface the updated app ships.
use super::*;
use crate::network::actor::{HostIdentity, native_ingress};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tilo_joined_room::testing::{APP, ROOM, raw_fixture};

/// The Chat surface this device has installed.
const INSTALLED: &str = "[[resources]]\nkind = \"chat\"\nlabel = \"Chat\"\nversion = 1\n";

/// The newer Chat surface the room's writer runs: it adds what the write
/// edits.
const NEWER: &str = "[[resources]]\nkind = \"chat\"\nlabel = \"Chat\"\nversion = 1\n\n\
     [[resources.fields]]\nowner = \"chat\"\npath = \"messages\"\nstored = \"value\"\n\
     title = \"Messages\"\ntype = \"String\"\n";

/// The governor's chat message `text`, after the seed `seed` of the chat,
/// naming the surface `surface`.
fn message_after(seed: &NeighbourMsg, text: &str, surface: [u8; 32]) -> NeighbourMsg {
    let NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        record_id,
        envelope,
    } = seed
    else {
        panic!("the seed is a document");
    };
    let bytes = match collab_types::DocumentMutation::decode(&envelope.payload).unwrap() {
        collab_types::DocumentMutation::Snapshot(bytes)
        | collab_types::DocumentMutation::Incremental(bytes) => bytes,
    };
    let governor = SigningKey::from_bytes(&[61; 32]);
    let mut chat = AutoCommit::load(&bytes).unwrap();
    chat.set_actor(automerge::ActorId::from(
        governor.verifying_key().to_bytes().to_vec(),
    ));
    let before = chat.get_heads();
    let messages = chat.put_object(ROOT, "messages", ObjType::Map).unwrap();
    chat.put(&messages, text, text).unwrap();
    chat.commit_edit("chat", "messages");
    let mut write = (**envelope).clone().with_surface(Some(surface));
    write.payload = collab_types::DocumentMutation::Incremental(chat.save_after(&before))
        .encode()
        .unwrap();
    write.seq = envelope.seq.checked_add(1).unwrap();
    collab_types::sign_op(&mut write, &governor).unwrap();
    NeighbourMsg::AppDocument {
        peer: *peer,
        app_id: app_id.clone(),
        cluster_id: cluster_id.clone(),
        record_id: record_id.clone(),
        envelope: Box::new(write),
    }
}

/// Whether the room's chat as the device holds it carries `text`.
fn holds(owner: &tilo_joined_room::NativeWorkspaceOwner, room: RoomId, text: &str) -> bool {
    owner
        .with_current(room, |view| Ok(view.peer().document(APP, "")))
        .unwrap()
        .is_some_and(|bytes| {
            let chat = AutoCommit::load(&bytes).unwrap();
            chat.get(ROOT, "messages")
                .unwrap()
                .is_some_and(|(_, messages)| chat.get(&messages, text).unwrap().is_some())
        })
}

#[test]
#[serial_test::serial(process_env)]
fn a_write_naming_a_newer_chat_surface_updates_chat_and_is_then_applied() {
    let apps = tempfile::tempdir().unwrap();
    let apps_root = std::fs::canonicalize(apps.path()).unwrap();
    let installed = apps_root.join("installed");
    let store = apps_root.join("store");
    crate::shell::test_support::install_signed_package(
        &installed,
        APP,
        "1.0.0",
        &[("probe", b"binary"), ("surface.toml", INSTALLED.as_bytes())],
    );
    // The newer build the store has, which `app update` installs.
    crate::shell::test_support::install_signed_package(
        &store,
        APP,
        "1.1.0",
        &[("probe", b"binary"), ("surface.toml", NEWER.as_bytes())],
    );
    let catalogue = apps_root.join("app");
    std::fs::write(
        &catalogue,
        format!(
            "#!/bin/sh\n[ \"$1\" = update ] || exit 2\n\
             cp -a '{store}/pkgs/.' '{installed}/pkgs/' && cp '{store_registry}' '{registry}'\n",
            store = store.display(),
            installed = installed.display(),
            store_registry = package_store::Layout::new(&store).registry().display(),
            registry = package_store::Layout::new(&installed).registry().display(),
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&catalogue, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", &installed);
    env.set("TILO_APP_BIN", &catalogue);
    crate::shell::decide_receipt_against_surfaces();

    let directory = tempfile::tempdir().unwrap();
    let owner = tilo_joined_room::open_file(&directory.path().join("owner.json")).unwrap();
    let (intent, bootstrap, messages, governor, now) = raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let enrollment = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    let (certificate, hint, continuity) = owner
        .with_current(room, |view| {
            Ok((
                view.device().clone(),
                view.transport_hint()?.clone(),
                view.continuity(),
            ))
        })
        .unwrap();
    let scope = Arc::new(native_ingress::Scope::new(
        native_ingress::Connect::new(owner.clone(), room).unwrap(),
        hint,
        certificate.clone(),
        continuity,
    ));
    let (mut net, _commands) = shell_network_for_test(
        WorkspaceTree::bootstrap_default().shared(),
        neighbour_wire::peer_handle(&certificate.device_public_key),
    );
    net.host_identity = Some(HostIdentity {
        device_cert: certificate,
        device_signing_key: SigningKey::from_bytes(&[63; 32]),
        workspace_device_certs: HashMap::default(),
        workspace_revocations: HashMap::default(),
        identity_paths: None,
    });
    let seed = messages
        .into_iter()
        .find(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        .expect("the chat's seed");
    net.dispatch_native_input(scope.input(governor, true, seed.clone()));
    let newer = *blake3::hash(NEWER.as_bytes()).as_bytes();
    let write = message_after(&seed, "hello from a newer chat", newer);
    let named = ui::app_surface::registered();

    net.dispatch_native_input(scope.input(governor, true, write));
    assert!(
        !holds(&owner, room, "hello from a newer chat"),
        "no surface this device has decides it yet: it is held"
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while ui::app_surface::registered() == named {
        assert!(Instant::now() < deadline, "Chat's update never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        crate::shell::app_versions::installed_surface(APP, &hex::encode(newer)).as_deref(),
        Some(NEWER.as_bytes()),
        "the write started Chat's update, which brought the surface it names"
    );
    let again = net.native_admission.next_again();
    assert_eq!(
        again.len(),
        1,
        "the held write is decided again once the update is in"
    );
    net.dispatch_native_inputs(again);
    assert!(
        holds(&owner, room, "hello from a newer chat"),
        "after Chat's update the write is applied"
    );
}
