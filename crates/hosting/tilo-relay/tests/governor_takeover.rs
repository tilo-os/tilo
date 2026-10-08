//! Another governor of the identity takes a room over from a revoked one and
//! governs it fully: a device successor admits, invites and revokes; a relay
//! of the identity takes over a room another relay created; and members that
//! cannot reach the successor follow it through any other member.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "end-to-end scenarios whose relays and sessions live until they end"
)]

use std::time::{Duration, SystemTime};

use tilo_peer_state::join::{
    InviteGrant, JoinChallenge, MemberRole, decode_and_verify_invitation, issue_invitation,
    sign_join_proof, verify_join_proof,
};
use tilo_peer_state::succession::{self, GovernorSuccession};
use tilo_peer_state::{Custody, NeighbourMsg, StatePeer, TransportOrigin};
use tilo_relay::coedit::RelayCoeditSession;
use tilo_relay::daemon::Relay;
use tilo_relay::rooms::{RoomAdminClient, RoomCommand, RoomReply, RoomSummary};
use tilo_user_identity::{DeviceCapabilities, RevocationEntry, RevocationList};

use crate::common::{TestDevice, TestUser};
use crate::governed_rooms::{
    device_endpoint, governance_since, join, member_device, publish, receive_refusing_until,
    revoke, role, room_heads, session, start,
};

/// A room Alice's relay governed until she revoked the relay's device:
/// her laptop and tablet and Bob's phone are members, each replica fed by
/// its own session with the relay, all holding the same room history.
struct Handover {
    alice: TestUser,
    bob: TestUser,
    host: TestDevice,
    laptop: TestDevice,
    tablet: TestDevice,
    phone: TestDevice,
    state: tempfile::TempDir,
    relay: Relay,
    room: RoomSummary,
    laptop_peer: StatePeer,
    tablet_peer: StatePeer,
    bob_peer: StatePeer,
    laptop_session: RelayCoeditSession,
    tablet_session: RelayCoeditSession,
    bob_session: RelayCoeditSession,
    revocation: RevocationEntry,
}

impl Handover {
    async fn new() -> Self {
        let alice = TestUser::fresh("Alice");
        let bob = TestUser::fresh("Bob");
        let host = alice.enrol("alice-home", DeviceCapabilities::host());
        let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
        let tablet = alice.enrol("alice-tablet", DeviceCapabilities::joined());
        let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        let relay = start(state.path(), &alice, &host).await;
        let address = relay.node_addr();
        let admin = RoomAdminClient::new(device_endpoint(&laptop).await, laptop.cert.clone());
        let room = admin.create(address.clone(), "Handover").await.unwrap();
        for device in [&laptop, &tablet] {
            RoomAdminClient::new(device_endpoint(device).await, device.cert.clone())
                .send(
                    address.clone(),
                    RoomCommand::Enter {
                        room: room.room.clone(),
                    },
                )
                .await
                .unwrap();
        }
        let bob_peer = join(&admin, &address, &room.room, &bob, &phone, 6).await;
        let fresh = || {
            StatePeer::new(
                &room.room,
                host.cert.clone(),
                vec![host.cert.clone()],
                RevocationList::new(),
                SystemTime::now(),
            )
            .unwrap()
        };
        let mut this = Self {
            laptop_peer: fresh(),
            tablet_peer: fresh(),
            bob_peer,
            laptop_session: session(&relay, &laptop, room.workspace_id).await,
            tablet_session: session(&relay, &tablet, room.workspace_id).await,
            bob_session: session(&relay, &phone, room.workspace_id).await,
            revocation: revoke(&alice, host.device_pubkey()),
            alice,
            bob,
            host,
            laptop,
            tablet,
            phone,
            state,
            relay,
            room,
        };
        let devices = [
            this.laptop.device_pubkey(),
            this.tablet.device_pubkey(),
            this.phone.device_pubkey(),
        ];
        let everyone =
            move |peer: &StatePeer| devices.iter().all(|device| member_device(peer, *device));
        for (what, session, peer) in [
            (
                "the laptop's replica",
                &this.laptop_session,
                &mut this.laptop_peer,
            ),
            (
                "the tablet's replica",
                &this.tablet_session,
                &mut this.tablet_peer,
            ),
            ("Bob's replica", &this.bob_session, &mut this.bob_peer),
        ] {
            receive_refusing_until(what, session, peer, everyone).await;
        }

        // Alice revokes her relay's device; it stops governing.
        tilo_relay::store::save_revocations(
            this.state.path(),
            std::slice::from_ref(&this.revocation),
        )
        .unwrap();
        tilo_test_env::within("the relay stops governing", async {
            while admin
                .send(
                    address.clone(),
                    RoomCommand::Enter {
                        room: this.room.room.clone(),
                    },
                )
                .await
                .is_ok()
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        // Every replica holds the history the relay wrote.
        let heads = sorted(room_heads(&this.bob_peer));
        for (what, session, peer) in [
            (
                "the laptop holds the room",
                &this.laptop_session,
                &mut this.laptop_peer,
            ),
            (
                "the tablet holds the room",
                &this.tablet_session,
                &mut this.tablet_peer,
            ),
        ] {
            receive_refusing_until(what, session, peer, |peer| {
                sorted(room_heads(peer)) == heads
            })
            .await;
        }
        this
    }

    /// The root's record handing the room to `successor`, vouching for the
    /// history every replica holds.
    fn succession(&self, successor: &TestDevice) -> GovernorSuccession {
        GovernorSuccession::sign(
            &self.room.room,
            self.host.cert.clone(),
            self.revocation.clone(),
            successor.cert.clone(),
            room_heads(&self.laptop_peer),
            &self.alice.user_sk,
        )
        .unwrap()
    }

    /// The laptop takes the room over from its own replica.
    fn laptop_governs(&self) -> (GovernorSuccession, StatePeer) {
        let succession = self.succession(&self.laptop);
        let mut governor = StatePeer::take_custody(
            &succession,
            [Custody::Replica(Box::new(self.laptop_peer.clone()))],
            SystemTime::now(),
        )
        .unwrap();
        governor
            .assume_governance(&succession, &self.laptop.sk, SystemTime::now())
            .unwrap();
        (succession, governor)
    }

    /// What the revoked relay would still sign: Bob's removal.
    fn rogue_removal(&self) -> Vec<NeighbourMsg> {
        let mut rogue = self.bob_peer.clone();
        let before = rogue.clone();
        rogue
            .remove_member(self.bob.user_pk, &self.host.sk, SystemTime::now())
            .unwrap();
        governance_since(&before, &mut rogue, &self.host.sk)
    }

    async fn close(self) {
        self.laptop_session.close().await;
        self.tablet_session.close().await;
        self.bob_session.close().await;
        self.relay.shutdown().await;
    }
}

/// A change the revoked relay could still send a member that has not
/// learned of its revocation: the room's owner replaced, written under an
/// actor id that is no governor's.
fn forged_owner(peer: &StatePeer) -> NeighbourMsg {
    use automerge::transaction::Transactable as _;
    let mut doc = peer.admitted_room().unwrap().clone();
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(vec![0xAB; 32]));
    doc.put(automerge::ROOT, "owner", hex::encode([0xAB; 32]))
        .unwrap();
    doc.commit();
    NeighbourMsg::SyncResp {
        changes: doc
            .get_changes(&heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        room: peer.room().to_owned(),
    }
}

/// `peer` after taking `message` from the revoked relay `host`.
fn misled(peer: &StatePeer, host: &TestDevice, message: NeighbourMsg) -> StatePeer {
    let mut peer = peer.clone();
    peer.receive_from(
        TransportOrigin::RetainedRelayDelta(host.device_pubkey()),
        message,
        SystemTime::now(),
    )
    .unwrap();
    peer
}

fn sorted(mut heads: Vec<[u8; 32]>) -> Vec<[u8; 32]> {
    heads.sort_unstable();
    heads
}

fn now_ns() -> u64 {
    tilo_clock::unix_now_ns()
}

fn from(device: &TestDevice) -> TransportOrigin {
    TransportOrigin::RelayDelta(device.device_pubkey())
}

/// Every message in `messages`, as `peer` takes them from `device`.
fn deliver(peer: &mut StatePeer, device: &TestDevice, messages: Vec<NeighbourMsg>) {
    for message in messages {
        peer.receive_from(from(device), message, SystemTime::now())
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_took_a_room_over_admits_invites_and_revokes() {
    let h = Box::pin(Handover::new()).await;
    let (_, mut governor) = h.laptop_governs();
    let mut bob = h.bob_peer.clone();
    deliver(&mut bob, &h.laptop, vec![governor.room_message()]);
    assert_eq!(bob.authority(), &h.laptop.cert);

    // The laptop invites Carol under its own authority, and admits the
    // device that redeems the invitation.
    let carol = TestUser::fresh("Carol");
    let carol_phone = carol.enrol("carol-phone", DeviceCapabilities::member());
    let grant = InviteGrant {
        workspace_id: h.room.workspace_id,
        room: h.room.room.clone(),
        authority: h.laptop.cert.clone(),
        issuer: h.laptop.cert.clone(),
        delegation: None,
        role: MemberRole::Editor,
        nonce: [7; 16],
        recipient_user_public_key: Some(carol.user_pk),
        invitee_name: None,
        expires_at_ns: now_ns() + 600_000_000_000,
        transport: Vec::new(),
    };
    let signed = issue_invitation(grant, &h.laptop.sk).unwrap();
    let verified = decode_and_verify_invitation(
        &signed,
        Some(carol.user_pk),
        &RevocationList::new(),
        now_ns(),
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &verified,
        &carol_phone.cert,
        [1; 32],
        [2; 32],
        now_ns() + 60_000_000_000,
        now_ns(),
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &carol_phone.sk).unwrap();
    let redemption = verify_join_proof(
        &proof,
        &challenge,
        &carol_phone.cert,
        &RevocationList::new(),
        now_ns(),
    )
    .unwrap();
    let before = governor.clone();
    assert!(
        governor
            .admit_invited_member(&redemption, &h.laptop.sk, SystemTime::now())
            .unwrap()
    );
    let admission = governance_since(&before, &mut governor, &h.laptop.sk);

    // Carol's replica is pinned to the laptop her invitation names, and
    // takes the room with the history the revoked relay wrote before it.
    let pinned_to = |device: &TestDevice| {
        StatePeer::new(
            &h.room.room,
            device.cert.clone(),
            vec![device.cert.clone()],
            RevocationList::new(),
            SystemTime::now(),
        )
        .unwrap()
    };
    let mut carol_peer = pinned_to(&h.laptop);
    deliver(&mut carol_peer, &h.laptop, vec![governor.room_message()]);
    assert_eq!(carol_peer.authority(), &h.laptop.cert);
    assert_eq!(
        role(&carol_peer, carol_phone.device_pubkey()),
        Some(collab_types::Role::Editor)
    );
    assert!(member_device(&carol_peer, h.phone.device_pubkey()));
    let restored = StatePeer::restore(
        &carol_peer.durable().unwrap(),
        &h.room.room,
        h.laptop.cert.clone(),
        vec![h.laptop.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    assert!(member_device(&restored, carol_phone.device_pubkey()));

    // A replica pinned to a device the records do not lead to refuses the
    // room, whoever sends it.
    let mut misled = pinned_to(&h.tablet);
    assert!(
        misled
            .receive_from(from(&h.tablet), governor.room_message(), SystemTime::now())
            .is_err(),
        "a room handed to another device is not the tablet's"
    );

    // Members take the admission.
    deliver(&mut bob, &h.laptop, admission);
    assert!(member_device(&bob, carol_phone.device_pubkey()));

    // The laptop admits another of Alice's devices.
    let phone = h.alice.enrol("alice-phone", DeviceCapabilities::joined());
    let before = governor.clone();
    assert!(
        governor
            .admit_certificate(phone.cert.clone(), &h.laptop.sk, SystemTime::now())
            .unwrap()
    );
    let admitted = governance_since(&before, &mut governor, &h.laptop.sk);
    deliver(&mut bob, &h.laptop, admitted.clone());
    deliver(&mut carol_peer, &h.laptop, admitted);
    for peer in [&bob, &carol_peer] {
        assert!(peer.certificates().contains(&phone.cert));
    }

    // The laptop publishes Alice's revocation of her tablet; every member
    // drops it.
    let revocation = revoke(&h.alice, h.tablet.device_pubkey());
    let before = governor.clone();
    assert!(
        governor
            .publish_governed_revocations(
                std::slice::from_ref(&revocation),
                &h.laptop.sk,
                SystemTime::now()
            )
            .unwrap()
    );
    let revoked = governance_since(&before, &mut governor, &h.laptop.sk);
    deliver(&mut bob, &h.laptop, revoked.clone());
    deliver(&mut carol_peer, &h.laptop, revoked);
    for peer in [&bob, &carol_peer] {
        assert!(peer.revocations().contains(&revocation));
        assert!(!member_device(peer, h.tablet.device_pubkey()));
        assert!(member_device(peer, carol_phone.device_pubkey()));
    }
    Box::pin(h.close()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_of_the_identity_takes_over_a_room_it_did_not_create() {
    let mut h = Box::pin(Handover::new()).await;
    let home = h.alice.enrol("alice-home-2", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &h.alice, &home).await;
    let address = relay.node_addr();
    let admin = RoomAdminClient::new(device_endpoint(&h.laptop).await, h.laptop.cert.clone());
    let succession = h.succession(&home);
    let replica = h.laptop_peer.durable().unwrap();

    // Refused: another person's device, a record naming another successor, a
    // record the root did not sign, and no source holding the history.
    let bob_admin = RoomAdminClient::new(device_endpoint(&h.phone).await, h.phone.cert.clone());
    assert!(
        bob_admin
            .take_over(address.clone(), succession.clone(), Some(replica.clone()))
            .await
            .is_err(),
        "only a device of the relay's identity hands it a room"
    );
    assert!(
        admin
            .take_over(
                address.clone(),
                h.succession(&h.laptop),
                Some(replica.clone())
            )
            .await
            .is_err(),
        "a record naming another successor"
    );
    let mut forged = succession.clone();
    forged.cutoff = vec![[1; 32]];
    assert!(
        admin
            .take_over(address.clone(), forged, Some(replica.clone()))
            .await
            .is_err(),
        "a record the root did not sign"
    );
    assert!(
        admin
            .take_over(address.clone(), succession.clone(), None)
            .await
            .is_err(),
        "the relay retains nothing of a room it never hosted"
    );
    let revoked = RoomAdminClient::new(device_endpoint(&h.host).await, h.host.cert.clone());
    assert!(
        revoked
            .take_over(address.clone(), succession.clone(), Some(replica.clone()))
            .await
            .is_err(),
        "the revoked governor hands nothing over, though this relay never learned its revocation"
    );
    let forged = misled(&h.laptop_peer, &h.host, forged_owner(&h.laptop_peer));
    assert!(
        admin
            .take_over(
                address.clone(),
                succession.clone(),
                Some(forged.durable().unwrap())
            )
            .await
            .is_err(),
        "a replica holding governance written past the cutoff"
    );

    // The laptop hands the new relay the room with its replica.
    let summary = admin
        .take_over(address.clone(), succession.clone(), Some(replica))
        .await
        .unwrap();
    assert_eq!(summary.room, h.room.room);
    assert_eq!(summary.name, "Handover");
    assert!(
        admin
            .take_over(
                address.clone(),
                succession.clone(),
                Some(h.laptop_peer.durable().unwrap())
            )
            .await
            .is_err(),
        "a room the relay governs is not taken over twice"
    );
    assert!(matches!(
        admin
            .send(
                address.clone(),
                RoomCommand::Enter {
                    room: h.room.room.clone(),
                },
            )
            .await
            .unwrap(),
        RoomReply::Entered { .. }
    ));

    // It invites and admits a new member, whose replica is pinned to it.
    let carol = TestUser::fresh("Carol");
    let carol_phone = carol.enrol("carol-phone", DeviceCapabilities::member());
    let carol_peer = join(&admin, &address, &h.room.room, &carol, &carol_phone, 9).await;
    assert_eq!(carol_peer.authority(), &home.cert);
    assert!(member_device(&carol_peer, carol_phone.device_pubkey()));
    assert!(member_device(&carol_peer, h.phone.device_pubkey()));

    // Bob, still pinned to the revoked relay, follows the new one from what
    // it publishes, and refuses the revoked relay's governance.
    let removal = h.rogue_removal();
    let bob_session = session(&relay, &h.phone, h.room.workspace_id).await;
    receive_refusing_until(
        "Bob follows the new relay",
        &bob_session,
        &mut h.bob_peer,
        |peer| peer.authority() == &home.cert && member_device(peer, carol_phone.device_pubkey()),
    )
    .await;
    assert!(h.bob_peer.revocations().contains(&h.revocation));
    for message in removal {
        assert!(
            h.bob_peer
                .receive_from(
                    TransportOrigin::RetainedRelayDelta(h.host.device_pubkey()),
                    message,
                    SystemTime::now()
                )
                .is_err()
        );
    }

    // After a restart the new relay still governs the room.
    bob_session.close().await;
    relay.shutdown().await;
    let relay = start(state.path(), &h.alice, &home).await;
    let address = relay.node_addr();
    assert!(matches!(
        admin.send(address.clone(), RoomCommand::List).await.unwrap(),
        RoomReply::Rooms(rooms) if rooms.iter().any(|room| room.room == h.room.room)
    ));
    admin
        .send(
            address.clone(),
            RoomCommand::Enter {
                room: h.room.room.clone(),
            },
        )
        .await
        .unwrap();
    relay.shutdown().await;
    Box::pin(h.close()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_takeover_bind_leaves_no_governed_room_and_can_retry() {
    let h = Box::pin(Handover::new()).await;
    let home = h
        .alice
        .enrol("alice-home-bind-retry", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &h.alice, &home).await;
    let address = relay.node_addr();
    let admin = RoomAdminClient::new(device_endpoint(&h.laptop).await, h.laptop.cert.clone());
    let succession = h.succession(&home);
    let replica = h.laptop_peer.durable().unwrap();
    let workspaces = state.path().join("workspaces");
    std::fs::create_dir_all(&workspaces).unwrap();
    let blocked = workspaces.join(h.room.workspace_id.to_string());
    std::fs::write(&blocked, b"test-owned bind failure").unwrap();
    assert!(
        admin
            .take_over(address.clone(), succession.clone(), Some(replica.clone()))
            .await
            .is_err()
    );
    assert!(
        matches!(admin.send(address, RoomCommand::List).await.unwrap(), RoomReply::Rooms(rooms) if rooms.is_empty())
    );
    assert!(
        !state
            .path()
            .join("rooms")
            .join(format!("{}.room", h.room.room))
            .exists(),
        "a failed bind must remove the persisted room before restart"
    );
    std::fs::remove_file(blocked).unwrap();
    relay.shutdown().await;
    let relay = start(state.path(), &h.alice, &home).await;
    let address = relay.node_addr();
    assert!(
        matches!(admin.send(address.clone(), RoomCommand::List).await.unwrap(), RoomReply::Rooms(rooms) if rooms.is_empty()),
        "a refused bind must not silently acquire governance on restart"
    );
    let taken = admin
        .take_over(address.clone(), succession, Some(replica))
        .await
        .unwrap();
    assert_eq!(taken.room, h.room.room);
    assert!(
        matches!(admin.send(address, RoomCommand::List).await.unwrap(), RoomReply::Rooms(rooms) if rooms.len() == 1 && rooms[0].room == h.room.room)
    );
    relay.shutdown().await;
    Box::pin(h.close()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_follow_a_successor_they_cannot_reach_through_any_member() {
    let mut h = Box::pin(Handover::new()).await;
    let (succession, mut governor) = h.laptop_governs();
    let removal = h.rogue_removal();
    let tablet_before = h.tablet_peer.clone();

    // Only Bob reaches the laptop. He follows it and relays the record
    // through the revoked relay, which still hosts the room.
    deliver(&mut h.bob_peer, &h.laptop, vec![governor.room_message()]);
    let relayed = h.bob_peer.succession_message().unwrap().unwrap();
    publish(&h.bob_session, &relayed);

    // The tablet, which never hears from the laptop, follows it from Bob's
    // relay: it holds the revocation and refuses the revoked relay.
    receive_refusing_until(
        "the tablet follows the laptop through Bob",
        &h.tablet_session,
        &mut h.tablet_peer,
        |peer| peer.authority() == &h.laptop.cert,
    )
    .await;
    assert!(h.tablet_peer.revocations().contains(&h.revocation));
    for message in removal.clone() {
        assert!(
            h.tablet_peer
                .receive_from(
                    TransportOrigin::RetainedRelayDelta(h.host.device_pubkey()),
                    message,
                    SystemTime::now()
                )
                .is_err(),
            "the revoked relay's governance is refused"
        );
    }
    // It relays the record further, and keeps following across a restart.
    assert_eq!(
        h.tablet_peer.succession_message().unwrap(),
        Some(relayed.clone())
    );
    let restored = StatePeer::restore(
        &h.tablet_peer.durable().unwrap(),
        &h.room.room,
        h.host.cert.clone(),
        vec![h.host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    assert_eq!(restored.authority(), &h.laptop.cert);

    // A replica that follows from the records alone, and a relay's retention
    // that holds them, are both still custody of the room.
    StatePeer::take_custody(
        &succession,
        [Custody::Replica(Box::new(h.tablet_peer.clone()))],
        SystemTime::now(),
    )
    .unwrap();
    StatePeer::take_custody(
        &succession,
        [Custody::Retained(vec![
            (h.phone.device_pubkey(), relayed.clone()),
            (h.host.device_pubkey(), h.laptop_peer.room_message()),
        ])],
        SystemTime::now(),
    )
    .unwrap();

    // A member that took governance the revoked relay wrote past the cutoff
    // under another actor id follows neither the records nor the laptop.
    let forged = misled(&tablet_before, &h.host, forged_owner(&tablet_before));
    for (what, origin, message) in [
        ("relayed records", from(&h.phone), relayed.clone()),
        (
            "the laptop's room",
            from(&h.laptop),
            governor.room_message(),
        ),
    ] {
        let mut member = forged.clone();
        assert!(
            member
                .receive_from(origin, message, SystemTime::now())
                .is_err(),
            "{what}"
        );
        assert_eq!(member.authority(), &h.host.cert, "{what}");
    }

    // Records no replica follows, whoever relays them.
    let relay_of = |records: Vec<GovernorSuccession>| NeighbourMsg::GovernorSuccession {
        room: h.room.room.clone(),
        records: records
            .iter()
            .map(|record| postcard::to_stdvec(record).unwrap())
            .collect(),
    };
    let mut forged = succession.clone();
    forged.successor = h.tablet.cert.clone();
    let carol = TestUser::fresh("Carol");
    let carol_home = carol.enrol("carol-home", DeviceCapabilities::host());
    let foreign = GovernorSuccession::sign(
        &h.room.room,
        carol_home.cert.clone(),
        revoke(&carol, carol_home.device_pubkey()),
        carol
            .enrol("carol-laptop", DeviceCapabilities::primary())
            .cert,
        succession.cutoff.clone(),
        &carol.user_sk,
    )
    .unwrap();
    for (what, record) in [
        ("a forged record", forged),
        ("another identity's record", foreign),
    ] {
        let mut member = tablet_before.clone();
        let _ = member.receive_from(from(&h.phone), relay_of(vec![record]), SystemTime::now());
        assert_eq!(member.authority(), &h.host.cert, "{what}");
    }
    let mut member = tablet_before.clone();
    assert!(
        member
            .receive_from(
                from(&h.phone),
                NeighbourMsg::GovernorSuccession {
                    room: uuid::Uuid::new_v4().to_string(),
                    records: match relayed.clone() {
                        NeighbourMsg::GovernorSuccession { records, .. } => records,
                        _ => unreachable!(),
                    },
                },
                SystemTime::now()
            )
            .is_err(),
        "records of another room"
    );
    assert_eq!(member.authority(), &h.host.cert);

    // A member that took the revoked relay's governance past the cutoff
    // does not follow.
    let mut lagging = tablet_before.clone();
    for message in removal.clone() {
        lagging
            .receive_from(
                TransportOrigin::RetainedRelayDelta(h.host.device_pubkey()),
                message,
                SystemTime::now(),
            )
            .unwrap();
    }
    assert!(
        lagging
            .receive_from(from(&h.phone), relayed.clone(), SystemTime::now())
            .is_err()
    );
    assert_eq!(lagging.authority(), &h.host.cert);

    // A second root-signed successor relayed to a following member contests
    // the room.
    let rival = h.succession(&h.tablet);
    let mut contested = h.tablet_peer.clone();
    assert!(
        contested
            .receive_from(from(&h.phone), relay_of(vec![rival]), SystemTime::now())
            .is_err()
    );
    assert!(contested.contested_succession().is_some());
    assert!(
        contested
            .receive_from(from(&h.laptop), governor.room_message(), SystemTime::now())
            .is_err(),
        "a contested room takes governance from neither successor"
    );

    // Once the laptop's room reaches the tablet, the tablet takes it and its
    // history records the succession; the laptop's later governance follows.
    deliver(&mut h.tablet_peer, &h.laptop, vec![governor.room_message()]);
    assert_eq!(
        succession::recorded(
            h.tablet_peer.admitted_room().unwrap(),
            &h.host.device_pubkey()
        )
        .unwrap(),
        vec![succession]
    );
    let before = governor.clone();
    governor
        .set_member_role(
            h.bob.user_pk,
            MemberRole::Viewer,
            &h.laptop.sk,
            SystemTime::now(),
        )
        .unwrap();
    deliver(
        &mut h.tablet_peer,
        &h.laptop,
        governance_since(&before, &mut governor, &h.laptop.sk),
    );
    assert_eq!(
        role(&h.tablet_peer, h.phone.device_pubkey()),
        Some(collab_types::Role::Viewer)
    );
    let restored = StatePeer::restore(
        &h.tablet_peer.durable().unwrap(),
        &h.room.room,
        h.host.cert.clone(),
        vec![h.host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    assert_eq!(restored.authority(), &h.laptop.cert);
    Box::pin(h.close()).await;
}
