//! What one more admission costs a workspace's governor as the workspace grows.

use crate::StatePeer;
use crate::join::{
    InviteGrant, JoinChallenge, MemberRole, VerifiedRedemption, issue_invitation, sign_join_proof,
    verify_join_proof,
};
use ed25519_dalek::SigningKey;
use std::time::{Instant, SystemTime};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use crate::join::WorkspaceId;

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
            workspace_id: WorkspaceId::from_u128(7),
            issuer: peer.owner_certificates()[0].clone(),
            role: MemberRole::Editor,
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

fn time<T>(label: &str, members: u32, run: impl FnOnce() -> T) -> T {
    let _ = crate::governance_work::take();
    let started = Instant::now();
    let value = run();
    let took = started.elapsed();
    let work = crate::governance_work::take();
    eprintln!("COST members={members} {label} {took:?} {work:?}");
    value
}

/// Every kind of change a governor makes to a crowded workspace reads the
/// same through the narrowed paths (the parts a change wrote, the rows a
/// change left as they were) as through a whole reading: each governance
/// write a test makes compares the two ([`crate::workspace::changed_rows_by`]),
/// and the member list, access and standings after each step are the ones a
/// copy that never read an earlier state derives.
#[test]
fn every_change_reads_the_same_narrowed_as_whole() {
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
                    .set_member_role(person(2), MemberRole::Viewer, &authority, now)
                    .unwrap();
            }),
        ),
        (
            "administrator",
            Box::new(|peer| {
                peer.governance_mut()
                    .set_member_administrator(person(3), &authority, now)
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
            peer.governance().governed_standings(now).unwrap(),
            fresh.governance().governed_standings(now).unwrap(),
            "{step}: what each device may do"
        );
    }
}

/// The governance copy is stored as a saved base and the changes since it:
/// an admission keeps the base its state was stored on, the same allocation,
/// and stores only its own change beside it; once the changes since the base
/// weigh as much as half of it, a base is saved anew. Whatever was stored
/// restores the workspace as it stands.
#[test]
fn the_governance_copy_is_stored_as_a_base_and_the_changes_since() {
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

#[test]
#[ignore = "a measurement: run with --ignored --nocapture"]
fn admission_cost_breakdown() {
    for members in [30, 300] {
        let (peer, authority, now) = crowd(members);
        let redemption = redemption(&peer, &authority, members, now_ns(now));
        let mut next = time("clone", members, || peer.clone());
        time("admit_invited_member", members, || {
            next.governance_mut()
                .admit_invited_member(&redemption, &authority, now)
                .unwrap()
        });
        time("name_person", members, || {
            next.governance_mut()
                .name_person(
                    redemption.certificate().user_public_key,
                    "Newcomer",
                    &authority,
                    now,
                )
                .unwrap()
        });
        time("governed_access", members, || {
            next.governance().governed_access(now).unwrap()
        });
        time("governed_access again", members, || {
            next.governance().governed_access(now).unwrap()
        });
        time("governed_standings", members, || {
            next.governance().governed_standings(now).unwrap()
        });
        time("governed_members", members, || {
            next.governance().governed_members(now).unwrap()
        });
        time("permissions", members, || {
            next.permissions(redemption.certificate().device_public_key, now)
                .unwrap()
        });
        time("durable_parts", members, || next.durable_parts().unwrap());
    }
}

/// What a newcomer's copy works through taking in the governance of a
/// workspace of `members` and reading who is in it, as a fresh page does:
/// nothing it reads was checked in its process before.
fn newcomer_work(members: u32) -> crate::governance_work::GovernanceWork {
    let (host, authority, now) = crowd(members);
    let message = host.workspace_message();
    let owner = host.owner_certificates()[0].clone();
    tilo_user_identity::cert::forget_verified_signatures();
    crate::members::op::forget_verified_ops();
    let _ = crate::governance_work::take();
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
    assert_eq!(people.len(), members);
    guest.governance().governed_standings(now).unwrap();
    assert!(matches!(guest.security_basis(), Some(Ok(_))));
    crate::governance_work::take()
}

/// A newcomer taking in a workspace of 300 against one of 30. The
/// governance names every member with their certificate and the proof they
/// were let in, and the newcomer checks each of those once: one row decoded,
/// three signatures (the certificate, the membership operation and the
/// receipt of the invitation they redeemed) and one receipt read per
/// member, a bound the protocol makes inherent. Every reading over all the
/// rows is made a fixed number of times, however many members there are.
#[test]
fn a_newcomer_works_through_each_member_once() {
    let small = newcomer_work(30);
    let large = newcomer_work(300);
    let more = |large: u64, small: u64| large.saturating_sub(small);
    assert_eq!(
        (large.governance_reads, large.replays, large.security_bases),
        (small.governance_reads, small.replays, small.security_bases),
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
