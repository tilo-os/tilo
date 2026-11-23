//! What one more admission costs a workspace's governor as the workspace grows.

use crate::StatePeer;
use crate::join::WorkspaceId;
use crate::join::{
    InviteGrant, JoinChallenge, Role, VerifiedRedemption, issue_invitation, sign_join_proof,
    verify_join_proof,
};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

fn key(n: u32, salt: u8) -> SigningKey {
    let mut seed = [salt; 32];
    seed[..4].copy_from_slice(&n.to_le_bytes());
    SigningKey::from_bytes(&seed)
}

/// Person `n`'s device, certified by their own root.
pub(crate) fn person(n: u32) -> (SigningKey, DeviceCert) {
    let root = key(n, 201);
    let device = key(n, 202);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: format!("person-{n}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (device, cert)
}

/// An invitation of the workspace's owner, redeemed by person `n`.
pub(crate) fn redemption(
    peer: &StatePeer,
    authority: &SigningKey,
    n: u32,
    now_ns: u64,
) -> VerifiedRedemption {
    let (device, cert) = person(n);
    let mut nonce = [7; 16];
    nonce[..4].copy_from_slice(&n.to_le_bytes());
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
            issuer: peer.owner_certificates()[0].clone(),
            role: Role::Editor,
            nonce,
            recipient_user_public_key: Some(cert.user_public_key),
            invitee_name: None,
            expires_at_ns: now_ns.checked_add(10_000_000_000).unwrap(),
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
    let mut request = [9; 32];
    request[..4].copy_from_slice(&n.to_le_bytes());
    let challenge = JoinChallenge::new(
        &join,
        &cert,
        request,
        [3; 32],
        now_ns.checked_add(1_000_000_000).unwrap(),
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &device).unwrap();
    verify_join_proof(&proof, &challenge, &cert, &RevocationList::new(), now_ns).unwrap()
}

pub(crate) fn now_ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

/// Admit person `n`, named as they chose, as a workspace host admits them.
pub(crate) fn admit(peer: &mut StatePeer, authority: &SigningKey, n: u32, now: SystemTime) {
    let redemption = redemption(peer, authority, n, now_ns(now));
    let mut next = peer.clone();
    assert!(
        next.governance_mut()
            .admit_invited_member(&redemption, authority, now)
            .unwrap()
    );
    next.governance_mut()
        .name_person(
            redemption.certificate().user_public_key,
            &format!("Person {n}"),
            authority,
            now,
        )
        .unwrap();
    *peer = next;
}

/// A workspace host's copy of a workspace with `members` people admitted.
pub(crate) fn crowd(members: u32) -> (StatePeer, SigningKey, SystemTime) {
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    peer.write_as_workspace_host();
    for n in 0..members {
        admit(&mut peer, &authority, n, now);
    }
    (peer, authority, now)
}

tilo_test_env::scenario! {
    fn a_crowded_workspaces_governance_is_read_and_stored_by_what_changed;

    phase every_change_reads_the_same_narrowed_as_whole;
    // Every kind of change a governor makes to a crowded workspace reads the
    // same through the narrowed paths (the parts a change wrote, the rows a
    // change left as they were) as through a whole reading: each governance
    // write a test makes compares the two ([`crate::workspace::changed_rows_by`]),
    // and the member list, access and accesses after each step are the ones a
    // copy that never read an earlier state derives.
    {
        type Step<'a> = (&'a str, Box<dyn Fn(&mut StatePeer) + 'a>);
        let (mut peer, authority, now) = crowd(4);
        let person = |n: u32| person(n).1.user_public_key;
        let steps: Vec<Step<'_>> = vec![
            ("admit", Box::new(|peer| admit(peer, &authority, 10, now))),
            (
                "rename",
                Box::new(|peer| {
                    peer.governance_mut()
                        .name_person(person(1), "Renamed", &authority, now)
                        .unwrap();
                }),
            ),
            (
                "role",
                Box::new(|peer| {
                    peer.governance_mut()
                        .set_member_role(person(2), Role::Viewer, &authority, now)
                        .unwrap();
                }),
            ),
            (
                "administrator",
                Box::new(|peer| {
                    peer.governance_mut()
                        .set_member_administrator(person(3), None, &authority, now)
                        .unwrap();
                }),
            ),
            (
                "remove",
                Box::new(|peer| {
                    peer.governance_mut()
                        .remove_member(person(0), &authority, now)
                        .unwrap();
                }),
            ),
            ("readmit", Box::new(|peer| admit(peer, &authority, 11, now))),
        ];
        for (step, change) in steps {
            // Read the state before, so its views carry over into the next.
            let _ = peer.governance().governed_members(now).unwrap();
            change(&mut peer);
            let fresh = StatePeer::restore(
                &peer.durable().unwrap(),
                peer.workspace_id(),
                peer.owner_certificates()[0].clone(),
                vec![],
                RevocationList::new(),
                now,
            )
            .unwrap();
            assert_eq!(
                peer.governance().governed_members(now).unwrap(),
                fresh.governance().governed_members(now).unwrap(),
                "{step}: the people"
            );
            assert_eq!(
                peer.governance().governed_access(now).unwrap(),
                fresh.governance().governed_access(now).unwrap(),
                "{step}: who may read"
            );
            assert_eq!(
                peer.governance().governed_device_access(now).unwrap(),
                fresh.governance().governed_device_access(now).unwrap(),
                "{step}: what each device may do"
            );
        }
    }

    phase the_governance_copy_is_stored_as_a_base_and_the_changes_since;
    // The governance copy is stored as a saved base and the changes since it:
    // an admission keeps the base its state was stored on, the same allocation,
    // and stores only its own change beside it; once the changes since the base
    // weigh as much as half of it, a base is saved anew. Whatever was stored
    // restores the workspace as it stands.
    {
        let (mut peer, authority, now) = crowd(3);
        let workspace = peer.workspace_id().to_owned();
        let owner = peer.owner_certificates()[0].clone();
        let restored = |parts: &[std::sync::Arc<[u8]>]| {
            StatePeer::restore(
                &parts.concat(),
                &workspace,
                owner.clone(),
                vec![],
                RevocationList::new(),
                now,
            )
            .unwrap()
        };
        let first = peer.durable_parts().unwrap();
        let mut base = std::sync::Arc::clone(&first[1]);
        let mut rebased = 0;
        for n in 3..40 {
            admit(&mut peer, &authority, n, now);
            let parts = peer.durable_parts().unwrap();
            assert_eq!(parts.concat(), peer.durable_whole().unwrap());
            if std::sync::Arc::ptr_eq(&parts[1], &base) {
                assert!(
                    parts[2].len().saturating_mul(2) <= base.len().saturating_add(64),
                    "the changes since the base stay within half of it"
                );
            } else {
                rebased += 1;
                base = std::sync::Arc::clone(&parts[1]);
            }
            assert_eq!(
                restored(&parts).governance().governed_members(now).unwrap(),
                peer.governance().governed_members(now).unwrap(),
                "admission {n} restores as it stands"
            );
        }
        assert!(
            rebased >= 1 && rebased * 2 < 37,
            "a base is saved anew now and then, not with every change: {rebased} of 37"
        );
    }
}

/// What a newcomer's copy works through taking in the governance of a
/// workspace of `members` and reading who is in it, as a fresh page does:
/// nothing it reads was checked in its process before.
fn newcomer_work(members: u32) -> crate::work::Work {
    let (host, authority, now) = crowd(members);
    let message = host.workspace_message();
    let owner = host.owner_certificates()[0].clone();
    tilo_user_identity::cert::forget_verified_signatures();
    crate::members::op::forget_verified_ops();
    let before = crate::work::now();
    let mut guest = StatePeer::joined(
        host.workspace_id(),
        owner,
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    guest
        .receive(authority.verifying_key().to_bytes(), message, now)
        .unwrap();
    let access = guest.governance().governed_access(now).unwrap();
    let members = usize::try_from(members).unwrap();
    assert_eq!(access.member_devices.len(), members.saturating_add(2));
    let people = guest.governance().governed_members(now).unwrap();
    // The workspace's owner is one of its members, beside the `members` it
    // admitted.
    assert_eq!(people.members.len(), members.saturating_add(1));
    guest.governance().governed_device_access(now).unwrap();
    assert!(matches!(guest.security_basis(), Some(Ok(_))));
    crate::work::now().since(before)
}

/// A newcomer taking in a workspace of 300 against one of 30. The
/// governance names every member with their certificate and the proof they
/// were let in, and the newcomer checks each of those once: one row decoded,
/// three signatures (the certificate, the membership operation and the
/// receipt of the invitation they redeemed) and one receipt read per
/// member, a bound the protocol makes inherent. Every reading over all the
/// rows is made a fixed number of times, however many members there are.
///
/// The process remembers, on purpose, what it has already checked, admitted
/// and replayed ([`newcomer_work`]), so these counts are a newcomer's only
/// where no other test reads the same rows: a process of its own.
#[test]
fn a_newcomer_works_through_each_member_once() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_newcomer_works_through_each_member_once",
        newcomer_of_30_against_300,
    );
}

fn newcomer_of_30_against_300() {
    let small = newcomer_work(30);
    let large = newcomer_work(300);
    let more = |large: u64, small: u64| large.saturating_sub(small);
    assert_eq!(
        (large.admission_reads, large.replays, large.security_bases),
        (small.admission_reads, small.replays, small.security_bases),
        "readings over every row, at 300 members against 30: {large:?} against {small:?}"
    );
    assert!(
        more(large.identities_decoded, small.identities_decoded) <= 270,
        "one row decoded per member: {large:?} against {small:?}"
    );
    assert!(
        more(large.signatures_verified, small.signatures_verified) <= 3 * 270,
        "three signatures per member: {large:?} against {small:?}"
    );
    assert!(
        more(large.receipts_read, small.receipts_read) <= 270,
        "one receipt per member: {large:?} against {small:?}"
    );
}

/// What a member's copy of a workspace of `members` works through when someone
/// enters it, as each device holding the workspace does: it reads who is in it
/// now and whether each of the `records` it holds needs keys of its own, which
/// is how it seals its next write there.
fn entered_work(members: u32, records: usize) -> crate::work::Work {
    let (mut peer, authority, now) = crowd(members);
    admit(&mut peer, &authority, members, now);
    let before = crate::work::now();
    let people = peer.governance().governed_members(now).unwrap();
    // The crowd, its owner and the person who just entered.
    assert_eq!(
        people.members.len(),
        usize::try_from(members.checked_add(2).unwrap()).unwrap()
    );
    let basis = peer.document_keys_basis(now);
    for record in 0..records {
        assert!(
            !basis
                .requires("so.tilo.notes", &format!("note-{record}"))
                .unwrap(),
            "every member reads the workspace's notes"
        );
    }
    crate::work::now().since(before)
}

/// A workspace of 300 against one of 30, someone entering each: the member's
/// copy walks the workspace's grants once for all its people, not once a
/// person, and reads who holds the workspace's keys once for all the records it
/// holds, not once a record. Each person and each record costs a lookup, a
/// bound the work does not grow past however many members there are.
#[test]
fn an_entry_reads_the_grants_and_the_keys_once() {
    let small = entered_work(30, 40);
    let large = entered_work(300, 40);
    assert_eq!(
        (large.grant_walks, large.key_access_reads),
        (small.grant_walks, small.key_access_reads),
        "grant walks and key-holder readings, at 300 members against 30: {large:?} against \
         {small:?}"
    );
    assert_eq!(
        large.key_access_reads, 1,
        "40 records' keys read off one reading of the key holders: {large:?}"
    );
}
