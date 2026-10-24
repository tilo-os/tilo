//! The prerequisite queue holds an app message while what it waits for is
//! still in flight. It is bounded, and it never refuses the input being
//! admitted: a held message whose prerequisite does not come is let go with
//! why, and the room goes on.
use super::*;
use crate::DropReason;
use crate::admission_tests::seeded;
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;

fn incremental(
    author: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
    name: &str,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, name, name).unwrap();
    doc.commit_edit("fixture", "fixture");
    author
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), key, now)
        .unwrap()
}

/// `message` as `key`'s write at `seq`: signed by a device the room never
/// certifies, so the certificate it waits for never comes.
fn as_author(message: &NeighbourMsg, key: &SigningKey, seq: u64) -> NeighbourMsg {
    let mut message = message.clone();
    if let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut message {
        *peer = peer_handle(&key.verifying_key().to_bytes());
        envelope.author =
            collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        envelope.seq = seq;
        collab_types::sign_op(envelope, key).unwrap();
    }
    message
}

fn value(peer: &StatePeer, name: &str) -> Option<String> {
    let doc = AutoCommit::load(&peer.document("test.app", "tab:1")?).ok()?;
    doc.get(ROOT, name)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

#[test]
fn a_held_message_whose_prerequisite_never_comes_is_let_go_with_why_and_the_room_goes_on() {
    let (mut receiver, key, _, _, now) = seeded();
    receiver.select_admission_v5().unwrap();
    let mut author = receiver.clone();
    let write = incremental(&mut author, &key, now, "kept");
    let stranger = SigningKey::from_bytes(&[77; 32]);
    let origin = TransportOrigin::RelayDelta(stranger.verifying_key().to_bytes());
    let first = receiver
        .receive_from_report(origin, as_author(&write, &stranger, 1), now)
        .unwrap();
    assert_eq!(
        first.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::Certificate)
    );
    // Fill the queue behind it: every one is held, none refused.
    for seq in 2..=u64::try_from(MAX_HELD_INPUTS).unwrap() {
        let report = receiver
            .receive_from_report(origin, as_author(&write, &stranger, seq), now)
            .unwrap();
        assert!(report.summary().dropped.is_empty());
    }
    assert_eq!(receiver.pending_count(), MAX_HELD_INPUTS);
    let next = u64::try_from(MAX_HELD_INPUTS).unwrap() + 1;
    let report = receiver
        .receive_from_report(origin, as_author(&write, &stranger, next), now)
        .expect("a full prerequisite queue must never refuse the input being admitted");
    assert_eq!(
        report.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::Certificate)
    );
    let [dropped] = report.summary().dropped.as_slice() else {
        panic!(
            "the oldest held message is let go: {:?}",
            report.summary().dropped
        );
    };
    assert_eq!(dropped.author, stranger.verifying_key().to_bytes());
    assert_eq!(dropped.sequence, 1, "the message held longest goes first");
    assert_eq!(dropped.waiting_for, Some(MissingPrerequisite::Certificate));
    assert_eq!(dropped.reason, DropReason::Superseded);
    assert!(
        dropped.to_string().contains("its author's certificate"),
        "the reason names what it waited for: {dropped}"
    );
    assert_eq!(receiver.pending_count(), MAX_HELD_INPUTS);
    // The queue survives a restart within its bounds.
    let restored = StatePeer::restore(
        &receiver.durable().unwrap(),
        receiver.room(),
        receiver.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(restored.pending_count(), MAX_HELD_INPUTS);
    // A member's write is still admitted, and nothing let go was applied.
    let admitted = receiver
        .receive_from_report(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            write,
            now,
        )
        .unwrap();
    assert!(matches!(admitted.primary(), InputDisposition::Accepted(_)));
    assert_eq!(value(&receiver, "kept").as_deref(), Some("kept"));
}

#[test]
fn a_held_message_whose_prerequisite_arrived_is_applied_when_its_place_is_needed() {
    let (mut receiver, owner_key, member_key, member, now) = seeded();
    receiver.select_admission_v5().unwrap();
    let mut author = receiver.clone();
    // The member's certificate has not reached this replica yet.
    receiver.core.certificates.remove(&member.device_public_key);
    let write = incremental(&mut author, &member_key, now, "late");
    let held = receiver
        .receive_from_report(
            TransportOrigin::RelayDelta(member.device_public_key),
            write,
            now,
        )
        .unwrap();
    assert_eq!(
        held.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::Certificate)
    );
    // Its certificate arrives on its own, which retries nothing.
    receiver
        .receive_from(
            TransportOrigin::UntrustedPresence(member.device_public_key),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(member.clone()),
                revocations: vec![],
                successions: Vec::new(),
            },
            now,
        )
        .unwrap();
    assert_eq!(receiver.pending_count(), 1);
    let stranger = SigningKey::from_bytes(&[78; 32]);
    let filler = incremental(&mut receiver.clone(), &owner_key, now, "filler");
    let origin = TransportOrigin::RelayDelta(stranger.verifying_key().to_bytes());
    let mut settled = Vec::new();
    for seq in 1..=u64::try_from(MAX_HELD_INPUTS).unwrap() {
        let report = receiver
            .receive_from_report(origin, as_author(&filler, &stranger, seq), now)
            .unwrap();
        assert!(
            report.summary().dropped.is_empty(),
            "a held message that can be decided now is not let go: {:?}",
            report.summary().dropped
        );
        settled.extend(report.settled().iter().cloned());
    }
    assert_eq!(value(&receiver, "late").as_deref(), Some("late"));
    assert!(
        settled
            .iter()
            .any(|admission| admission.key.author == member.device_public_key),
        "its admission is reported settled"
    );
    assert_eq!(receiver.pending_count(), MAX_HELD_INPUTS);
}

#[test]
fn a_message_let_go_is_asked_for_again_once_what_it_waited_for_lands_and_then_applies() {
    let (mut receiver, key, _, _, now) = seeded();
    receiver.select_admission_v5().unwrap();
    let mut author = receiver.clone();
    let first = incremental(&mut author, &key, now, "first");
    let second = incremental(&mut author, &key, now, "second");
    let member = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let held = receiver
        .receive_from_report(member, second.clone(), now)
        .unwrap();
    assert_eq!(
        held.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::CausalDependencies)
    );
    // The queue fills behind it, and the change it builds on has not come.
    let stranger = SigningKey::from_bytes(&[79; 32]);
    let origin = TransportOrigin::RelayDelta(stranger.verifying_key().to_bytes());
    let mut let_go = Vec::new();
    for seq in 1..=u64::try_from(MAX_HELD_INPUTS).unwrap() {
        let report = receiver
            .receive_from_report(origin, as_author(&first, &stranger, seq), now)
            .unwrap();
        assert!(
            !report.summary().replay_wanted,
            "nothing it waited for landed yet"
        );
        let_go.extend(report.summary().dropped.iter().cloned());
    }
    assert!(
        let_go
            .iter()
            .any(|input| input.author == key.verifying_key().to_bytes()
                && input.waiting_for == Some(MissingPrerequisite::CausalDependencies)),
        "the member's change was let go: {let_go:?}"
    );
    assert_eq!(value(&receiver, "second"), None);
    // What it built on arrives: the room's host is asked to replay.
    let landed = receiver.receive_from_report(member, first, now).unwrap();
    assert!(matches!(landed.primary(), InputDisposition::Accepted(_)));
    assert!(
        landed.summary().replay_wanted,
        "a message let go must not be gone for good: its prerequisite landed, so it is asked for again"
    );
    // The host's replay delivers it again, and now it applies.
    let replayed = receiver.receive_from_report(member, second, now).unwrap();
    assert!(matches!(replayed.primary(), InputDisposition::Accepted(_)));
    assert_eq!(value(&receiver, "second").as_deref(), Some("second"));
    // Asked once: what landed is forgotten.
    let again = receiver
        .receive_from_report(
            origin,
            as_author(&incremental(&mut author, &key, now, "x"), &stranger, 900),
            now,
        )
        .unwrap();
    assert!(!again.summary().replay_wanted);
}
