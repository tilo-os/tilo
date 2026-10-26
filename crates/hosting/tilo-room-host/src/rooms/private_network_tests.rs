use super::*;
use std::collections::HashMap;
use tilo_peer_state::join;
use tilo_relay_protocol::rooms::private::{PrivateConversation, PrivateOperation, PrivateReply};
use tilo_relay_session::rooms::private::{PRIVATE_ALPN, PrivateClient};
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

mod foreign_devices;

const BODY: &str = "private-network-body-must-never-enter-the-public-room";

/// A separate native replica, enrolled through the signed invitation's pinned
/// host. Its private client uses the very endpoint and certificate that joined.
struct NativeEditor {
    directory: tempfile::TempDir,
    owner: tilo_joined_room::NativeWorkspaceOwner,
    endpoint: iroh::Endpoint,
    client: PrivateClient,
}

impl NativeEditor {
    async fn join(
        rooms: &Rooms,
        room: &str,
        governor: &DeviceCert,
        key: &SigningKey,
        cert: &DeviceCert,
    ) -> Self {
        let invitation = rooms
            .issue(
                room,
                governor,
                IssueInvitationRequest {
                    request_id: cert.device_public_key,
                    role: join::MemberRole::Editor,
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
        let endpoint = endpoint(key).await;
        let workspace = rooms.room(room).await.unwrap().lock().await.workspace_id();
        let identity = tilo_identity_host::custody::HostIdentity {
            device_cert: cert.clone(),
            device_signing_key: key.clone(),
            workspace_device_certs: HashMap::default(),
            workspace_revocations: HashMap::default(),
            identity_paths: None,
        };
        let enrollment = tilo_test_env::within(
            "native Editor invitation enrollment",
            tilo_joined_room::enroll::prepare_and_complete(
                tilo_relay_session::workspace_join::WorkspaceJoinClient::new(endpoint.clone()),
                identity,
                workspace,
                None,
                join::encode_invitation_text(&invitation.signed_invitation),
                owner.clone(),
            ),
        )
        .await
        .unwrap();
        owner.install(&enrollment, SystemTime::now()).unwrap();
        let joined = tilo_desktop_model::RoomId::parse(room).unwrap();
        owner
            .with_current(joined, |view| {
                assert_eq!(view.governor(), governor);
                assert_eq!(
                    view.peer()
                        .permissions(cert.device_public_key, SystemTime::now())?
                        .role,
                    // An invitation cannot demote another device of the owner.
                    if cert.user_public_key == governor.user_public_key {
                        collab_types::Role::Owner
                    } else {
                        collab_types::Role::Editor
                    }
                );
                Ok(())
            })
            .unwrap();
        let client = PrivateClient::new(endpoint.clone(), cert.clone());
        Self {
            directory,
            owner,
            endpoint,
            client,
        }
    }

    fn assert_no_local_private_messages(&self, room: &str) {
        let joined = tilo_desktop_model::RoomId::parse(room).unwrap();
        self.owner.with_current(joined, |_| Ok(())).unwrap();
        let bytes = std::fs::read(self.directory.path().join("joined.json")).unwrap();
        let stored: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let private = &stored["rooms"][room]["privateDirectConversations"];
        assert_eq!(private["pairs"], serde_json::json!({}));
        assert_eq!(private["invocations"], serde_json::json!({}));
        assert!(
            !bytes
                .windows(BODY.len())
                .any(|window| window == BODY.as_bytes())
        );
    }
}

fn identity(seed: u8, expires_at_ns: Option<u64>) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[seed; 32]);
    let key = SigningKey::from_bytes(&[seed.checked_add(1).unwrap(); 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Private network test device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        // Invitations pin the wildcard-bound relay's regular interface address,
        // not loopback. Allow this endpoint to reach that signed enrollment route
        // as well as the private protocol endpoints used below.
        .bind_addr((std::net::Ipv4Addr::UNSPECIFIED, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    assert_eq!(endpoint.id().as_bytes(), &key.verifying_key().to_bytes());
    endpoint
}

/// Use signed invitations and proofs, not a fabricated caller or membership map.
/// Historical admission allows an already-expired certificate to remain exactly
/// the admitted certificate without sleeping or changing the process clock.
async fn admit(
    rooms: &Rooms,
    room_id: &str,
    governor: &SigningKey,
    device: &SigningKey,
    cert: &DeviceCert,
    now: SystemTime,
) {
    let state = rooms.room(room_id).await.unwrap();
    let ns = u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let admitted = {
        let mut room = state.lock().await;
        let signed = join::issue_invitation(
            join::InviteGrant {
                workspace_id: room.workspace_id(),
                room: room_id.into(),
                authority: room.record.authority.clone(),
                issuer: room.record.authority.clone(),
                delegation: None,
                role: join::MemberRole::Editor,
                nonce: [cert.device_public_key[0]; 16],
                recipient_user_public_key: Some(cert.user_public_key),
                invitee_name: Some(cert.device_label.clone()),
                expires_at_ns: ns.checked_add(60_000_000_000).unwrap(),
                transport: Vec::new(),
            },
            governor,
        )
        .unwrap();
        let verified = join::decode_and_verify_invitation(
            &signed,
            Some(cert.user_public_key),
            &RevocationList::new(),
            ns,
        )
        .unwrap();
        let challenge = join::JoinChallenge::new(
            &verified,
            cert,
            [11; 32],
            [12; 32],
            ns.checked_add(30_000_000_000).unwrap(),
            ns,
        )
        .unwrap();
        let proof = join::sign_join_proof(challenge.clone(), device).unwrap();
        let redemption =
            join::verify_join_proof(&proof, &challenge, cert, &RevocationList::new(), ns).unwrap();
        let mut candidate = room.peer().clone();
        assert!(
            candidate
                .governance_mut()
                .admit_invited_member(&redemption, governor, now)
                .unwrap()
        );
        let collaboration = room.collaboration().clone();
        room.commit(candidate, governor, now, |_, _| Ok(collaboration))
            .unwrap();
        room.peer()
            .certificates()
            .iter()
            .any(|current| current == cert)
    };
    assert!(admitted);
}

async fn rpc(
    client: &PrivateClient,
    address: &iroh::EndpointAddr,
    room: &str,
    operation: PrivateOperation,
) -> PrivateReply {
    tilo_test_env::within("private governor network reply", async {
        client
            .send(address.clone(), room.into(), operation)
            .await
            .unwrap()
    })
    .await
}

fn start_watch(
    client: PrivateClient,
    address: iroh::EndpointAddr,
    room: String,
    peer: String,
    after_sequence: u64,
) -> tokio::task::JoinHandle<PrivateReply> {
    tokio::spawn(async move {
        rpc(
            &client,
            &address,
            &room,
            PrivateOperation::Watch {
                peer,
                after_sequence,
            },
        )
        .await
    })
}

async fn wait_for_watch(rooms: &Rooms, room: &str, caller: &DeviceCert, peer: &str) {
    tilo_test_env::within("private pair subscription", async {
        let state = rooms.room(room).await.unwrap();
        loop {
            if state
                .lock()
                .await
                .private_watcher_count(caller, peer)
                .unwrap()
                > 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

fn conversation(reply: PrivateReply) -> PrivateConversation {
    match reply {
        PrivateReply::Conversation(conversation) => conversation,
        other => panic!("expected private conversation, got {other:?}"),
    }
}

async fn assert_read_watch_refused(
    client: &PrivateClient,
    address: &iroh::EndpointAddr,
    room: &str,
    peer: &str,
) {
    for operation in [
        PrivateOperation::Read {
            peer: peer.into(),
            after_sequence: 0,
            limit: 50,
        },
        PrivateOperation::Watch {
            peer: peer.into(),
            after_sequence: 0,
        },
    ] {
        assert!(matches!(
            rpc(client, address, room, operation).await,
            PrivateReply::Refused
        ));
    }
}

struct PrivatePair {
    room: String,
    address: iroh::EndpointAddr,
    alice: PrivateClient,
    bob: PrivateClient,
    a: String,
    b: String,
}

impl PrivatePair {
    async fn assert_contacts_and_open(&self) -> String {
        let Self {
            room,
            address,
            alice,
            bob,
            a,
            b,
        } = self;
        let contacts = rpc(alice, address, room, PrivateOperation::Contacts).await;
        let PrivateReply::Contacts(contacts) = contacts else {
            panic!("expected contacts")
        };
        assert!(contacts.iter().any(|contact| contact.public_key == *b));
        assert!(!contacts.iter().any(|contact| contact.public_key == *a));
        let opened_a = conversation(
            rpc(
                alice,
                address,
                room,
                PrivateOperation::Open { peer: b.clone() },
            )
            .await,
        );
        let opened_b = conversation(
            rpc(
                bob,
                address,
                room,
                PrivateOperation::Open { peer: a.clone() },
            )
            .await,
        );
        let mut people = [a.clone(), b.clone()];
        people.sort();
        let canonical = format!(
            "direct_{}",
            blake3::hash(&serde_json::to_vec(&("private-person-pair-v1", room, people)).unwrap())
                .to_hex()
        );
        assert_eq!(opened_a.id, canonical);
        assert_eq!(opened_b.id, canonical);
        assert!(opened_a.messages.is_empty());
        assert!(opened_b.messages.is_empty());
        canonical
    }

    async fn assert_message_exchange(
        &self,
        rooms: &Rooms,
        bob_cert: &DeviceCert,
        canonical: &str,
    ) -> PrivateConversation {
        let Self {
            room,
            address,
            alice,
            bob,
            a,
            b,
        } = self;
        let pending_message = start_watch(bob.clone(), address.clone(), room.clone(), a.clone(), 0);
        wait_for_watch(rooms, room, bob_cert, a).await;
        assert!(!pending_message.is_finished());
        assert!(matches!(
            rpc(
                alice,
                address,
                room,
                PrivateOperation::Send {
                    peer: b.clone(),
                    invocation: uuid::Uuid::new_v4(),
                    text: BODY.into(),
                }
            )
            .await,
            PrivateReply::Sent
        ));
        assert!(matches!(
            tilo_test_env::within("pending private message watch", pending_message)
                .await
                .unwrap(),
            PrivateReply::Changed(1)
        ));
        assert!(matches!(
            rpc(
                bob,
                address,
                room,
                PrivateOperation::Watch {
                    peer: a.clone(),
                    after_sequence: 0,
                }
            )
            .await,
            PrivateReply::Changed(1)
        ));
        let received = conversation(
            rpc(
                bob,
                address,
                room,
                PrivateOperation::Read {
                    peer: a.clone(),
                    after_sequence: 0,
                    limit: 50,
                },
            )
            .await,
        );
        assert_eq!(received.id, canonical);
        assert_eq!(received.peer, *a);
        assert_eq!(received.messages.len(), 1);
        let message = received.messages.first().unwrap();
        assert_eq!(message.text, BODY);
        assert_eq!(message.sender, *a);
        assert_eq!(message.sequence, 1);
        let own = conversation(
            rpc(
                alice,
                address,
                room,
                PrivateOperation::Read {
                    peer: b.clone(),
                    after_sequence: 0,
                    limit: 50,
                },
            )
            .await,
        );
        assert_eq!(own.id, received.id);
        assert_eq!(own.messages.first().unwrap().id, message.id);
        assert_eq!(
            serde_json::to_value(&own.messages).unwrap(),
            serde_json::to_value(&received.messages).unwrap(),
            "the separately enrolled replicas read the same canonical governor messages"
        );
        received
    }

    async fn assert_public_unchanged(
        &self,
        rooms: &Rooms,
        public_before: &[u8],
        canonical: &str,
        message_id: &str,
    ) {
        let (public_after, publications) = {
            let state = rooms.room(&self.room).await.unwrap();
            let room = state.lock().await;
            // Inspect the real retained outgoing bytes, not Debug or an empty stand-in.
            (
                postcard::to_stdvec(&room.record.outgoing).unwrap(),
                room.record
                    .outgoing
                    .iter()
                    .map(|publication| publication.bytes.clone())
                    .collect::<Vec<_>>(),
            )
        };
        assert!(!publications.is_empty());
        assert_eq!(public_after, public_before);
        for marker in [BODY, canonical, message_id] {
            for bytes in &publications {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|bytes| bytes == marker.as_bytes())
                );
            }
        }
    }

    async fn assert_removal(
        &self,
        rooms: &Rooms,
        governor_cert: &DeviceCert,
        bob_cert: &DeviceCert,
    ) {
        let Self {
            room,
            address,
            alice,
            bob,
            a,
            b,
        } = self;
        let pending_removal = start_watch(bob.clone(), address.clone(), room.clone(), a.clone(), 1);
        wait_for_watch(rooms, room, bob_cert, a).await;
        assert!(!pending_removal.is_finished());
        rooms
            .remove_member(
                room,
                governor_cert,
                RemoveMemberRequest {
                    request_id: [61; 32],
                    user_public_key: bob_cert.user_public_key,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            tilo_test_env::within("pending private membership watch", pending_removal)
                .await
                .unwrap(),
            PrivateReply::Refused
        ));
        assert_read_watch_refused(bob, address, room, a).await;
        // The remaining participant cannot use a stale contact either.
        assert_read_watch_refused(alice, address, room, b).await;
    }
}

#[tokio::test]
async fn two_remote_people_share_one_private_pair_and_removed_devices_lose_access() {
    let directory = tempfile::tempdir().unwrap();
    let (governor_key, governor_cert) = tests::governor();
    exercise_private_pair(
        tests::test_host(directory.path(), &governor_key, &governor_cert).await,
        &governor_key,
        &governor_cert,
    )
    .await;
}

async fn exercise_private_pair(
    relay: crate::rooms::fixtures::TestHost,
    governor_key: &SigningKey,
    governor_cert: &DeviceCert,
) {
    {
        let rooms = relay.rooms().clone();
        let created = rooms
            .create("Private conversations over QUIC")
            .await
            .unwrap();
        let (alice_key, alice_cert) = identity(31, None);
        let (bob_key, bob_cert) = identity(41, None);
        let (stranger_key, stranger_cert) = identity(51, None);
        let alice_native = NativeEditor::join(
            &rooms,
            &created.room,
            governor_cert,
            &alice_key,
            &alice_cert,
        )
        .await;
        let bob_native =
            NativeEditor::join(&rooms, &created.room, governor_cert, &bob_key, &bob_cert).await;
        alice_native.assert_no_local_private_messages(&created.room);
        bob_native.assert_no_local_private_messages(&created.room);
        let server = endpoint(governor_key).await;
        let router = iroh::protocol::Router::builder(server.clone())
            .accept(PRIVATE_ALPN, PrivateProtocol::new(rooms.clone()))
            .spawn();
        let address = server.addr();
        let alice_endpoint = &alice_native.endpoint;
        let bob_endpoint = &bob_native.endpoint;
        let stranger_endpoint = endpoint(&stranger_key).await;
        assert_ne!(alice_endpoint.id(), bob_endpoint.id());
        let stranger = PrivateClient::new(stranger_endpoint.clone(), stranger_cert);
        let pair = PrivatePair {
            room: created.room.clone(),
            address,
            alice: alice_native.client.clone(),
            bob: bob_native.client.clone(),
            a: hex::encode(alice_cert.user_public_key),
            b: hex::encode(bob_cert.user_public_key),
        };
        let canonical = pair.assert_contacts_and_open().await;
        let public_before = {
            let state = rooms.room(&created.room).await.unwrap();
            let room = state.lock().await;
            postcard::to_stdvec(&room.record.outgoing).unwrap()
        };
        let received = pair
            .assert_message_exchange(&rooms, &bob_cert, &canonical)
            .await;
        // Both native replicas read the same governor-held record without copying
        // the private pair or its messages into either local owner file.
        alice_native.assert_no_local_private_messages(&created.room);
        bob_native.assert_no_local_private_messages(&created.room);
        pair.assert_public_unchanged(
            &rooms,
            &public_before,
            &canonical,
            &received.messages.first().unwrap().id,
        )
        .await;
        for peer in [&pair.a, &pair.b, &canonical] {
            assert_read_watch_refused(&stranger, &pair.address, &created.room, peer).await;
        }
        pair.assert_removal(&rooms, governor_cert, &bob_cert).await;
        drop(rooms);
        alice_endpoint.close().await;
        bob_endpoint.close().await;
        stranger_endpoint.close().await;
        router.shutdown().await.unwrap();
    }
    relay.shutdown().await;
}

#[tokio::test]
async fn an_exact_admitted_but_expired_transport_certificate_cannot_read_or_watch() {
    let directory = tempfile::tempdir().unwrap();
    let (governor_key, governor_cert) = tests::governor();
    exercise_expired_certificate(
        tests::test_host(directory.path(), &governor_key, &governor_cert).await,
        &governor_key,
        &governor_cert,
    )
    .await;
}

async fn exercise_expired_certificate(
    relay: crate::rooms::fixtures::TestHost,
    governor_key: &SigningKey,
    governor_cert: &DeviceCert,
) {
    let rooms = relay.rooms().clone();
    let created = rooms.create("Expired private client").await.unwrap();
    let now = SystemTime::now();
    let historical = now.checked_sub(Duration::from_secs(60)).unwrap();
    let expires = u64::try_from(
        now.checked_sub(Duration::from_secs(30))
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let (expired_key, expired_cert) = identity(71, Some(expires));
    admit(
        &rooms,
        &created.room,
        governor_key,
        &expired_key,
        &expired_cert,
        historical,
    )
    .await;
    let (historically_admitted, currently_expired) = {
        let state = rooms.room(&created.room).await.unwrap();
        let room = state.lock().await;
        (
            room.peer()
                .permissions(expired_cert.device_public_key, historical)
                .is_ok(),
            room.peer()
                .permissions(expired_cert.device_public_key, now)
                .is_err(),
        )
    };
    assert!(historically_admitted);
    assert!(currently_expired);
    let server = endpoint(governor_key).await;
    let router = iroh::protocol::Router::builder(server.clone())
        .accept(PRIVATE_ALPN, PrivateProtocol::new(rooms.clone()))
        .spawn();
    drop(rooms);
    let client_endpoint = endpoint(&expired_key).await;
    let client = PrivateClient::new(client_endpoint.clone(), expired_cert);
    // Contacts has no pair lookup: a refusal here cannot be explained by an
    // unopened conversation masking a missing certificate-expiry check.
    assert!(matches!(
        rpc(
            &client,
            &server.addr(),
            &created.room,
            PrivateOperation::Contacts
        )
        .await,
        PrivateReply::Refused
    ));
    assert_read_watch_refused(
        &client,
        &server.addr(),
        &created.room,
        &hex::encode(governor_cert.user_public_key),
    )
    .await;
    client_endpoint.close().await;
    router.shutdown().await.unwrap();
    relay.shutdown().await;
}

/// A page reaches the same door over its HTTPS session: the device its
/// session proved is the caller, a watch is a poll whose time running out
/// answers the unchanged cursor, and a change wakes it at once.
#[tokio::test]
async fn a_pages_session_carries_the_same_private_door() {
    let directory = tempfile::tempdir().unwrap();
    let (governor_key, governor_cert) = tests::governor();
    let relay = tests::test_host(directory.path(), &governor_key, &governor_cert).await;
    let rooms = relay.rooms().clone();
    let created = rooms
        .create("Private conversations in a page")
        .await
        .unwrap();
    let (alice_key, alice) = identity(81, None);
    let (bob_key, bob) = identity(91, None);
    let now = SystemTime::now();
    admit(
        &rooms,
        &created.room,
        &governor_key,
        &alice_key,
        &alice,
        now,
    )
    .await;
    admit(&rooms, &created.room, &governor_key, &bob_key, &bob, now).await;
    let door = PrivateProtocol::new(rooms.clone());
    let (a, b) = (
        hex::encode(alice.user_public_key),
        hex::encode(bob.user_public_key),
    );
    let ask = |cert: &DeviceCert, operation, poll: Duration| {
        let (door, cert, room) = (door.clone(), cert.clone(), created.room.clone());
        async move {
            let until = async {
                tokio::time::sleep(poll).await;
                Ok(())
            };
            door.serve(&cert, &cert.device_public_key, &room, operation, until)
                .await
        }
    };
    let quick = Duration::from_millis(50);
    let PrivateReply::Contacts(contacts) = ask(&alice, PrivateOperation::Contacts, quick).await
    else {
        panic!("contacts");
    };
    assert!(contacts.iter().any(|contact| contact.public_key == b));
    assert!(matches!(
        ask(&alice, PrivateOperation::Open { peer: b.clone() }, quick).await,
        PrivateReply::Conversation(_)
    ));
    // Nothing new within the poll: the cursor it was given.
    let unchanged = ask(
        &bob,
        PrivateOperation::Watch {
            peer: a.clone(),
            after_sequence: 0,
        },
        quick,
    )
    .await;
    assert!(matches!(unchanged, PrivateReply::Changed(0)));
    let watching = tokio::spawn(ask(
        &bob,
        PrivateOperation::Watch {
            peer: a.clone(),
            after_sequence: 0,
        },
        Duration::from_secs(60),
    ));
    wait_for_watch(&rooms, &created.room, &bob, &a).await;
    let sent = ask(
        &alice,
        PrivateOperation::Send {
            peer: b.clone(),
            invocation: uuid::Uuid::new_v4(),
            text: BODY.into(),
        },
        quick,
    )
    .await;
    assert!(matches!(sent, PrivateReply::Sent));
    let changed = tilo_test_env::within("the page's watch wakes", watching)
        .await
        .unwrap();
    assert!(matches!(changed, PrivateReply::Changed(1)));
    let read = conversation(
        ask(
            &bob,
            PrivateOperation::Read {
                peer: a.clone(),
                after_sequence: 0,
                limit: 50,
            },
            quick,
        )
        .await,
    );
    assert_eq!(read.messages.len(), 1);
    assert_eq!(read.messages[0].text, BODY);
    assert_eq!(read.messages[0].sender, a);
    drop((rooms, door));
    relay.shutdown().await;
}

/// A person names themself in the room: governance holds the name, so
/// every person list shows it, not their device's label, and naming them
/// what they are called writes nothing. The room's spectator person, whom
/// every watching page is certified as, is a member yet nobody: it names
/// nobody, holds no conversations and is on no one's list.
#[tokio::test]
async fn people_name_themselves_and_the_spectator_is_nobody() {
    use tilo_relay_protocol::rooms::{RoomsReply, RoomsRequest};
    let directory = tempfile::tempdir().unwrap();
    let (governor_key, governor_cert) = tests::governor();
    let relay = tests::test_host(directory.path(), &governor_key, &governor_cert).await;
    let rooms = relay.rooms().clone();
    let created = rooms.create("Who is who").await.unwrap();
    let (alice_key, alice) = identity(82, None);
    let (bob_key, bob) = identity(92, None);
    let (watcher_key, watcher) = identity(112, None);
    let now = SystemTime::now();
    for (key, cert) in [
        (&alice_key, &alice),
        (&bob_key, &bob),
        (&watcher_key, &watcher),
    ] {
        admit(&rooms, &created.room, &governor_key, key, cert, now).await;
    }
    relay
        .admit_spectators(
            created.workspace_id,
            crate::room_access::Spectators {
                root: watcher.user_public_key,
                seats: 4,
                public: true,
            },
        )
        .await
        .unwrap();
    let name = |cert: &DeviceCert, name: &str| {
        let (rooms, cert, room) = (rooms.clone(), cert.clone(), created.room.clone());
        let name = name.to_owned();
        async move {
            rooms
                .page_rooms(&cert, RoomsRequest::Name { room, name })
                .await
        }
    };
    assert_eq!(name(&alice, "Lucky Seal").await, RoomsReply::Named);
    let summary = rooms.summary(&created.room).await.unwrap();
    let named = summary
        .members
        .iter()
        .find(|member| member.user_public_key == alice.user_public_key)
        .unwrap();
    assert_eq!(named.display_name.as_deref(), Some("Lucky Seal"));
    assert_eq!(name(&alice, "Lucky Seal").await, RoomsReply::Named);
    assert_eq!(
        rooms.summary(&created.room).await.unwrap().generation,
        summary.generation,
        "the name they already go by writes nothing"
    );

    let door = PrivateProtocol::new(rooms.clone());
    let ask = |cert: &DeviceCert, operation| {
        let (door, cert, room) = (door.clone(), cert.clone(), created.room.clone());
        async move {
            door.serve(
                &cert,
                &cert.device_public_key,
                &room,
                operation,
                std::future::ready(Ok(())),
            )
            .await
        }
    };
    let PrivateReply::Contacts(contacts) = ask(&bob, PrivateOperation::Contacts).await else {
        panic!("contacts");
    };
    let watcher_key_hex = hex::encode(watcher.user_public_key);
    assert!(contacts.iter().any(|contact| contact.public_key
        == hex::encode(alice.user_public_key)
        && contact.display_name.as_deref() == Some("Lucky Seal")));
    assert!(
        !contacts
            .iter()
            .any(|contact| contact.public_key == watcher_key_hex),
        "the spectator is nobody's contact"
    );
    let open_watcher = PrivateOperation::Open {
        peer: watcher_key_hex,
    };
    assert!(matches!(
        ask(&bob, open_watcher).await,
        PrivateReply::Refused
    ));
    assert!(matches!(
        ask(&watcher, PrivateOperation::Contacts).await,
        PrivateReply::Refused
    ));
    let (_, stranger) = identity(102, None);
    for (cert, asked) in [
        (&watcher, "Watching"),
        (&stranger, "Someone"),
        (&bob, "\u{7}"),
    ] {
        let refused = matches!(name(cert, asked).await, RoomsReply::Refused(_));
        assert!(refused, "{asked:?} names nobody");
    }
    drop((rooms, door));
    relay.shutdown().await;
}

/// A page's session proves one device: a certificate the session did not
/// prove is refused at the door like a stranger's.
#[tokio::test]
async fn a_page_is_only_the_device_its_session_proved() {
    let directory = tempfile::tempdir().unwrap();
    let (governor_key, governor_cert) = tests::governor();
    let relay = tests::test_host(directory.path(), &governor_key, &governor_cert).await;
    let rooms = relay.rooms().clone();
    let created = rooms.create("One device per session").await.unwrap();
    let (alice_key, alice) = identity(83, None);
    let (bob_key, bob) = identity(93, None);
    let now = SystemTime::now();
    admit(
        &rooms,
        &created.room,
        &governor_key,
        &alice_key,
        &alice,
        now,
    )
    .await;
    admit(&rooms, &created.room, &governor_key, &bob_key, &bob, now).await;
    let door = PrivateProtocol::new(rooms.clone());
    let ask = |cert: &DeviceCert, device: [u8; 32]| {
        let (door, cert, room) = (door.clone(), cert.clone(), created.room.clone());
        async move {
            let contacts = PrivateOperation::Contacts;
            door.serve(&cert, &device, &room, contacts, std::future::ready(Ok(())))
                .await
        }
    };
    assert!(matches!(
        ask(&alice, alice.device_public_key).await,
        PrivateReply::Contacts(_)
    ));
    assert!(matches!(
        ask(&alice, bob.device_public_key).await,
        PrivateReply::Refused
    ));
    let (_, stranger) = identity(103, None);
    assert!(matches!(
        ask(&stranger, stranger.device_public_key).await,
        PrivateReply::Refused
    ));
    drop((rooms, door));
    relay.shutdown().await;
}
