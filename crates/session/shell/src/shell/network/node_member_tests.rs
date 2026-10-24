//! A company node's own session as a member of the workspace its node
//! governs. The founder's root certifies two devices on the node: the relay
//! (a node that hosts, governs and stewards her rooms) and the session (a
//! plain device of hers, with its own key). Her browser creates the company
//! workspace on the relay and writes a sealed record. The session asks its
//! configured host for the rooms it holds for her and enters the workspace
//! there, joins it, gets the record's channel keys from the relay's steward
//! and reads the record; a record its app writes reaches the browser the same
//! way. The relay keeps ciphertext only.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use automerge::transaction::Transactable as _;
use collab_types::{
    ContentRetention, DocumentFrame, DocumentMutation, Ed25519PublicKey, OpKind, SignedOp,
    document_artifact_id, document_attestation_id, document_channel_id,
};
use ed25519_dalek::SigningKey;
use tilo_browser_peer::{DocumentRoute, Host, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::{HostIdentity, Reach, ShellNetwork};
use crate::shell::channel_encryption::{ChannelAccess, ChannelEncryption, ChannelEncryptionConfig};
use crate::shell::channel_policy::WorkspaceChannelDirectory;
use crate::shell::collab_core::BrokerChannels;
use crate::shell::state::{RoomId, WorkspaceTree};

const NOTES: &str = "so.tilo.notes";
/// Text only the workspace's members ever see.
const PLAN: &str = "Acme Q3 plan: ship the node";
const REPLY: &str = "Drafted on the node by an agent";

fn certify(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    caps: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: caps,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// Wait until `probe` answers, within the shared hang bound.
fn until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn document(title: &str) -> Vec<u8> {
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "title", title).unwrap();
    doc.save()
}

fn holds(dir: &std::path::Path, needle: &[u8]) -> bool {
    std::fs::read_dir(dir).unwrap().any(|entry| {
        let path = entry.unwrap().path();
        if path.is_dir() {
            holds(&path, needle)
        } else {
            std::fs::read(&path)
                .unwrap_or_default()
                .windows(needle.len())
                .any(|window| window == needle)
        }
    })
}

#[test]
#[serial_test::serial]
fn a_node_session_shares_sealed_records_with_the_browser() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();

    // Ada's root, her node's relay and her browser.
    let root = key(1);
    let node_key = key(2);
    let node = certify(
        &root,
        &node_key,
        "Acme node",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let browser_key = key(3);
    let browser = certify(
        &root,
        &browser_key,
        "Ada's browser",
        DeviceCapabilities::member(),
    );
    let browser_device = browser.device_public_key;
    let relay_state = dir.path().join("relay");
    let relay = runtime
        .block_on(tilo_relay::daemon::Relay::start(
            tilo_relay::RelayConfig {
                state_root: relay_state.clone(),
                ..tilo_relay::RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: node_key,
                device_cert: node.clone(),
                revocations: RevocationList::new(),
            },
        ))
        .unwrap();
    let relay_addr = relay.node_addr();
    home.env().set(
        "TILO_COLLAB_HOST",
        serde_json::to_string(&relay_addr).unwrap(),
    );

    // The company workspace is on the node, with the browser in it, and the
    // browser writes the plan.
    let acme = runtime.block_on(async {
        let rooms = relay.rooms().unwrap();
        let acme = rooms.create("Acme").await.unwrap();
        rooms.enter(&acme.room, &browser).await.unwrap();
        acme
    });
    let ada = {
        let _runtime = runtime.enter();
        PeerHandle::start(
            PeerConfig {
                host: Host::Iroh {
                    addr: relay_addr,
                    relay_url: None,
                },
                cert: browser,
                host_cert: node.clone(),
                revocations: RevocationList::new(),
                watching: false,
            },
            Arc::new(browser_key),
            Arc::new(MemoryPeerStore::default()),
        )
    };
    ada.join(acme.workspace_id).unwrap();
    let route = |record: &str| DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: acme.room.clone(),
        record: record.into(),
    };
    let plan = document(PLAN);
    ada.publish(route("plan"), &DocumentMutation::Snapshot(plan.clone()))
        .unwrap();

    // The node's session: its own key, certified by the same root as one of
    // Ada's devices (never a host), and the relay as its workspace host.
    let session_dir = dir.path().join("session");
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(session_dir.join("state.json"));
    let tree = tree.shared();
    let session_cert = Arc::new(Mutex::new(None));
    let net = Arc::new({
        let session_cert = Arc::clone(&session_cert);
        ShellNetwork::spawn_with(
            tree.clone(),
            &session_dir.join("device-private/keypair.bin"),
            &session_dir.join("peer-sync"),
            Reach::Loopback,
            move |secret, _| {
                let key = SigningKey::from_bytes(&secret.to_bytes());
                let cert = certify(
                    &root,
                    &key,
                    "Apps on Acme node",
                    DeviceCapabilities::member(),
                );
                *session_cert.lock().unwrap() = Some((key.clone(), cert.clone()));
                Some(HostIdentity {
                    device_cert: cert,
                    device_signing_key: key,
                    workspace_device_certs: std::collections::HashMap::new(),
                    workspace_revocations: std::collections::HashMap::new(),
                    identity_paths: None,
                })
            },
        )
        .unwrap()
    });
    let (session_key, session_cert) = session_cert.lock().unwrap().clone().unwrap();
    assert!(!session_cert.capabilities.can_act_as_host);
    let _ = net.join_default_workspace();
    let mut channels = ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: session_dir.join("channels"),
            vault: channel_crypto::VaultKey::new([9; 32]),
            device_key: session_key.clone(),
        },
        Box::new(
            WorkspaceChannelDirectory::new(tree.clone(), Box::new(|_| Vec::new()))
                .with_stewards(ShellNetwork::room_stewards(&net)),
        ),
        Arc::new(Mutex::new(BrokerChannels::default())),
    )
    .unwrap();

    // Its host session is up: the session asks the node, once, for the
    // rooms it holds for Ada and enters this one there, through the host's
    // own `tilo.host` actions on its continuity workspace, and the host
    // answers the invitation it redeems.
    let room = RoomId::parse(&acme.room).unwrap();
    net.enter_configured_host_rooms(None);
    // Its workspace is made as the join starts; the room is held once the
    // replica installs into it.
    until("the session to hold the company workspace", || {
        net.tick();
        let tree = tree.read();
        (!tree.is_joining(room)
            && tree
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room() == room))
        .then_some(())
    });
    assert_eq!(
        tree.read().desktop.active_workspace().unwrap().room(),
        room,
        "the first host room is the session's active workspace"
    );
    assert!(
        tree.read().seals_records(room),
        "its records are sealed channels the apps open, not committed documents"
    );
    net.enter_configured_host_rooms(None);
    assert_eq!(
        tree.read()
            .desktop
            .workspaces
            .iter()
            .filter(|workspace| workspace.room() == room)
            .count(),
        1,
        "a room the session holds is not joined again"
    );

    // The relay's steward admits the session to the plan's channel.
    let keys_for = |channels: &mut ChannelEncryption, record: &str| {
        let channel = document_channel_id(NOTES, &acme.room, record);
        until("the steward to admit the session", || {
            net.tick();
            channels.pump(&*net, Instant::now(), tilo_clock::try_unix_now().unwrap());
            match channels.access(room, channel, Instant::now()) {
                ChannelAccess::Ready(keys) => Some(keys),
                ChannelAccess::Waiting => None,
                other => panic!("the session cannot be admitted: {other:?}"),
            }
        })
    };
    let plan_keys = keys_for(&mut channels, "plan");

    // The session reads the browser's sealed plan.
    let inbound = net.inbound_documents_handle();
    let read = until("the plan to reach the session", || {
        net.tick();
        let mut inbound = inbound.lock().unwrap();
        let index = inbound
            .iter()
            .position(|document| document.record_id == "plan")?;
        Some(inbound.remove(index))
    });
    assert_eq!(read.app_id, NOTES);
    let envelope = *read.envelope;
    let opened = plan_keys
        .open_content(
            envelope.artifact,
            envelope.encryption.as_ref().expect("the plan is sealed"),
            &envelope.payload,
        )
        .unwrap();
    assert_eq!(
        DocumentFrame::decode(&opened).unwrap(),
        DocumentFrame::Whole(DocumentMutation::Snapshot(plan))
    );

    // An app on the node writes a record: sealed under its channel's keys
    // and signed by the session's device, as the broker sends it.
    let reply_keys = keys_for(&mut channels, "reply");
    let reply = document(REPLY);
    let frame = DocumentMutation::Snapshot(reply.clone())
        .encode_frames()
        .unwrap()
        .remove(0);
    let artifact = document_artifact_id(NOTES, &acme.room, "reply");
    let (metadata, sealed) = reply_keys
        .seal_content(artifact, ContentRetention::Checkpoint, &frame)
        .unwrap();
    let mut op = SignedOp::new_unsigned_with_encryption(
        sealed,
        artifact,
        Ed25519PublicKey::from_bytes(session_cert.device_public_key),
        document_attestation_id(NOTES, &acme.room, "reply"),
        OpKind::Change,
        SystemTime::now(),
        1,
        metadata,
    );
    collab_types::sign_op(&mut op, &session_key).unwrap();
    net.publish_document(NOTES.into(), acme.room.clone(), "reply", op);

    until("the relay to retain the reply", || {
        net.tick();
        let retained = runtime
            .block_on(relay.room_records().retained(acme.workspace_id))
            .unwrap();
        retained
            .iter()
            .any(|entry| {
                matches!(
                    postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&entry.body),
                    Ok(neighbour_wire::NeighbourMsg::AppDocument { record_id, .. }) if record_id == "reply"
                )
            })
            .then_some(())
    });

    // The browser sees it.
    ada.open_route(route("reply")).unwrap();
    let mut events = Vec::new();
    let seen = until("the reply to reach the browser", || {
        net.tick();
        channels.pump(&*net, Instant::now(), tilo_clock::try_unix_now().unwrap());
        events.extend(ada.drain());
        let index = events.iter().position(|event| {
            matches!(event, PeerEvent::Document(document) if document.route.record == "reply")
        })?;
        match events.remove(index) {
            PeerEvent::Document(document) => document.mutation(),
            _ => None,
        }
    });
    assert_eq!(seen, DocumentMutation::Snapshot(reply));

    // The session administers the room it holds through the room's own
    // `tilo.workspace` governance actions on its relay session, as an agent
    // or a screen reader would: the relay issues the invitation, and
    // cancelling it there takes it off the relay's pending list.
    let target = net
        .administered_room(room)
        .expect("a relay of Ada's governs the room the session holds");
    let token = net
        .issue_relay_invitation(
            &target,
            &super::relay_rooms::RelayInvitation {
                role: crate::network::invite::InviteRole::Viewer,
                recipient: None,
                invitee: Some("Grace"),
                lifetime: 600,
            },
        )
        .expect("the relay issues the invitation through the room's action");
    assert!(!token.is_empty());
    let pending = || {
        runtime
            .block_on(relay.rooms().unwrap().pending(&acme.room, &node))
            .unwrap()
    };
    let issued = pending()
        .into_iter()
        .find(|invitation| invitation.invitee_name.as_deref() == Some("Grace"))
        .expect("the relay holds the invitation it issued");
    assert_eq!(issued.role, tilo_peer_state::join::MemberRole::Viewer);
    assert!(
        net.cancel_relay_invitation(room, issued.invite_id)
            .expect("the relay cancels it through the room's action")
    );
    assert!(
        !pending()
            .iter()
            .any(|invitation| invitation.invite_id == issued.invite_id),
        "a cancelled invitation is no longer pending at the relay"
    );

    // The session, a device of Ada's that does not host, saves a shortcut
    // into the room its relay hosts: signed here as Ada's, committed there,
    // and back in the room as the session holds it.
    let ada_person = hex::encode(session_cert.user_public_key);
    let shortcut: tilo_automation::Shortcut = serde_json::from_value(serde_json::json!({
        "id": "standup",
        "name": "Standup notes",
        "steps": [],
        "saved_by": ada_person,
    }))
    .unwrap();
    let saving = runtime.spawn(
        crate::shell::broker_services::NativeShortcutLibrary::commit(
            tree.clone(),
            Some(Arc::clone(&net)),
            room,
            ada_person.clone(),
            tilo_automation::ShortcutChange::Save(Box::new(shortcut)),
        ),
    );
    until("the room's host to commit the shortcut", || {
        net.tick();
        saving.is_finished().then_some(())
    });
    runtime
        .block_on(saving)
        .unwrap()
        .expect("the room's host commits the session's shortcut");
    let held = until("the session to hold the saved shortcut", || {
        net.tick();
        tree.read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)?
            .shortcuts
            .iter()
            .find(|shortcut| shortcut.id == "standup")
            .cloned()
    });
    assert_eq!(
        tilo_peer_state::room::proven_saver(
            &acme.room,
            &held,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        ),
        Some(ada_person),
        "saved with the session device's proof"
    );

    // The browser announces no identity to the session, yet its actions
    // reach it as the room's governance admitted it: asking for the
    // workspace's catalogue (the node's applications among it).
    let ask = tilo_actions::remote::RemoteActionFrame::new(
        tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
            correlation: 7,
            query: tilo_actions::remote::CatalogueQuery::default(),
        },
    )
    .encode()
    .unwrap();
    ada.send_remote_action(acme.workspace_id, session_cert.device_public_key, ask)
        .unwrap();
    let actions = net.inbound_remote_actions_handle();
    let request = until("the browser's action to reach the session", || {
        net.tick();
        actions.lock().unwrap().pop()
    });
    assert!(matches!(
        request.message,
        tilo_actions::remote::RemoteActionMessage::CatalogueRequest { correlation: 7, .. }
    ));
    assert_eq!(
        request.from.as_bytes(),
        &browser_device,
        "answered to the device the relay authenticated"
    );

    assert!(
        net.peer_identities
            .get(&crate::shell::state::PeerOrigin::peer(&request.from))
            .is_none(),
        "the browser has sent no legacy identity hello"
    );
    let presence = collab_types::PeerPresence {
        key: postcard::to_stdvec(&ui::presence::PresenceKey::Workspace {
            id: acme.room.clone(),
        })
        .unwrap(),
        payload: collab_types::PeerPresencePayload::Arrived,
    };
    let publisher = neighbour_wire::peer_handle(&browser_device);
    let publish_presence = |topic, endpoint, publisher, cluster| {
        net.dispatch_neighbour_msg(
            topic,
            endpoint,
            None,
            false,
            neighbour_wire::NeighbourMsg::AppPresenceV2 {
                peer: publisher,
                app_id: NOTES.into(),
                cluster_id: cluster,
                presence: presence.clone(),
                colour: None,
            },
        );
    };
    let inbound = net.inbound_app_presence_handle();
    inbound.lock().unwrap().clear();
    publish_presence(
        acme.workspace_id,
        request.from,
        publisher,
        acme.room.clone(),
    );
    let observed = inbound.lock().unwrap().pop().expect("admitted presence");
    assert_eq!(observed.peer, publisher);
    assert_eq!(observed.cluster_id, acme.room);
    assert_eq!(observed.presence, presence);
    publish_presence(
        acme.workspace_id,
        request.from,
        publisher + 1,
        acme.room.clone(),
    );
    let stranger = iroh::SecretKey::from_bytes(&[0x71; 32]).public();
    publish_presence(
        acme.workspace_id,
        stranger,
        neighbour_wire::peer_handle(stranger.as_bytes()),
        acme.room.clone(),
    );
    publish_presence(
        acme.workspace_id,
        request.from,
        publisher,
        RoomId::new().to_string(),
    );
    assert!(
        inbound.lock().unwrap().is_empty(),
        "spoofed or cross-room presence was admitted"
    );

    // The relay carried both only as ciphertext.
    assert!(!holds(&relay_state, PLAN.as_bytes()));
    assert!(!holds(&relay_state, REPLY.as_bytes()));

    ada.close();
    drop(net);
    runtime.block_on(relay.shutdown());
}
