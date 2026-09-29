//! Comment records in a room: a Commenter writes records they own and adds
//! them to the room's index, and nothing else; nobody else writes into a
//! comment record.

use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::SystemTime;

const APP: &str = "so.tilo.chat";

/// A person from another root, admitted to `peer`'s room at `role` through
/// an invitation the authority issued.
fn invited(
    peer: &mut StatePeer,
    authority: &SigningKey,
    role: crate::join::MemberRole,
    seed: u8,
    now: SystemTime,
) -> (SigningKey, DeviceCert) {
    use crate::join::{InviteGrant, JoinChallenge, issue_invitation, sign_join_proof};
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap();
    let root = SigningKey::from_bytes(&[seed; 32]);
    let key = SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: 7,
            room: peer.room().into(),
            authority: peer.authority().clone(),
            issuer: peer.authority().clone(),
            delegation: None,
            role,
            nonce: [seed; 16],
            recipient_user_public_key: Some(cert.user_public_key),
            invitee_name: Some("guest".into()),
            expires_at_ns: now_ns.saturating_add(10_000_000_000),
            transport: vec![],
        },
        authority,
    )
    .unwrap();
    let join = crate::join::decode_and_verify_invitation(
        &signed,
        Some(cert.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &cert,
        [seed; 32],
        [seed.wrapping_add(2); 32],
        now_ns.saturating_add(1_000_000_000),
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &key).unwrap();
    let redemption =
        crate::join::verify_join_proof(&proof, &challenge, &cert, &RevocationList::new(), now_ns)
            .unwrap();
    assert!(
        peer.admit_invited_member(&redemption, authority, now)
            .unwrap()
    );
    (key, cert)
}

fn delta(doc: &mut AutoCommit, write: impl FnOnce(&mut AutoCommit)) -> Vec<u8> {
    let before = doc.get_heads();
    write(doc);
    doc.commit();
    doc.save_after(&before)
}

/// The room route's document with its shared index, and the change that
/// adds `record` to it.
fn index_entry(record: &str) -> Vec<u8> {
    let mut index = AutoCommit::new();
    tilo_automerge::ApplyIncoming::apply_incoming(&mut index, tilo_room_index::index_genesis())
        .unwrap();
    let entries = tilo_room_index::index_ro(&index).unwrap();
    let mut all = index.save();
    all.extend(delta(&mut index, |doc| {
        let entry = doc
            .put_object(&entries, record, automerge::ObjType::Map)
            .unwrap();
        doc.put(&entry, "kind", "chat.message").unwrap();
    }));
    all
}

#[test]
fn a_commenter_writes_comment_records_they_own_and_references_them() {
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    let (guest, cert) = invited(
        &mut peer,
        &authority,
        crate::join::MemberRole::Commenter,
        61,
        now,
    );
    let author = collab_types::Ed25519PublicKey::from_bytes(cert.device_public_key);
    let record = collab_types::comment_record_id(&author, "m1", "chat.channel:c_feedback").unwrap();

    let mut comment = AutoCommit::new();
    let body = delta(&mut comment, |doc| {
        doc.put(ROOT, "text", "Send fails").unwrap();
    });
    let message = peer
        .publish_incremental(APP, &record, body, &guest, now)
        .expect("a Commenter writes their own comment record");
    let NeighbourMsg::AppDocument { envelope, .. } = &message else {
        panic!("a document message");
    };
    assert_eq!(envelope.op_class, collab_types::OpClass::Comment);

    let reference = peer
        .publish_incremental(APP, "", index_entry(&record), &guest, now)
        .expect("a Commenter adds their own comment record to the room index");
    let NeighbourMsg::AppDocument { envelope, .. } = &reference else {
        panic!("a document message");
    };
    assert_eq!(envelope.op_class, collab_types::OpClass::Comment);
}

#[test]
fn a_commenter_changes_nothing_else() {
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    let (guest, _) = invited(
        &mut peer,
        &authority,
        crate::join::MemberRole::Commenter,
        71,
        now,
    );
    let mut document = AutoCommit::new();
    let edit = delta(&mut document, |doc| {
        doc.put(ROOT, "value", "changed").unwrap();
    });
    assert!(
        peer.publish_incremental("test.app", "tab:1", edit, &guest, now)
            .is_err(),
        "a Commenter does not write an ordinary record"
    );
    let other = hex::encode([9_u8; 32]);
    assert!(
        peer.publish_incremental(
            APP,
            "",
            index_entry(&format!("comment:{other}:m1@chat.channel:c_feedback")),
            &guest,
            now
        )
        .is_err(),
        "a Commenter does not reference someone else's comment record"
    );
}

#[test]
fn nobody_writes_into_someone_elses_comment_record() {
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    let (_, cert) = invited(
        &mut peer,
        &authority,
        crate::join::MemberRole::Commenter,
        81,
        now,
    );
    let author = collab_types::Ed25519PublicKey::from_bytes(cert.device_public_key);
    let record = collab_types::comment_record_id(&author, "m1", "chat.channel:c_feedback").unwrap();
    let mut comment = AutoCommit::new();
    let body = delta(&mut comment, |doc| {
        doc.put(ROOT, "text", "rewritten").unwrap();
    });
    assert!(
        peer.publish_incremental(APP, &record, body, &authority, now)
            .is_err(),
        "even the owner of the room does not write into a member's comment"
    );
}
