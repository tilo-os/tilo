//! Every write in a sealed room is encrypted under its record channel's
//! keys, from every app, in process or not. A write an app cannot encrypt
//! is refused on the device that made it, and says so: the room's host
//! never receives a plaintext write.

use std::os::fd::IntoRawFd as _;
use std::path::Path;
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use tilo_user_identity::DeviceCapabilities;
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabRequest, CollaborationManager, CollaborationState,
};

use super::cold_reads::CHAT;
use super::{Device, Person, RoomId, Wakes, hosted_workspace, scratch, until};

impl Device {
    /// A connection of `app` in `room` to this device's broker at `socket`,
    /// made as the launcher makes an app's, presenting `identity`.
    fn connect_app_as(
        &self,
        socket: &Path,
        app: &str,
        room: RoomId,
        identity: CertifiedEndpointIdentity,
    ) -> CollaborationManager {
        crate::shell::launcher::register_in_process_launch(app, &room.to_string());
        let mut stream =
            std::os::unix::net::UnixStream::connect(socket).expect("connect to the broker");
        launcher::services::write_collab_handoff(&mut stream, app, &room.to_string())
            .expect("hand the connection over as the launcher does");
        CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            app,
            room.to_string().parse().expect("a room is a UUID"),
            None,
            Some(hex::encode(self.cert().device_public_key)),
            identity,
        )
        .expect("start the collaboration client")
    }

    /// How many plaintext writes of `app` this device's own host retains in
    /// `room`.
    fn retained_plaintext(&self, room: RoomId, app: &str) -> usize {
        let net = self.net();
        let host = net.room_host().expect("the device runs its host");
        let workspace = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
        let records = host.room_records();
        net.wait_on(async move { records.retained(workspace).await })
            .and_then(std::convert::identity)
            .map_or(0, |bodies| {
                bodies
                    .iter()
                    .filter(|body| {
                        matches!(
                            postcard::from_bytes::<crate::network::protocol::NeighbourMsg>(&body.body),
                            Ok(crate::network::protocol::NeighbourMsg::AppDocument { app_id, envelope, .. })
                                if app_id == app && envelope.encryption.is_none()
                        )
                    })
                    .count()
            })
    }
}

/// Wait until `manager`'s connection to `device`'s broker is up.
fn until_connected(device: &Device, manager: &mut CollaborationManager) {
    until(&[device], "the app's connection to the broker", || {
        let _ = manager.poll();
        matches!(manager.state(), CollaborationState::Ready).then_some(())
    });
}

/// Send `request` on `manager` and wait for how its publication ended: the
/// refusal, as the app is told it. A write that was published fails.
fn refusal(device: &Device, manager: &mut CollaborationManager, request: CollabRequest) -> String {
    assert!(manager.send(request), "the app queued its write");
    let receipt = manager
        .begin_document_publication_barrier()
        .expect("a publication barrier on a connected app");
    until(&[device], "the write's publication to end", || {
        let _ = manager.poll();
        match receipt.try_complete() {
            Ok(None) => None,
            Ok(Some(())) => Some(None),
            Err(error) => Some(Some(format!("{error:?}"))),
        }
    })
    .unwrap_or_else(|| panic!("{} published a write into the sealed room", device.label))
}

#[test]
#[serial_test::serial]
fn a_sealed_room_never_receives_a_plaintext_write_from_an_in_process_app() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(83);
    home.env().set("TILO_USER_ID", ada.id());
    let mut desktop = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A61,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&desktop, &[], "Projects");
    desktop.open_app_first(room, CHAT);
    let socket = dir.path().join("collab.sock");
    let broker = desktop.start_broker(&socket);

    let mut document = automerge::AutoCommit::new();
    document
        .put(automerge::ROOT, "title", "written in the clear")
        .unwrap();
    // A change an app makes names the action that made it.
    document.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    );
    let changes = document.save_after(&[]);

    // An app in this process with no channel keys to encrypt under: its
    // write is refused before it leaves the app, and the app is told why.
    let mut app =
        desktop.connect_app_as(&socket, CHAT, room, desktop.keyless_endpoint_identity(room));
    until_connected(&desktop, &mut app);
    let refused = refusal(
        &desktop,
        &mut app,
        CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Incremental(changes.clone()),
            authorization: None,
        },
    );
    assert!(
        refused.contains("seals every write"),
        "the app refuses the write it cannot encrypt, and says why; it was told: {refused}"
    );
    drop(app);

    // An app that signs an unencrypted op itself: the device's broker
    // refuses it, and the app is told why.
    let mut app =
        desktop.connect_app_as(&socket, CHAT, room, desktop.keyless_endpoint_identity(room));
    until_connected(&desktop, &mut app);
    let key = desktop.key();
    let room_id = room.to_string();
    let frame = collab_types::DocumentMutation::Incremental(changes)
        .encode_frames()
        .unwrap()
        .remove(0);
    let now = std::time::SystemTime::now();
    let mut op = collab_types::SignedOp::new_unsigned(
        frame,
        collab_types::document_artifact_id(CHAT, &room_id, ""),
        collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        collab_types::document_attestation_id(CHAT, &room_id, ""),
        collab_types::OpKind::Change,
        now,
        collab_types::next_unordered_document_sequence(now),
    );
    collab_types::sign_op(&mut op, &key).unwrap();
    let refused = refusal(
        &desktop,
        &mut app,
        CollabRequest::SignedOperation {
            envelope: Box::new(op),
        },
    );
    assert!(
        refused.contains("seals every write"),
        "the broker refuses an unencrypted write to the sealed room, and says why; the app was told: {refused}"
    );
    drop(app);

    assert_eq!(
        desktop.retained_plaintext(room, CHAT),
        0,
        "the room's host received a plaintext write"
    );
    drop(broker);
    drop(desktop);
}
