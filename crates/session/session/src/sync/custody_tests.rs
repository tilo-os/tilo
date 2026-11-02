//! What an app writes on one device reaches the room's other devices: the
//! broker commits it to this device's owner, the canonical copy of the room
//! here, and the owner's outgoing custody leaves on the plane's session with
//! the room's host, which hands it to every other member.
//!
//! Two sessions' planes on this machine's loopback, each with its own
//! engine, join one room governed by a host on this machine whose
//! certificate takes part in no channels: the room's records are documents
//! the owner commits.

tilo_automerge_isolate::test_worker!();

use std::collections::HashMap;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_desktop_model::{RoomId, WorkspaceId};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::{Client, Enrolling, OwnPresence};
use crate::channels::ChannelLink;
use crate::engine::Engine;
use crate::fabric::MediaContinuity;

/// `key`'s certificate as a device of the person whose root key is `root`.
fn certified(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities,
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("a certified device");
    cert
}

/// A host on this machine governing one room, certified so that it takes
/// part in no channels: what its members record is committed in the clear.
struct Host {
    relay: Option<tilo_relay::daemon::Relay>,
    rooms: tilo_relay::rooms::Rooms,
    cert: DeviceCert,
    room: String,
    runtime: tokio::runtime::Runtime,
    _state: tempfile::TempDir,
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Some(relay) = self.relay.take() {
            self.runtime.block_on(relay.shutdown());
        }
    }
}

impl Host {
    fn start() -> Self {
        let state = tempfile::tempdir().expect("the host's directory");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("the host's runtime");
        let owner_key = SigningKey::from_bytes(&[0x71; 32]);
        let host_key = SigningKey::from_bytes(&[0x72; 32]);
        let cert = certified(
            &owner_key,
            &host_key,
            "The notes host",
            DeviceCapabilities {
                can_own_workspaces: false,
                ..tilo_user_identity::cloud_enrollment::host_capabilities()
            },
        );
        let (relay, rooms, room) = runtime.block_on(async {
            let relay = tilo_relay::daemon::Relay::start(
                tilo_relay::RelayConfig {
                    state_root: state.path().join("host"),
                    ..tilo_relay::RelayConfig::default()
                },
                tilo_relay::daemon::RelayIdentity {
                    user_public_key: owner_key.verifying_key().to_bytes(),
                    device_signing_key: host_key.clone(),
                    device_cert: cert.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                },
            )
            .await
            .expect("the host starts");
            let rooms = relay.rooms().expect("the host governs rooms").clone();
            let room = rooms.create("Notes").await.expect("the room").room;
            (relay, rooms, room)
        });
        Self {
            relay: Some(relay),
            rooms,
            cert,
            room,
            runtime,
            _state: state,
        }
    }

    /// An invitation to the room that only `person`'s devices redeem.
    fn invitation(&self, person: [u8; 32], request: u8) -> String {
        let issued = self
            .runtime
            .block_on(self.rooms.issue(
                &self.room,
                &self.cert,
                tilo_workspace_join::invitations::IssueInvitationRequest {
                    request_id: [request; 32],
                    role: tilo_workspace_join::MemberRole::Editor,
                    recipient_user_public_key: Some(person),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                    inviter_name: None,
                },
            ))
            .expect("an invitation to the room");
        tilo_workspace_join::encode_invitation_text(&issued.signed_invitation)
    }
}

/// One session's side of the room: its engine, and its plane.
struct Device {
    engine: std::sync::Arc<Engine>,
    plane: Client,
    _state: tempfile::TempDir,
}

impl Device {
    /// A session of its own person's device, whose plane runs with its
    /// rooms' channel link as the session's does.
    fn start(seed: u8, label: &str) -> Self {
        let root = SigningKey::from_bytes(&[seed; 32]);
        let key = SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
        let cert = certified(&root, &key, label, DeviceCapabilities::member());
        let identity = tilo_network_actor::HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let state = tempfile::tempdir().expect("the session's directory");
        let engine = std::sync::Arc::new(
            Engine::open(identity.clone(), Some(state.path()), std::iter::empty())
                .expect("the session's engine opens"),
        );
        engine.attach_link(ChannelLink::new());
        let plane = Client::start(
            Some(identity),
            Some(std::sync::Arc::clone(&engine)),
            OwnPresence::default(),
            Vec::new(),
            MediaContinuity::default(),
            std::sync::Arc::new(|| ()),
        )
        .expect("the session's plane starts");
        Self {
            engine,
            plane,
            _state: state,
        }
    }

    fn person(&self) -> [u8; 32] {
        self.engine.identity().device_cert.user_public_key
    }

    /// Join the host's room with an invitation for this person, and wait
    /// until the room is held here.
    fn join(&self, host: &Host, request: u8) -> RoomId {
        let room = RoomId::parse(&host.room).expect("a room");
        let workspace = WorkspaceId::of_room(room);
        self.engine.bind(workspace, room);
        self.plane
            .enroll(Enrolling {
                workspace,
                workspace_id: room,
                invitation: host.invitation(self.person(), request),
            })
            .expect("the plane takes the enrollment");
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(enrolled) = self.plane.take_enrolled().pop() {
                enrolled.result.expect("the room admits this device");
                return room;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the room's host answers the enrollment"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The receipt of an effect this device's app had in `room`, as the broker
/// records it: redacted, its actor this device's person.
fn receipt(device: &Device, room: RoomId, invocation: uuid::Uuid) -> OwnerCommitRequest {
    let mut effect = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        tilo_actions::InvocationId(invocation),
        &hex::encode(device.person()),
        tilo_actions::ActionContext::for_resource("groceries", "note", "Groceries"),
        tilo_actions::EffectReceiptStatus::Succeeded,
        tilo_clock::unix_now_i64(),
    );
    effect.completed_at = 100;
    OwnerCommitRequest {
        workspace: room.to_string(),
        operation: CommitOperation::EffectReceipt {
            invocation,
            redacted: serde_json::to_vec(&effect).expect("a redacted receipt"),
        },
    }
}

/// Whether `device`'s copy of `room` holds the receipt of `invocation`.
fn holds_receipt(device: &Device, room: RoomId, invocation: uuid::Uuid) -> bool {
    device
        .engine
        .owner()
        .with_current(room, |view| {
            Ok(view.peer().admitted_workspace().is_some_and(|workspace| {
                tilo_desktop_model::projection::workspace_effect_receipts_of(workspace)
                    .iter()
                    .any(|held| held.invocation.0 == invocation)
            }))
        })
        .unwrap_or(false)
}

/// An effect an app had in a room this session holds: the broker the
/// session serves its apps from commits its receipt to the session's owner,
/// and the owner's custody leaves for the room's other session, which then
/// holds it too.
#[test]
fn a_receipt_the_owner_commits_reaches_the_rooms_other_session() {
    let host = Host::start();
    let writer = Device::start(0x51, "The writing device");
    let reader = Device::start(0x61, "The reading device");
    let room = writer.join(&host, 0x01);
    assert_eq!(reader.join(&host, 0x02), room);

    // The broker the writer's session serves its apps from.
    let publisher = crate::plane::owner_publisher(Some(std::sync::Arc::clone(&writer.engine)));
    assert!(
        publisher.records_effects_for(&room.to_string()),
        "the room's effect receipts are the owner's to commit"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the broker's runtime");
    let invocation = uuid::Uuid::new_v4();
    runtime
        .block_on(publisher.commit_owner(receipt(&writer, room, invocation), None))
        .expect("the owner commits the app's receipt");
    assert!(
        holds_receipt(&writer, room, invocation),
        "the receipt is the writer's owner's"
    );

    let deadline = tilo_test_env::hang_deadline();
    while !holds_receipt(&reader, room, invocation) {
        assert!(
            std::time::Instant::now() < deadline,
            "the receipt reaches the room's other session"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
