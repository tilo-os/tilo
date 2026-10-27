use super::*;
use crate::network::actor::{HostIdentity, native_ingress};
use ed25519_dalek::SigningKey;
use std::collections::HashMap;
use tilo_joined_room::testing::{APP, ROOM, raw_fixture};

#[test]
fn native_replay_yields_between_signed_admissions_without_reordering_control() {
    let directory = tempfile::tempdir().unwrap();
    let owner = tilo_joined_room::open_file(&directory.path().join("owner.json")).unwrap();
    let (intent, bootstrap, messages, governor, now) = raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let enrollment = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    let certificate = owner
        .with_current(room, |view| {
            assert!(view.peer().document(APP, "").is_none());
            Ok(view.device().clone())
        })
        .unwrap();
    let message = messages
        .into_iter()
        .find(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        .expect("actual signed fixture document");
    let tree = WorkspaceTree::bootstrap_default().shared();
    let (mut net, mut commands) = shell_network_for_test(
        tree,
        neighbour_wire::peer_handle(&certificate.device_public_key),
    );
    net.host_identity = Some(HostIdentity {
        device_cert: certificate,
        device_signing_key: SigningKey::from_bytes(&[63; 32]),
        workspace_device_certs: HashMap::default(),
        workspace_revocations: HashMap::default(),
        identity_paths: None,
    });
    net.event_tx
        .try_send(NetEvent::NativeInput(signed_input(
            &owner, room, governor, &message,
        )))
        .unwrap();
    net.event_tx
        .try_send(NetEvent::PeerLeft {
            ws: DEFAULT_WS_TOPIC,
            peer: iroh::SecretKey::from_bytes(&[42; 32]).public(),
            cause: PeerDepartureCause::LivenessTimedOut,
            membership_epoch: None,
        })
        .unwrap();
    net.tick();
    owner
        .with_current(room, |view| {
            assert!(
                view.peer().document(APP, "").is_some(),
                "the first actual signed admission must complete"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        queued(&net),
        1,
        "durable replay must yield after one native admission"
    );
    assert_eq!(take_native_publications(&mut commands), 1);
    assert!(
        net.peer_departures().is_empty(),
        "later control must preserve FIFO order"
    );
    assert_eq!(net.inbound_documents.lock().unwrap().len(), 1);
    net.tick();
    assert_eq!(net.peer_departures().len(), 1);
    assert_eq!(queued(&net), 0);
    // The first document can change continuity. Reconnect under its actual
    // current pin rather than proving only that stale input is rejected.
    net.event_tx
        .try_send(NetEvent::NativeInput(signed_input(
            &owner, room, governor, &message,
        )))
        .unwrap();
    net.event_tx
        .try_send(NetEvent::PeerLeft {
            ws: DEFAULT_WS_TOPIC,
            peer: iroh::SecretKey::from_bytes(&[43; 32]).public(),
            cause: PeerDepartureCause::LivenessTimedOut,
            membership_epoch: None,
        })
        .unwrap();
    net.tick();
    assert_eq!(net.inbound_documents.lock().unwrap().len(), 2);
    assert_eq!(
        take_native_publications(&mut commands),
        0,
        "an unchanged signed replay must not schedule recovery reconciliation"
    );
    assert_eq!(queued(&net), 1);
    assert_eq!(net.peer_departures().len(), 1);
    net.tick();
    assert_eq!(net.peer_departures().len(), 2);
    assert_eq!(queued(&net), 0);
}

/// The events still to handle: those queued, and the one a batch of
/// admissions carried over ahead of them.
fn queued(net: &ShellNetwork) -> usize {
    net.event_rx
        .lock()
        .unwrap()
        .len()
        .saturating_add(usize::from(net.native_admission.carries()))
}

fn take_native_publications(commands: &mut mpsc::Receiver<NetCmd>) -> usize {
    let mut publications = 0usize;
    while let Ok(command) = commands.try_recv() {
        if matches!(command, NetCmd::NativePublish { .. }) {
            publications = publications.checked_add(1).expect("bounded command queue");
        }
    }
    publications
}

fn signed_input(
    owner: &tilo_joined_room::NativeWorkspaceOwner,
    room: RoomId,
    governor: [u8; 32],
    message: &NeighbourMsg,
) -> native_ingress::Input {
    let (certificate, hint, continuity) = owner
        .with_current(room, |view| {
            Ok((
                view.device().clone(),
                view.transport_hint()?.clone(),
                view.continuity(),
            ))
        })
        .unwrap();
    Arc::new(native_ingress::Scope::new(
        native_ingress::Connect::new(owner.clone(), room).unwrap(),
        hint,
        certificate,
        continuity,
    ))
    .input(governor, true, message.clone())
}
