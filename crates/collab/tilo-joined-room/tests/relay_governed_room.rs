//! A native device of another person joins a room a relay governs: it
//! redeems the invitation, installs the room, holds its relay session, and
//! edits with the room's owner, each seeing the other's presence.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the relay serves the whole test"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use collab_types::CommitNamed as _;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::RoomId;
use tilo_identity_host::custody::HostIdentity;
use tilo_joined_room::NativeWorkspaceOwner;
use tilo_joined_room::session::{Connect, Scope};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::join::{self, MemberRole};
use tilo_peer_state::{StatePeer, TransportOrigin};
use tilo_relay::coedit::{Inbound, RelayCoeditSession};
use tilo_relay::rooms::IssueInvitationRequest;
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::{Relay, RelayClient, RelayConfig};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

tilo_automerge_isolate::test_worker!();

const APP: &str = "so.tilo.chat";

fn certificate(
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

/// The text of an Editor's invitation to `room`, for `person` only, that
/// the relay governing it issues for the room's owner `issuer`.
async fn invite(relay: &Relay, issuer: &DeviceCert, room: &str, person: [u8; 32]) -> String {
    let invitation = relay
        .rooms()
        .unwrap()
        .issue(
            room,
            issuer,
            IssueInvitationRequest {
                request_id: person,
                role: MemberRole::Editor,
                recipient_user_public_key: Some(person),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    join::encode_invitation_text(&invitation.signed_invitation)
}

async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .unwrap()
}

fn text(doc: Option<Vec<u8>>, key: &str) -> Option<String> {
    let doc = AutoCommit::load(&doc?).ok()?;
    doc.get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// A person's native device holding a room through the joined-room owner,
/// as the shell does.
struct Native {
    identity: HostIdentity,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    scope: Arc<Scope>,
    session: RelayCoeditSession,
    presence: Vec<[u8; 32]>,
}

impl Native {
    async fn join(
        directory: &std::path::Path,
        cert: DeviceCert,
        key: SigningKey,
        invitation: &str,
        workspace_id: u64,
    ) -> Self {
        let owner = tilo_joined_room::open_file(&directory.join("joined-rooms.json")).unwrap();
        let identity = HostIdentity {
            device_cert: cert,
            device_signing_key: key.clone(),
            workspace_device_certs: HashMap::default(),
            workspace_revocations: HashMap::default(),
            identity_paths: None,
        };
        let endpoint = endpoint(&key).await;
        let enrollment = tilo_joined_room::enroll::prepare_and_complete(
            WorkspaceJoinClient::new(endpoint.clone()),
            identity.clone(),
            workspace_id,
            None,
            invitation.to_owned(),
            owner.clone(),
        )
        .await
        .unwrap();
        owner.install(&enrollment, SystemTime::now()).unwrap();
        let room = RoomId::parse(&enrollment.bootstrap().room).unwrap();
        let connect = Connect::new(owner.clone(), room).unwrap();
        let (hint, device, revocations, continuity) = connect.material(&identity).unwrap();
        let session = tilo_joined_room::session::dial(
            endpoint,
            workspace_id,
            &hint,
            device.clone(),
            key,
            revocations,
        )
        .await
        .unwrap();
        let scope = Arc::new(Scope::new(connect, hint, device, continuity));
        Self {
            identity,
            owner,
            room,
            scope,
            session,
            presence: Vec::new(),
        }
    }

    fn document(&self) -> Option<Vec<u8>> {
        self.owner
            .with_current(self.room, |view| Ok(view.peer().document(APP, "")))
            .unwrap()
    }

    async fn receive_until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        tilo_test_env::within(what, async {
            while !done(self) {
                match self.session.recv().await.expect("the session stays open") {
                    Inbound::Delta { author, data } => {
                        let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                        let governor = self
                            .owner
                            .with_current(self.room, |view| Ok(view.governor().device_public_key))
                            .unwrap();
                        let input = self.scope.input(author, author == governor, message);
                        if let Err(error) = input.admit(&self.identity) {
                            panic!("{what}: admission refused: {error}");
                        }
                    }
                    Inbound::Presence { author, .. } => self.presence.push(author),
                    _ => {}
                }
            }
        })
        .await;
    }

    /// Write `key` = `value` into the chat record as this device, durably,
    /// and send it.
    fn write(&self, key: &str, value: &str) {
        let now = SystemTime::now();
        let message = self
            .owner
            .with_current(self.room, |view| {
                let mut doc = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
                let heads = doc.get_heads();
                doc.put(ROOT, key, value).unwrap();
                doc.commit_edit("fixture", "fixture");
                view.peer().clone().publication_mut().publish_incremental(
                    APP,
                    "",
                    doc.save_after(&heads),
                    &self.identity.device_signing_key,
                    now,
                )
            })
            .unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = message else {
            panic!("a signed document write")
        };
        self.owner
            .commit_document(
                OwnerCommitRequest {
                    room: self.room.to_string(),
                    operation: CommitOperation::Document {
                        app: APP.into(),
                        record: String::new(),
                        envelope,
                    },
                },
                now,
            )
            .unwrap();
        self.scope.publish(&self.session, &self.identity).unwrap();
    }
}

fn publish(session: &RelayCoeditSession, message: &NeighbourMsg) {
    let (channel, kind) = neighbour_wire::retention(message, false).unwrap();
    let body = postcard::to_stdvec(message).unwrap();
    if kind == neighbour_wire::Retention::Checkpoint {
        session.publish_checkpoint(channel, body)
    } else {
        session.publish_retained_delta(channel, body)
    }
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_native_device_of_another_person_joins_a_relay_governed_room() {
    let alice = SigningKey::from_bytes(&[1; 32]);
    let bob = SigningKey::from_bytes(&[2; 32]);
    let host_key = SigningKey::from_bytes(&[3; 32]);
    let laptop_key = SigningKey::from_bytes(&[4; 32]);
    let desk_key = SigningKey::from_bytes(&[5; 32]);
    let host = certificate(&alice, &host_key, "alice-home", DeviceCapabilities::host());
    let laptop = certificate(
        &alice,
        &laptop_key,
        "alice-laptop",
        DeviceCapabilities::primary(),
    );
    let desk = certificate(&bob, &desk_key, "bob-desk", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: alice.verifying_key().to_bytes(),
            device_signing_key: host_key.clone(),
            device_cert: host.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let address = relay.node_addr();

    // Alice's room on her relay, her device in it as its owner's, and Bob's
    // person invited.
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Design review").await.unwrap();
    rooms.enter(&room.room, &laptop).await.unwrap();
    let invitation = invite(&relay, &laptop, &room.room, bob.verifying_key().to_bytes()).await;

    // Bob's native device redeems it, installs the room and connects.
    let directory = tempfile::tempdir().unwrap();
    let mut bob_native = Native::join(
        directory.path(),
        desk.clone(),
        desk_key,
        &invitation,
        room.workspace_id,
    )
    .await;
    assert_eq!(bob_native.room.to_string(), room.room);
    let role = |native: &Native, device: [u8; 32]| {
        native
            .owner
            .with_current(native.room, |view| {
                Ok(view
                    .peer()
                    .permissions(device, SystemTime::now())
                    .ok()
                    .map(|member| member.role))
            })
            .unwrap()
    };
    assert_eq!(
        role(&bob_native, desk.device_public_key),
        Some(collab_types::Role::Editor)
    );

    // Alice's replica, over her own relay session.
    let alice_session = RelayCoeditSession::join(
        RelayClient::connect_to_pinned_host(
            endpoint(&laptop_key).await,
            address.clone(),
            laptop.clone(),
            laptop_key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        room.workspace_id,
        laptop.device_public_key,
    )
    .unwrap();
    let mut alice_peer = StatePeer::new(
        &room.room,
        host.clone(),
        vec![host.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    let mut alice_presence = Vec::new();
    let alice_receive =
        async |peer: &mut StatePeer,
               presence: &mut Vec<[u8; 32]>,
               what: &str,
               done: &dyn Fn(&StatePeer, &[[u8; 32]]) -> bool| {
            tilo_test_env::within(what, async {
                while !done(peer, presence) {
                    match alice_session.recv().await.expect("the session stays open") {
                        Inbound::Delta { author, data } => {
                            let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                            let origin = if author == peer.authority().device_public_key {
                                TransportOrigin::RetainedRelayDelta(author)
                            } else {
                                TransportOrigin::RelayDelta(author)
                            };
                            peer.receive_from(origin, message, SystemTime::now())
                                .unwrap();
                        }
                        Inbound::Presence { author, .. } => presence.push(author),
                        _ => {}
                    }
                }
            })
            .await;
        };
    alice_receive(
        &mut alice_peer,
        &mut alice_presence,
        "Alice's replica admits Bob's device",
        &|peer, _| {
            peer.permissions(desk.device_public_key, SystemTime::now())
                .is_ok_and(|member| member.role == collab_types::Role::Editor)
        },
    )
    .await;

    // Alice writes; Bob's native owner admits it.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "from Alice").unwrap();
    doc.commit_edit("fixture", "topic");
    let message = alice_peer
        .publication_mut()
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop_key, SystemTime::now())
        .unwrap();
    publish(&alice_session, &message);
    bob_native
        .receive_until("Alice's write at Bob's native device", |native| {
            text(native.document(), "topic").as_deref() == Some("from Alice")
        })
        .await;

    // Bob writes through his durable outgoing custody; Alice sees it.
    bob_native.write("reply", "from Bob");
    alice_receive(
        &mut alice_peer,
        &mut alice_presence,
        "Bob's write at Alice",
        &|peer, _| text(peer.document(APP, ""), "reply").as_deref() == Some("from Bob"),
    )
    .await;

    // Each sees the other's presence.
    alice_session.publish_presence(b"alice".to_vec()).unwrap();
    bob_native
        .session
        .publish_presence(b"bob".to_vec())
        .unwrap();
    bob_native
        .receive_until("Alice's presence at Bob", |native| {
            native.presence.contains(&laptop.device_public_key)
        })
        .await;
    alice_receive(
        &mut alice_peer,
        &mut alice_presence,
        "Bob's presence at Alice",
        &|_, seen| seen.contains(&desk.device_public_key),
    )
    .await;
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owners_native_device_holds_its_relay_governed_room() {
    let alice = SigningKey::from_bytes(&[1; 32]);
    let host_key = SigningKey::from_bytes(&[3; 32]);
    let laptop_key = SigningKey::from_bytes(&[4; 32]);
    let host = certificate(&alice, &host_key, "alice-home", DeviceCapabilities::host());
    let laptop = certificate(
        &alice,
        &laptop_key,
        "alice-laptop",
        DeviceCapabilities::primary(),
    );
    let state = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: alice.verifying_key().to_bytes(),
            device_signing_key: host_key.clone(),
            device_cert: host.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Mine").await.unwrap();
    rooms.enter(&room.room, &laptop).await.unwrap();
    let invitation = invite(
        &relay,
        &laptop,
        &room.room,
        alice.verifying_key().to_bytes(),
    )
    .await;
    let directory = tempfile::tempdir().unwrap();
    let native = Native::join(
        directory.path(),
        laptop.clone(),
        laptop_key,
        &invitation,
        room.workspace_id,
    )
    .await;
    let role = native
        .owner
        .with_current(native.room, |view| {
            Ok(view
                .peer()
                .permissions(laptop.device_public_key, SystemTime::now())
                .map(|member| member.role))
        })
        .unwrap();
    assert_eq!(role, Ok(collab_types::Role::Owner));
    relay.shutdown().await;
}
