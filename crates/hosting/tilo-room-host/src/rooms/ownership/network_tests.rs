//! Moving a room with members' devices connected over the network, as the
//! desktop holds a joined room: they follow the handoff to the new host,
//! and what a member writes while the room moves waits on that device until
//! the new host is up, then reaches it.

use std::collections::HashMap;
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::RoomId;
use tilo_identity_host::custody::HostIdentity;
use tilo_joined_room::NativeWorkspaceOwner;
use tilo_joined_room::session::{Connect, Scope};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::join::{self, MemberRole};
use tilo_relay_session::RelayCoeditSession;
use tilo_relay_session::coedit::Inbound;
use tilo_relay_session::rooms::IssueInvitationRequest;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use web_time::SystemTime;

use crate::rooms::Rooms;
use crate::rooms::fixtures::{device_host, test_host};
use crate::rooms::ownership::tests::write_record;
use crate::rooms::tests::governor;

const APP: &str = "so.tilo.chat";

/// A person's own device, certified by their root.
fn device(label: &str) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&rand::random());
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .unwrap()
}

/// A member's device holding the room through the joined-room owner, with a
/// session to the host the room is reached at.
struct Member {
    identity: HostIdentity,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    workspace_id: u64,
    endpoint: iroh::Endpoint,
    scope: Arc<Scope>,
    session: RelayCoeditSession,
    _directory: tempfile::TempDir,
}

impl Member {
    async fn join(
        rooms: &Rooms,
        room: &str,
        issuer: &DeviceCert,
        (key, cert): (SigningKey, DeviceCert),
    ) -> Self {
        let invitation = rooms
            .issue(
                room,
                issuer,
                IssueInvitationRequest {
                    request_id: cert.device_public_key,
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(cert.user_public_key),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns()
                        .checked_add(600_000_000_000)
                        .unwrap(),
                    inviter_name: None,
                },
            )
            .await
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let owner = tilo_joined_room::open_file(&directory.path().join("joined.json")).unwrap();
        let identity = HostIdentity {
            device_cert: cert,
            device_signing_key: key.clone(),
            workspace_device_certs: HashMap::default(),
            workspace_revocations: HashMap::default(),
            identity_paths: None,
        };
        let endpoint = endpoint(&key).await;
        let workspace_id = rooms.summary(room).await.unwrap().workspace_id;
        let enrollment = tilo_test_env::within(
            "the member's enrollment",
            tilo_joined_room::enroll::prepare_and_complete(
                tilo_relay_session::workspace_join::WorkspaceJoinClient::new(endpoint.clone()),
                identity.clone(),
                workspace_id,
                None,
                join::encode_invitation_text(&invitation.signed_invitation),
                owner.clone(),
            ),
        )
        .await
        .unwrap();
        owner.install(&enrollment, SystemTime::now()).unwrap();
        let room = RoomId::parse(room).unwrap();
        let (scope, session) = connect(&owner, room, &identity, &endpoint, workspace_id).await;
        Self {
            identity,
            owner,
            room,
            workspace_id,
            endpoint,
            scope,
            session,
            _directory: directory,
        }
    }

    /// Dial wherever the room is reached now, as the desktop does when a
    /// session's pin no longer holds.
    async fn reconnect(&mut self) {
        let (scope, session) = connect(
            &self.owner,
            self.room,
            &self.identity,
            &self.endpoint,
            self.workspace_id,
        )
        .await;
        self.scope.close();
        self.scope = scope;
        self.session = session;
    }

    fn view<T>(&self, read: impl FnOnce(&tilo_joined_room::NativeCommittedView) -> T) -> T {
        self.owner
            .with_current(self.room, |view| Ok(read(view)))
            .unwrap()
    }

    async fn receive_until(
        &self,
        what: &str,
        done: impl Fn(&tilo_joined_room::NativeCommittedView) -> bool,
    ) {
        tilo_test_env::within(what, async {
            while !self.view(&done) {
                if let Inbound::Delta { author, data } =
                    self.session.recv().await.expect("the session stays open")
                {
                    let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                    let governor = self.view(|view| view.governor().device_public_key);
                    let input = self.scope.input(author, author == governor, message);
                    input.admit(&self.identity).unwrap();
                }
            }
        })
        .await;
    }

    /// Write `value` into the chat record, durably on this device, and hand
    /// it to the session if it may still send.
    fn write(&self, value: &str) -> Result<(), String> {
        let now = std::time::SystemTime::now();
        let message = self.view(|view| {
            let mut doc = view
                .peer()
                .document(APP, "")
                .map(|bytes| automerge::AutoCommit::load(&bytes).unwrap())
                .unwrap_or_default();
            let heads = doc.get_heads();
            doc.put(automerge::ROOT, "topic", value).unwrap();
            doc.commit_edit("fixture", "topic");
            view.peer()
                .clone()
                .publish_incremental(
                    APP,
                    "",
                    doc.save_after(&heads),
                    &self.identity.device_signing_key,
                    now,
                )
                .unwrap()
        });
        let NeighbourMsg::AppDocument { envelope, .. } = message else {
            panic!("a signed record write");
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
        self.scope.publish(&self.session, &self.identity)
    }

    /// How many writes wait on this device to be sent.
    fn waiting(&self) -> usize {
        self.owner
            .with_outgoing(self.room, std::time::SystemTime::now(), |_, outgoing| {
                Ok(outgoing.entries().count())
            })
            .unwrap()
    }
}

/// A session to the host `owner` reaches `room` at now.
async fn connect(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    identity: &HostIdentity,
    endpoint: &iroh::Endpoint,
    workspace_id: u64,
) -> (Arc<Scope>, RelayCoeditSession) {
    let connect = Connect::new(owner.clone(), room).unwrap();
    let (hint, device, revocations, continuity) = connect.material(identity).unwrap();
    let session = tilo_test_env::within(
        "the member's session",
        tilo_joined_room::session::dial(
            endpoint.clone(),
            workspace_id,
            &hint,
            device.clone(),
            identity.device_signing_key.clone(),
            revocations,
        ),
    )
    .await
    .unwrap();
    (
        Arc::new(Scope::new(connect, hint, device, continuity)),
        session,
    )
}

fn topic(rooms_document: Option<Vec<u8>>) -> Option<String> {
    use automerge::ReadDoc as _;
    let doc = automerge::AutoCommit::load(&rooms_document?).ok()?;
    doc.get(automerge::ROOT, "topic")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_made_while_the_room_moves_reaches_the_new_host_once_it_is_up() {
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let (owner_key, owner) = governor();
    let host = test_host(old.path(), &owner_key, &owner).await;
    let room = host.rooms().create("Studio").await.unwrap().room;
    let (recipient_key, recipient_cert) = device("recipient's desk");
    let recipient = Member::join(
        host.rooms(),
        &room,
        &owner,
        (recipient_key.clone(), recipient_cert.clone()),
    )
    .await;
    let member = Member::join(host.rooms(), &room, &owner, device("member's laptop")).await;
    write_record(
        host.rooms(),
        &room,
        (
            &member.identity.device_signing_key,
            &member.identity.device_cert,
        ),
        "before the move",
    )
    .await;
    member
        .receive_until("the record at the member", |view| {
            view.peer().document(APP, "").is_some()
        })
        .await;

    // The recipient's desk hosts the rooms placed on it.
    let recipients = device_host(new.path(), &recipient_key, &recipient_cert).await;
    let offer = host
        .rooms()
        .propose_ownership(&room, &owner, [1; 32], recipient_cert.user_public_key)
        .await
        .unwrap();
    recipient
        .receive_until("the offer at the recipient", |view| {
            view.peer().governance().ownership_offer().as_ref() == Some(&offer)
        })
        .await;
    let acceptance = recipients.rooms().acceptance(&offer).unwrap();
    host.rooms()
        .accept_ownership(&room, &recipient_cert, [2; 32], acceptance)
        .await
        .unwrap();
    for (who, device) in [("the recipient", &recipient), ("the member", &member)] {
        device
            .receive_until(&format!("the handoff at {who}"), |view| {
                view.governor() == &recipient_cert
            })
            .await;
    }

    // Moving: the member's session is pinned to the old host, which takes
    // nothing more, so the write waits on the member's device.
    assert!(member.write("while it moves").is_err());
    assert!(member.waiting() > 0);
    assert_eq!(
        member.view(|view| view.transport_hint().unwrap().host.clone()),
        recipient_cert
    );

    // The recipient's host takes the room up from the recipient's copy, and
    // the member's write reaches it there.
    let durable = recipient.view(|view| view.peer().durable().unwrap());
    let mut writes = recipients.rooms().record_writes();
    recipients.rooms().adopt(&room, &durable).await.unwrap();
    let mut member = member;
    member.reconnect().await;
    member
        .scope
        .publish(&member.session, &member.identity)
        .unwrap();
    tilo_test_env::within("the member's write at the new host", async {
        while topic(
            recipients
                .rooms()
                .record_document(&room, APP, "")
                .await
                .unwrap(),
        )
        .as_deref()
            != Some("while it moves")
        {
            writes.recv().await.unwrap();
        }
    })
    .await;
    recipients.shutdown().await;
    host.shutdown().await;
}
