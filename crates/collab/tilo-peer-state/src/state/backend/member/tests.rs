use super::*;
use crate::PeerLimits;
use crate::StatePeer;
use crate::state::backend::restore::tests::restore_bounded;
use crate::state::err;
use crate::state::now_ns;
use crate::verified_certificate::VerifiedCertificate;
use automerge::ObjType;
use automerge::ROOT;
use automerge::ReadDoc;
use automerge::transaction::Transactable;
use collab_types::Ed25519PublicKey;
use collab_types::caps::Principal;
use ed25519_dalek::SigningKey;
use tilo_action_policy::{
    ActionKind, Answer, GrantId, KindSet, Lifetime, Subject, TreePath, roles,
};
use tilo_automerge::ApplyIncoming;
use web_time::Duration;

fn put_member(peer: &mut StatePeer, person: &str, role: Role) {
    let room = peer.core.room_doc.as_mut().unwrap();
    let members = room.get(ROOT, "members").unwrap().unwrap().1;
    room.put(
        &members,
        person,
        automerge::ScalarValue::Bytes(serde_json::to_vec(&role).unwrap()),
    )
    .unwrap();
    room.commit();
}
fn put_grant(peer: &mut StatePeer, grant: &Grant) {
    let room = peer.core.room_doc.as_mut().unwrap();
    let grants = room.get(ROOT, "grants").unwrap().unwrap().1;
    room.put(
        &grants,
        grant.id.as_str(),
        automerge::ScalarValue::Bytes(serde_json::to_vec(grant).unwrap()),
    )
    .unwrap();
    room.commit();
}
fn fixture() -> (StatePeer, [u8; 32], SystemTime) {
    let (mut peer, _, _, cert, now) = crate::admission_tests::seeded();
    let room = peer.core.room_doc.as_mut().unwrap();
    room.put(ROOT, "owner", "a different person").unwrap();
    room.commit();
    put_member(&mut peer, &hex::encode(cert.user_public_key), Role::Editor);
    (peer, cert.device_public_key, now)
}
#[test]
fn bounded_current_member_matches_actual_legacy_membership_and_rechecks_removal() {
    let (mut peer, source, now) = fixture();
    let bytes = peer.durable().unwrap();
    let expected = peer.permissions(source, now).unwrap();
    assert_eq!(expected.role, Role::Editor);
    let restored = restore_bounded(&peer, now, &PeerLimits::default());
    let read = restored.read();
    let member = read.member(source, now).unwrap();
    assert_eq!(member.role(), expected.role);
    assert_eq!(member.grants(), expected.policy.grants.as_slice());
    assert_eq!(member.person(), expected.person);
    assert_eq!(peer.durable().unwrap(), bytes);
    // A blocked member is no longer current on either path.
    let block = expected
        .policy
        .block(&expected.person, Subject::person("a different person"));
    put_grant(&mut peer, &block);
    assert!(peer.permissions(source, now).is_err());
    let restored = restore_bounded(&peer, now, &PeerLimits::default());
    assert!(restored.read().member(source, now).is_err());
}
// Literal pre-extraction role body, with the historical projection oracle.
fn original_role(peer: &StatePeer, source: &[u8; 32], now: SystemTime) -> Result<Role, String> {
    let cert = peer.certificate(source, now)?;
    let room = peer
        .core
        .room_doc
        .as_ref()
        .ok_or("room governance missing")?;
    let (_, room_policy) = crate::room::extract_governed_fields(room);
    let identities = crate::room::governance_of(Some(room)).identities;
    let policy = crate::cached_document::PolicyData {
        owner: room_policy.owner.clone(),
        members: room_policy.members.clone(),
        grants: room_policy.grants.clone(),
        identities,
    };
    // A root-signed certificate must also be the certificate admitted by room governance.
    let identity: GovernedIdentity = serde_json::from_slice(
        policy
            .identities
            .get(&hex::encode(source))
            .ok_or("device is absent from governed roster")?,
    )
    .map_err(err)?;
    if identity.device_cert != *cert {
        return Err("governed certificate mismatch".into());
    }
    let mut revoked = peer.core.revocations.clone();
    for entry in identity.revocations {
        if entry.user_public_key != cert.user_public_key {
            return Err("revocation root mismatch".into());
        }
        revoked.insert(entry).map_err(err)?;
    }
    if revoked.is_revoked(source) {
        return Err("device revoked by room".into());
    }
    let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(cert.user_public_key));
    room_policy
        .active_role(person.id())
        .ok_or_else(|| "person has no room membership".into())
}

fn compare_native(peer: &StatePeer, source: [u8; 32], now: SystemTime) {
    assert_eq!(peer.role(&source, now), original_role(peer, &source, now));
    let expected = peer.permissions(source, now);
    let restored = restore_bounded(peer, now, &PeerLimits::default());
    let view = restored.read();
    let actual = view.member(source, now);
    match (expected, actual) {
        (Ok(expected), Ok(member)) => {
            assert_eq!(member.role(), expected.role);
            assert_eq!(member.grants(), expected.policy.grants.as_slice());
            assert_eq!(member.person(), expected.person);
            assert_eq!(member.foreign(), expected.foreign);
        }
        (Err(_), Err(_)) => {}
        (expected, actual) => panic!(
            "membership differs: legacy={expected:?} native={:?}",
            actual.as_ref().err()
        ),
    }
}
#[test]
fn bounded_current_member_preserves_malformed_container_and_conflict_winner_semantics() {
    for kind in [ObjType::List, ObjType::Text, ObjType::Map] {
        let (mut peer, source, now) = fixture();
        let owner = hex::encode(peer.core.authority.user_public_key);
        let room = peer.core.room_doc.as_mut().unwrap();
        let object = room.put_object(ROOT, "members", kind).unwrap();
        if kind == ObjType::List {
            room.insert(&object, 0, "not a map entry").unwrap();
        } else if kind == ObjType::Text {
            room.splice_text(&object, 0, 0, "not a map entry").unwrap();
        } else {
            room.put(
                &object,
                "invalid",
                automerge::ScalarValue::Bytes(b"not json".to_vec()),
            )
            .unwrap();
            room.put(
                &object,
                owner.as_str(),
                automerge::ScalarValue::Bytes(serde_json::to_vec(&Role::Viewer).unwrap()),
            )
            .unwrap();
        }
        room.commit();
        compare_native(&peer, source, now);
    }
    let (mut peer, source, now) = fixture();
    peer.core
        .room_doc
        .as_mut()
        .unwrap()
        .put(ROOT, "members", false)
        .unwrap();
    compare_native(&peer, source, now);
    for map_wins in [false, true] {
        let (mut peer, source, now) = fixture();
        let doc = peer.admitted_room().unwrap();
        let mut valid = doc
            .clone()
            .with_actor(automerge::ActorId::from(vec![if map_wins { 3 } else { 1 }]));
        let mut invalid = doc.clone().with_actor(automerge::ActorId::from(vec![2]));
        let members = valid.put_object(ROOT, "members", ObjType::Map).unwrap();
        valid
            .put(
                &members,
                hex::encode(peer.authority().user_public_key),
                automerge::ScalarValue::Bytes(serde_json::to_vec(&Role::Commenter).unwrap()),
            )
            .unwrap();
        valid.commit();
        invalid.put_object(ROOT, "members", ObjType::List).unwrap();
        invalid.commit();
        valid.merge_incoming(&mut invalid).unwrap();
        assert_eq!(valid.get_all(ROOT, "members").unwrap().len(), 2);
        peer.core.room_doc = Some(valid.into());
        compare_native(&peer, source, now);
    }
}
#[test]
fn bounded_current_member_copies_all_grant_fields() {
    let (mut peer, source, now) = fixture();
    let room = peer.room().to_owned();
    let long = "owned grant string ".repeat(32);
    let subjects = [
        Subject::Role(Role::Viewer),
        Subject::Person(long.clone()),
        Subject::App(long.clone()),
        Subject::Agent(long.clone()),
        Subject::Task(long.clone()),
        Subject::Shortcut(long.clone()),
        Subject::ForeignDevice,
    ];
    let nodes = [
        TreePath::room(&room),
        TreePath::room(&room).app(long.clone()),
        TreePath::room(&room)
            .app(long.clone())
            .kind(long.clone())
            .resource(long.clone())
            .action(long.clone()),
        TreePath::room(&room).rest().field(&long),
        TreePath::room(&room).any(tilo_action_policy::Level::App),
        roles::room_resource(&room),
    ];
    let lifetimes = [
        Lifetime::UntilRevoked,
        Lifetime::Until(u64::MAX),
        Lifetime::Task(long.clone()),
    ];
    for (i, node) in nodes.into_iter().enumerate() {
        let grant = Grant {
            id: GrantId::new(format!("extra{i:02}{long}")),
            subject: subjects[i % subjects.len()].clone(),
            node,
            kinds: KindSet::READ_UNDOABLE,
            answer: Answer::Ask,
            lifetime: lifetimes[i % lifetimes.len()].clone(),
            granted_by: Subject::Person(long.clone()),
        };
        put_grant(&mut peer, &grant);
    }
    compare_native(&peer, source, now);
}
fn row(peer: &StatePeer, source: &[u8; 32]) -> GovernedIdentity {
    let room = peer.admitted_room().unwrap();
    let identities = room.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let raw = room
        .get(&identities, hex::encode(source))
        .unwrap()
        .unwrap()
        .0;
    serde_json::from_slice(raw.as_bytes().unwrap()).unwrap()
}
fn replace_row(peer: &mut StatePeer, source: &[u8; 32], bytes: Vec<u8>) {
    let room = peer.core.room_doc.as_mut().unwrap();
    let identities = room.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    room.put(
        &identities,
        hex::encode(source),
        automerge::ScalarValue::Bytes(bytes),
    )
    .unwrap();
    room.commit();
}
#[test]
#[expect(
    clippy::similar_names,
    reason = "`root` and `room` are the domain's own names"
)]
fn shared_current_member_retains_original_certificate_json_and_revocation_error_order() {
    let (base, source, now) = fixture();
    let mut cases = Vec::new();
    let mut malformed = base.clone();
    replace_row(&mut malformed, &source, b"{bad json".to_vec());
    cases.push(malformed);
    let mut absent = base.clone();
    let room = absent.core.room_doc.as_mut().unwrap();
    let ids = room.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    room.delete(&ids, hex::encode(source)).unwrap();
    cases.push(absent);
    let mut mismatch = base.clone();
    let mut identity = row(&mismatch, &source);
    identity.device_cert.device_label.push_str("changed");
    replace_row(
        &mut mismatch,
        &source,
        serde_json::to_vec(&identity).unwrap(),
    );
    cases.push(mismatch);
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: base.authority().user_public_key,
        revoked_device_public_key: source,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &root).unwrap();
    for mode in 0..4 {
        let mut candidate = base.clone();
        let mut identity = row(&candidate, &source);
        identity.revocations = vec![revoked.clone()];
        if mode == 1 {
            identity.revocations[0].user_public_key = [88; 32];
        }
        if mode == 2 {
            identity.revocations[0].signature[0] ^= 1;
        }
        if mode == 3 {
            let mut bad = revoked.clone();
            bad.signature[0] ^= 1;
            identity.revocations.push(bad);
        }
        replace_row(
            &mut candidate,
            &source,
            serde_json::to_vec(&identity).unwrap(),
        );
        cases.push(candidate);
    }
    let mut missing = base;
    missing.core.room_doc = None;
    cases.push(missing);
    for peer in cases {
        assert_eq!(peer.role(&source, now), original_role(&peer, &source, now));
        assert_eq!(
            peer.role(&[66; 32], now),
            original_role(&peer, &[66; 32], now)
        );
        assert_eq!(
            peer.role(&source, SystemTime::UNIX_EPOCH),
            original_role(&peer, &source, SystemTime::UNIX_EPOCH)
        );
    }
}

#[test]
fn bounded_current_member_preserves_restriction_role_semantics_and_current_certificate_expiry() {
    let (mut restricted, source, now) = fixture();
    let person = hex::encode(row(&restricted, &source).device_cert.user_public_key);
    // A deny on one app narrows what the member may do there; it is no block.
    let deny = Grant::new(
        "denial",
        Subject::person(person.as_str()),
        TreePath::room(restricted.room()).app("so.tilo.notes"),
        KindSet::ALL,
        Answer::Deny,
        Subject::person("a different person"),
    );
    put_grant(&mut restricted, &deny);
    assert_eq!(restricted.role(&source, now).unwrap(), Role::Editor);
    compare_native(&restricted, source, now);
    let (mut expiring, source, now) = fixture();
    let mut identity = row(&expiring, &source);
    identity.device_cert.expires_at_ns = Some(now_ns(now) + 1);
    tilo_user_identity::sign_device_cert(
        &mut identity.device_cert,
        &SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    expiring.core.certificates.insert(
        source,
        VerifiedCertificate::historical_typed(identity.device_cert.clone()).unwrap(),
    );
    replace_row(
        &mut expiring,
        &source,
        serde_json::to_vec(&identity).unwrap(),
    );
    compare_native(&expiring, source, now);
    compare_native(&expiring, source, now + Duration::from_nanos(1));
}
#[test]
fn bounded_current_member_refuses_a_small_governance_limit_without_changing_peer() {
    let (peer, source, now) = fixture();
    let bytes = peer.durable().unwrap();
    let limits = PeerLimits {
        governance_collection_bytes: 1,
        ..PeerLimits::default()
    };
    let restored = restore_bounded(&peer, now, &limits);
    assert!(matches!(
        restored.read().member(source, now),
        Err(BoundedPeerError::Capacity)
    ));
    let restored = restore_bounded(&peer, now, &PeerLimits::default());
    assert_eq!(
        restored.read().member(source, now).unwrap().role(),
        Role::Editor
    );
    assert_eq!(peer.durable().unwrap(), bytes);
}
fn make_foreign(peer: &mut StatePeer, source: &[u8; 32]) {
    let mut identity = row(peer, source);
    identity.device_cert.capabilities.foreign = true;
    tilo_user_identity::sign_device_cert(
        &mut identity.device_cert,
        &SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    peer.core.certificates.insert(
        *source,
        VerifiedCertificate::historical_typed(identity.device_cert.clone()).unwrap(),
    );
    replace_row(peer, source, serde_json::to_vec(&identity).unwrap());
}
fn assert_foreign(peer: &StatePeer, source: [u8; 32], now: SystemTime, role: Role) {
    let legacy = peer.permissions(source, now).unwrap();
    assert_eq!(legacy.role, role);
    assert!(legacy.foreign);
    let invite = roles::governance_action(peer.room(), roles::governance::INVITE);
    assert!(!legacy.allows(&invite, ActionKind::Undoable, now));
    let restored = restore_bounded(peer, now, &PeerLimits::default());
    let read = restored.read();
    let member = read.member(source, now).unwrap();
    assert_eq!(member.role(), role);
    assert!(member.foreign());
}
#[test]
fn a_foreign_device_acts_with_at_most_the_workspace_bound() {
    let (mut peer, source, now) = fixture();
    let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
    let room = peer.core.room_doc.as_mut().unwrap();
    room.put(ROOT, "owner", person.as_str()).unwrap();
    room.commit();
    assert_eq!(peer.permissions(source, now).unwrap().role, Role::Owner);

    make_foreign(&mut peer, &source);
    assert_foreign(&peer, source, now, Role::Owner);
    let content = TreePath::room(peer.room()).app("app").resource("record");
    assert!(
        peer.permissions(source, now)
            .unwrap()
            .allows(&content, ActionKind::Undoable, now)
    );

    // The room narrows what a foreign device may do to reading.
    let room_id = peer.room().to_owned();
    put_grant(
        &mut peer,
        &Grant::new(
            "foreign-device-read",
            Subject::ForeignDevice,
            TreePath::room(&room_id),
            KindSet::UNDOABLE.union(KindSet::OUTSIDE),
            Answer::Deny,
            Subject::person(person.as_str()),
        ),
    );
    assert_foreign(&peer, source, now, Role::Owner);
    assert!(
        !peer
            .permissions(source, now)
            .unwrap()
            .allows(&content, ActionKind::Undoable, now)
    );
    assert!(
        peer.check_document_read(source, "app", "record", now)
            .is_ok()
    );
}
#[test]
fn a_foreign_device_is_not_bounded_in_the_room_it_governs() {
    let (mut peer, source, now) = fixture();
    let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
    let room = peer.core.room_doc.as_mut().unwrap();
    room.put(ROOT, "owner", person.as_str()).unwrap();
    room.commit();
    make_foreign(&mut peer, &source);
    assert!(peer.permissions(source, now).unwrap().foreign);

    peer.core.authority = row(&peer, &source).device_cert;
    let member = peer.permissions(source, now).unwrap();
    assert_eq!(member.role, Role::Owner);
    assert!(!member.foreign);
    let restored = restore_bounded(&peer, now, &PeerLimits::default());
    assert_eq!(
        restored.read().member(source, now).unwrap().role(),
        Role::Owner
    );
}
