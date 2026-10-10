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
use collab_types::ArtifactKind;
use collab_types::Ed25519PublicKey;
use collab_types::caps::Verb;
use ed25519_dalek::SigningKey;
use tilo_automerge::ApplyIncoming;
use web_time::Duration;

fn fixture() -> (StatePeer, [u8; 32], SystemTime) {
    let (mut peer, _, _, cert, now) = crate::admission_tests::seeded();
    let room = peer.core.room_doc.as_mut().unwrap();
    room.put(ROOT, "owner", "a different person").unwrap();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    let cap = CapOverride::grant(
        Subject::User(Ed25519PublicKey::from_bytes(cert.user_public_key)),
        Scope::Workspace,
        Verb::Write,
    )
    .with_expiry(now_secs(now).checked_add(1).unwrap());
    room.put(
        &caps,
        "member",
        automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
    )
    .unwrap();
    room.commit();
    (peer, cert.device_public_key, now)
}
#[test]
fn bounded_current_member_matches_actual_legacy_membership_and_rechecks_expiry() {
    let (peer, source, now) = fixture();
    let bytes = peer.durable().unwrap();
    let expected = peer.permissions(source, now).unwrap();
    assert_eq!(expected.0, Role::Editor);
    let restored = restore_bounded(&peer, now, &PeerLimits::default());
    let read = restored.read();
    let member = read.member(source, now).unwrap();
    assert_eq!(member.role(), expected.0);
    assert_eq!(member.capabilities(), expected.1);
    assert_eq!(member.person(), expected.2);
    let later = now + Duration::from_secs(2);
    assert!(peer.permissions(source, later).is_err());
    assert!(read.member(source, later).is_err());
    assert_eq!(peer.durable().unwrap(), bytes);
}
// Literal pre-extraction role body, with the historical projection oracle.
fn original_role(peer: &StatePeer, source: &[u8; 32], now: SystemTime) -> Result<Role, String> {
    let cert = peer.certificate(source, now)?;
    let room = peer
        .core
        .room_doc
        .as_ref()
        .ok_or("room governance missing")?;
    let (_, owner, caps) = room::extract_governed_fields(room);
    let identities = room::governance_of(Some(room)).identities;
    let policy = crate::cached_document::PolicyData {
        owner,
        caps,
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
    room::role_of(
        policy.owner.as_deref(),
        &policy.caps,
        person.id(),
        now_secs(now),
    )
    .ok_or_else(|| "person has no room membership".into())
}

fn compare_native(peer: &StatePeer, source: [u8; 32], now: SystemTime) {
    assert_eq!(peer.role(&source, now), original_role(peer, &source, now));
    let expected = peer.permissions(source, now);
    let restored = restore_bounded(peer, now, &PeerLimits::default());
    let view = restored.read();
    let actual = view.member(source, now);
    match (expected, actual) {
        (Ok((role, caps, person)), Ok(member)) => {
            assert_eq!(member.role(), role);
            assert_eq!(member.capabilities(), caps);
            assert_eq!(member.person(), person);
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
        let room = peer.core.room_doc.as_mut().unwrap();
        let object = room.put_object(ROOT, "capabilities", kind).unwrap();
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
            let cap = CapOverride::grant(
                Subject::User(Ed25519PublicKey::from_bytes(
                    peer.core.authority.user_public_key,
                )),
                Scope::Workspace,
                Verb::Read,
            );
            room.put(
                &object,
                "valid",
                automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
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
        .put(ROOT, "capabilities", false)
        .unwrap();
    compare_native(&peer, source, now);
    for map_wins in [false, true] {
        let (mut peer, source, now) = fixture();
        let doc = peer.admitted_room().unwrap();
        let mut valid = doc
            .clone()
            .with_actor(automerge::ActorId::from(vec![if map_wins { 3 } else { 1 }]));
        let mut invalid = doc.clone().with_actor(automerge::ActorId::from(vec![2]));
        let caps = valid
            .put_object(ROOT, "capabilities", ObjType::Map)
            .unwrap();
        let cap = CapOverride::grant(
            Subject::User(Ed25519PublicKey::from_bytes(
                peer.authority().user_public_key,
            )),
            Scope::Workspace,
            Verb::Comment,
        );
        valid
            .put(
                &caps,
                "valid",
                automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
            )
            .unwrap();
        valid.commit();
        invalid
            .put_object(ROOT, "capabilities", ObjType::List)
            .unwrap();
        invalid.commit();
        valid.merge_incoming(&mut invalid).unwrap();
        assert_eq!(valid.get_all(ROOT, "capabilities").unwrap().len(), 2);
        peer.core.room_doc = Some(valid.into());
        compare_native(&peer, source, now);
    }
}
#[test]
fn bounded_current_member_copies_all_capability_fields() {
    let (mut peer, source, now) = fixture();
    let room = peer.core.room_doc.as_mut().unwrap();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    let principal = Principal::person_from_key(Ed25519PublicKey::from_bytes(
        peer.core.authority.user_public_key,
    ));
    let long = "owned capability string ".repeat(32);
    let subjects = [
        Subject::Role(Role::Viewer),
        Subject::User(Ed25519PublicKey::from_bytes([47; 32])),
        Subject::Principal(Principal::Person(long.clone())),
        Subject::Principal(Principal::App(long.clone())),
        Subject::Principal(Principal::Agent(long.clone())),
        Subject::Principal(Principal::Token(long.clone())),
        Subject::Principal(Principal::Room(long.clone())),
    ];
    let scopes = [
        Scope::Workspace,
        Scope::App(long.clone()),
        Scope::FsPath(long.clone()),
        Scope::Device(long.clone()),
        Scope::NetHost(long.clone()),
        Scope::Action {
            app: long.clone(),
            resource: long.clone(),
            action: long.clone(),
        },
        Scope::Room(long.clone()),
        Scope::FsRecord(long.clone()),
        Scope::ResourceType(ArtifactKind::Note),
        Scope::ResourceInstance(collab_types::document_artifact_id("app", "room", "record")),
    ];
    for (i, scope) in scopes.into_iter().enumerate() {
        let mut cap = CapOverride::grant(subjects[i % subjects.len()].clone(), scope, Verb::Read);
        cap.id = long.clone();
        cap.granted_by = Some(principal.clone());
        room.put(
            &caps,
            format!("extra{i:02}"),
            automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
        )
        .unwrap();
    }
    room.commit();
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
    let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(
        restricted.authority().user_public_key,
    ));
    let room = restricted.core.room_doc.as_mut().unwrap();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    let cap = CapOverride::new(
        Subject::Principal(person),
        Scope::Workspace,
        collab_types::caps::Effect::Restrict,
        Verb::Invite,
    );
    room.put(
        &caps,
        "denial",
        automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
    )
    .unwrap();
    room.commit();
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
fn put_capability(peer: &mut StatePeer, key: &str, cap: &CapOverride) {
    let room = peer.core.room_doc.as_mut().unwrap();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    room.put(
        &caps,
        key,
        automerge::ScalarValue::Bytes(serde_json::to_vec(cap).unwrap()),
    )
    .unwrap();
    room.commit();
}
fn assert_member(peer: &StatePeer, source: [u8; 32], now: SystemTime, role: Role, cap: Verb) {
    let person = hex::encode(row(peer, &source).device_cert.user_public_key);
    let restriction = CapOverride::new(
        Subject::person(person.as_str()),
        Scope::Workspace,
        collab_types::caps::Effect::Restrict,
        cap,
    );
    let (legacy_role, legacy_caps, _) = peer.permissions(source, now).unwrap();
    assert_eq!(legacy_role, role);
    assert_eq!(legacy_caps.last(), Some(&restriction));
    let restored = restore_bounded(peer, now, &PeerLimits::default());
    let read = restored.read();
    let member = read.member(source, now).unwrap();
    assert_eq!(member.role(), role);
    assert_eq!(member.capabilities().last(), Some(&restriction));
}
#[test]
fn a_foreign_device_acts_with_at_most_the_workspace_cap() {
    let (mut peer, source, now) = fixture();
    let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
    let room = peer.core.room_doc.as_mut().unwrap();
    room.put(ROOT, "owner", person.as_str()).unwrap();
    room.commit();
    assert_eq!(peer.permissions(source, now).unwrap().0, Role::Owner);

    make_foreign(&mut peer, &source);
    assert_member(&peer, source, now, Role::Editor, Verb::Write);

    put_capability(
        &mut peer,
        collab_types::foreign_device::FOREIGN_DEVICE_CAP_ID,
        &collab_types::foreign_device::foreign_device_cap(Verb::Read),
    );
    assert_member(&peer, source, now, Role::Viewer, Verb::Read);
    assert!(
        peer.check_document_read(source, "app", "record", now)
            .is_ok()
    );
}
#[test]
fn a_foreign_device_is_not_capped_in_the_room_it_governs() {
    let (mut peer, source, now) = fixture();
    let person = hex::encode(row(&peer, &source).device_cert.user_public_key);
    let room = peer.core.room_doc.as_mut().unwrap();
    room.put(ROOT, "owner", person.as_str()).unwrap();
    room.commit();
    make_foreign(&mut peer, &source);
    assert_eq!(peer.permissions(source, now).unwrap().0, Role::Editor);

    peer.core.authority = row(&peer, &source).device_cert;
    let (role, caps, _) = peer.permissions(source, now).unwrap();
    assert_eq!(role, Role::Owner);
    assert!(
        caps.iter()
            .all(|cap| cap.effect != collab_types::caps::Effect::Restrict)
    );
    let restored = restore_bounded(&peer, now, &PeerLimits::default());
    assert_eq!(
        restored.read().member(source, now).unwrap().role(),
        Role::Owner
    );
}
