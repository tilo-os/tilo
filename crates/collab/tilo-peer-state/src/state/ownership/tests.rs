use super::super::{NeighbourMsg, SigningKey, StatePeer, SystemTime, TransportOrigin};
use crate::handoff::tests::{Parties, ROOM};
use crate::handoff::{HandoffError, OWNERSHIP_OFFER_KEY};
use automerge::{ROOT, ScalarValue, transaction::Transactable as _};
use tilo_user_identity::{DeviceCert, RevocationList};

/// The governor's copy of the room, as the owner's device `governor` made
/// it, and a member's copy that has every governance change of it.
fn room(parties: &Parties, now: SystemTime) -> (StatePeer, StatePeer) {
    room_of(parties, parties.roster(), now)
}

/// [`room`], its roster `roster`.
fn room_of(parties: &Parties, roster: Vec<DeviceCert>, now: SystemTime) -> (StatePeer, StatePeer) {
    let mut governor = StatePeer::new(
        ROOM,
        parties.governor.cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    governor
        .install_seed(
            super::super::workspace_seed(ROOM, &parties.governor.cert, &roster, "Room", 7).unwrap(),
            &parties.governor.key,
            now,
        )
        .unwrap();
    let mut member = StatePeer::new(
        ROOM,
        parties.governor.cert.clone(),
        roster,
        RevocationList::new(),
        now,
    )
    .unwrap();
    sync(&governor, &mut member, &parties.governor.key, now).unwrap();
    (governor, member)
}

/// Deliver every governance change `from` holds to `to`, as sent by `key`.
fn sync(
    from: &StatePeer,
    to: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
) -> Result<(), String> {
    let changes = from
        .admitted_room()
        .unwrap()
        .clone()
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    to.receive_from(
        TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        NeighbourMsg::SyncResp {
            room: ROOM.into(),
            changes,
        },
        now,
    )
    .map(|_| ())
}

/// A governance change `key` writes into `peer`'s room: setting `field`.
fn write(
    peer: &mut StatePeer,
    key: &SigningKey,
    field: &str,
    now: SystemTime,
) -> Result<(), String> {
    let mut doc = peer.admitted_room().unwrap().clone();
    let before = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    doc.put(ROOT, field, ScalarValue::Bytes(vec![1, 2, 3]))
        .unwrap();
    doc.commit();
    let changes = doc
        .get_changes(&before)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        NeighbourMsg::SyncResp {
            room: ROOM.into(),
            changes,
        },
        now,
    )
    .map(|_| ())
}

fn owner_of(peer: &StatePeer) -> Option<String> {
    tilo_desktop_model::projection::room_policy_of(peer.admitted_room().unwrap(), ROOM).owner
}

fn person(cert: &DeviceCert) -> String {
    hex::encode(cert.user_public_key)
}

#[test]
fn a_member_follows_the_handoff_to_the_recipients_device() {
    let now = SystemTime::now();
    let parties = Parties::new();
    let (mut governor, mut member) = room(&parties, now);
    let offer = parties.offer(1);
    assert!(
        governor
            .governance_mut()
            .offer_ownership(&offer, &parties.governor.key, now)
            .unwrap()
    );
    sync(&governor, &mut member, &parties.governor.key, now).unwrap();
    assert_eq!(member.governance().ownership_offer(), Some(offer.clone()));

    let handoff = parties.handoff(&offer, 2);
    assert!(
        governor
            .governance_mut()
            .commit_handoff(&handoff, &parties.governor.key, now)
            .unwrap()
    );
    assert_eq!(governor.governance().handed_on(), Some(handoff.clone()));
    assert_eq!(governor.governance().ownership_offer(), None);
    assert_eq!(owner_of(&governor), Some(person(&parties.successor.cert)));
    let previous =
        tilo_desktop_model::projection::room_policy_of(governor.admitted_room().unwrap(), ROOM);
    assert_eq!(
        previous.members.get(&person(&parties.governor.cert)),
        Some(&tilo_action_policy::Role::Owner),
        "the previous owner stays as an administrator"
    );

    sync(&governor, &mut member, &parties.governor.key, now).unwrap();
    assert_eq!(member.follow_handoffs().unwrap(), vec![handoff]);
    assert_eq!(member.authority(), &parties.successor.cert);
    assert!(
        member.follow_handoffs().unwrap().is_empty(),
        "followed once"
    );
    write(&mut member, &parties.governor.key, OWNERSHIP_OFFER_KEY, now)
        .expect_err("the previous governor governs no more");
    write(
        &mut member,
        &parties.successor.key,
        OWNERSHIP_OFFER_KEY,
        now,
    )
    .unwrap();

    // Restored, the member's copy is the successor's room.
    let restored = StatePeer::restore(
        &member.durable().unwrap(),
        ROOM,
        parties.successor.cert.clone(),
        parties.roster(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(owner_of(&restored), Some(person(&parties.successor.cert)));
}

#[test]
fn a_member_with_a_wrong_clock_still_follows_the_handoff() {
    // The successor's certificate expired long ago by the member's clock,
    // and the offer too: a handoff valid when committed stays valid.
    let parties = Parties::with_successor_expiry(Some(40));
    let now = SystemTime::now();
    // Its expired certificate is on no roster: it reaches the member only
    // through the handoff.
    let roster = vec![
        parties.governor.cert.clone(),
        parties.proposer.cert.clone(),
        parties.member.cert.clone(),
    ];
    let (mut governor, _) = room_of(&parties, roster.clone(), now);
    let offer = parties.offer(1);
    governor
        .governance_mut()
        .offer_ownership(&offer, &parties.governor.key, now)
        .unwrap();
    let handoff = parties.handoff(&offer, 2);
    governor
        .governance_mut()
        .commit_handoff(&handoff, &parties.governor.key, now)
        .unwrap();
    for clock in [
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(60),
        now + std::time::Duration::from_secs(400 * 365 * 24 * 3600),
    ] {
        let mut member = StatePeer::new(
            ROOM,
            parties.governor.cert.clone(),
            roster.clone(),
            RevocationList::new(),
            clock,
        )
        .unwrap();
        sync(&governor, &mut member, &parties.governor.key, clock).unwrap();
        assert_eq!(member.follow_handoffs().unwrap(), vec![handoff.clone()]);
        assert_eq!(member.authority(), &parties.successor.cert);
    }
}

#[test]
fn a_new_offer_replaces_the_open_one_and_its_acceptance_is_refused() {
    let now = SystemTime::now();
    let parties = Parties::new();
    let (mut governor, _) = room(&parties, now);
    let first = parties.offer(1);
    let second = parties.offer(2);
    for offer in [&first, &second] {
        governor
            .governance_mut()
            .offer_ownership(offer, &parties.governor.key, now)
            .unwrap();
    }
    assert_eq!(governor.governance().ownership_offer(), Some(second));
    let replaced = parties.handoff(&first, 2);
    assert!(matches!(
        governor
            .governance_mut()
            .commit_handoff(&replaced, &parties.governor.key, now),
        Err(super::OwnershipWriteError::Handoff(
            HandoffError::WrongOffer
        ))
    ));
    assert_eq!(owner_of(&governor), Some(person(&parties.governor.cert)));
}

#[test]
fn a_withdrawn_offer_changes_nothing() {
    let now = SystemTime::now();
    let parties = Parties::new();
    let (mut governor, _) = room(&parties, now);
    let offer = parties.offer(1);
    governor
        .governance_mut()
        .offer_ownership(&offer, &parties.governor.key, now)
        .unwrap();
    assert!(
        governor
            .governance_mut()
            .withdraw_ownership_offer(offer.offer_id, &parties.governor.key, now)
            .unwrap()
    );
    assert_eq!(governor.governance().ownership_offer(), None);
    assert!(
        !governor
            .governance_mut()
            .withdraw_ownership_offer(offer.offer_id, &parties.governor.key, now)
            .unwrap(),
        "withdrawn already"
    );
    assert!(matches!(
        governor.governance_mut().commit_handoff(
            &parties.handoff(&offer, 2),
            &parties.governor.key,
            now
        ),
        Err(super::OwnershipWriteError::Handoff(
            HandoffError::WrongOffer
        ))
    ));
    assert_eq!(owner_of(&governor), Some(person(&parties.governor.cert)));
    assert!(governor.governance().governor_handoffs().is_empty());
}

#[test]
fn a_handed_off_governor_records_no_more_offers_or_handoffs() {
    let now = SystemTime::now();
    let parties = Parties::new();
    let (mut governor, _) = room(&parties, now);
    let offer = parties.offer(1);
    governor
        .governance_mut()
        .offer_ownership(&offer, &parties.governor.key, now)
        .unwrap();
    let handoff = parties.handoff(&offer, 2);
    governor
        .governance_mut()
        .commit_handoff(&handoff, &parties.governor.key, now)
        .unwrap();
    assert!(matches!(
        governor
            .governance_mut()
            .commit_handoff(&handoff, &parties.governor.key, now),
        Err(super::OwnershipWriteError::Handoff(HandoffError::Moved))
    ));
    assert!(matches!(
        governor
            .governance_mut()
            .offer_ownership(&parties.offer(3), &parties.governor.key, now),
        Err(super::OwnershipWriteError::Handoff(HandoffError::Moved))
    ));
}

#[test]
fn nobody_but_the_governor_writes_an_offer_or_a_handoff() {
    let now = SystemTime::now();
    let parties = Parties::new();
    let (_, mut member) = room(&parties, now);
    for field in [OWNERSHIP_OFFER_KEY, crate::handoff::GOVERNOR_HANDOFFS_KEY] {
        write(&mut member, &parties.member.key, field, now)
            .expect_err("a member's write of the room's ownership");
        write(&mut member, &parties.proposer.key, field, now)
            .expect_err("the owner's other device writes through the governor");
    }
    assert!(
        member
            .governance_mut()
            .offer_ownership(&parties.offer(1), &parties.member.key, now)
            .is_err()
    );
}

#[test]
fn a_record_is_covered_by_the_new_governor_only_along_the_handoff_log() {
    let now = SystemTime::now();
    let parties = Parties::new();
    let (mut governor, _) = room(&parties, now);
    let offer = parties.offer(1);
    governor
        .governance_mut()
        .offer_ownership(&offer, &parties.governor.key, now)
        .unwrap();
    governor
        .governance_mut()
        .commit_handoff(&parties.handoff(&offer, 2), &parties.governor.key, now)
        .unwrap();
    let doc = governor.admitted_room().unwrap();
    let key = |cert: &DeviceCert| cert.device_public_key;
    let (old, new, member) = (
        key(&parties.governor.cert),
        key(&parties.successor.cert),
        key(&parties.member.cert),
    );
    assert!(crate::handoff::handed_between(doc, &old, &new));
    assert!(!crate::handoff::handed_between(doc, &new, &old));
    assert!(!crate::handoff::handed_between(doc, &member, &new));
    assert!(!crate::handoff::handed_between(doc, &old, &member));
}
