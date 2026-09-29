//! Delegated host admission into a public room: the owner's invite-token
//! delegation lets a host admit Commenters and nothing more. Every case runs
//! the real `mint_root` / `attenuate` / `HostAdmissionV1::{sign, verify}`
//! path.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use collab_types::caps::Verb;
use collab_types::{
    Ed25519PublicKey, HostAdmissionError, HostAdmissionSpec, HostAdmissionV1, InviteError,
    InviteSpec, InviteToken, Role, attenuate, generate_keypair, mint_root,
};

const ROOM: &str = "6f4d8e8a-5b0c-4f55-9d57-1b5fdb2c0a11";
const WORKSPACE: u64 = 0x7110;

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

fn delegation_spec(host: Ed25519PublicKey, role: Role, caps: &[Verb]) -> InviteSpec {
    InviteSpec {
        subject: host,
        workspace: WORKSPACE,
        role,
        caps: caps.iter().copied().collect::<BTreeSet<_>>(),
        sub_resource: None,
        not_after: None,
    }
}

fn admission(role: Role, delegation: Vec<InviteToken>) -> HostAdmissionSpec {
    let (_, member_user) = generate_keypair();
    HostAdmissionSpec {
        room: ROOM.into(),
        workspace: WORKSPACE,
        member_user,
        role,
        invite: [9; 32],
        admitted_at_unix_seconds: 5,
        delegation,
    }
}

struct Fixture {
    owner: Ed25519PublicKey,
    owner_sk: ed25519_dalek::SigningKey,
    host: ed25519_dalek::SigningKey,
    delegation: Vec<InviteToken>,
}

fn fixture() -> Fixture {
    let (owner_sk, owner) = generate_keypair();
    let (host, host_pk) = generate_keypair();
    let root = mint_root(
        delegation_spec(host_pk, Role::Commenter, &[Verb::Read, Verb::Comment]),
        &owner_sk,
    )
    .unwrap();
    Fixture {
        owner,
        owner_sk,
        host,
        delegation: vec![root],
    }
}

#[test]
fn a_delegated_host_admits_a_commenter() {
    let f = fixture();
    let signed = HostAdmissionV1::sign(admission(Role::Commenter, f.delegation), &f.host).unwrap();
    assert_eq!(signed.verify(f.owner, t(10)), Ok(Role::Commenter));
    let viewer =
        HostAdmissionV1::sign(admission(Role::Viewer, signed.delegation), &f.host).unwrap();
    assert_eq!(viewer.verify(f.owner, t(10)), Ok(Role::Viewer));
}

#[test]
fn a_host_can_never_admit_an_editor_or_an_owner() {
    let f = fixture();
    for role in [Role::Editor, Role::Owner] {
        assert_eq!(
            HostAdmissionV1::sign(admission(role, f.delegation.clone()), &f.host).unwrap_err(),
            HostAdmissionError::RoleTooHigh,
            "refused at the source"
        );
    }
    // And a record that bypassed `sign` — re-labelled after signing — fails
    // verification: the role is covered by the signature and the ceiling is
    // checked again by every member.
    let mut escalated =
        HostAdmissionV1::sign(admission(Role::Commenter, f.delegation), &f.host).unwrap();
    escalated.role = Role::Editor;
    assert_eq!(
        escalated.verify(f.owner, t(10)),
        Err(HostAdmissionError::RoleTooHigh)
    );
}

#[test]
fn an_owner_cannot_delegate_more_than_commenter_admission() {
    let (owner_sk, owner) = generate_keypair();
    let (host, host_pk) = generate_keypair();
    let editor_delegation = mint_root(
        delegation_spec(
            host_pk,
            Role::Editor,
            &[Verb::Read, Verb::Comment, Verb::Write],
        ),
        &owner_sk,
    )
    .unwrap();
    let signed =
        HostAdmissionV1::sign(admission(Role::Commenter, vec![editor_delegation]), &host).unwrap();
    assert_eq!(
        signed.verify(owner, t(10)),
        Err(HostAdmissionError::RoleTooHigh),
        "an over-broad delegation is refused outright rather than trusted up to the ceiling"
    );
}

#[test]
fn a_viewer_delegation_admits_no_commenters() {
    let (owner_sk, owner) = generate_keypair();
    let (host, host_pk) = generate_keypair();
    let viewer_only = mint_root(
        delegation_spec(host_pk, Role::Viewer, &[Verb::Read]),
        &owner_sk,
    )
    .unwrap();
    let signed =
        HostAdmissionV1::sign(admission(Role::Commenter, vec![viewer_only]), &host).unwrap();
    assert_eq!(
        signed.verify(owner, t(10)),
        Err(HostAdmissionError::RoleTooHigh)
    );
}

#[test]
fn only_the_delegated_host_and_the_rooms_owner_count() {
    let f = fixture();
    let (impostor, _) = generate_keypair();
    let forged =
        HostAdmissionV1::sign(admission(Role::Commenter, f.delegation.clone()), &impostor).unwrap();
    assert_eq!(
        forged.verify(f.owner, t(10)),
        Err(HostAdmissionError::NotDelegate),
        "another host cannot borrow a delegation it was not issued"
    );

    let (_, stranger) = generate_keypair();
    let signed = HostAdmissionV1::sign(admission(Role::Commenter, f.delegation), &f.host).unwrap();
    assert_eq!(
        signed.verify(stranger, t(10)),
        Err(HostAdmissionError::Delegation(InviteError::UntrustedRoot)),
        "a delegation rooted anywhere but the room's owner is worthless"
    );
}

#[test]
fn a_delegation_is_scoped_to_one_whole_room() {
    let f = fixture();
    let mut other_room = admission(Role::Commenter, f.delegation.clone());
    other_room.workspace = WORKSPACE + 1;
    let signed = HostAdmissionV1::sign(other_room, &f.host).unwrap();
    assert_eq!(
        signed.verify(f.owner, t(10)),
        Err(HostAdmissionError::WrongRoom)
    );

    let (pinned_host, pinned_host_pk) = generate_keypair();
    let pinned = mint_root(
        InviteSpec {
            sub_resource: Some(collab_types::ArtifactId::new()),
            ..delegation_spec(
                pinned_host_pk,
                Role::Commenter,
                &[Verb::Read, Verb::Comment],
            )
        },
        &f.owner_sk,
    )
    .unwrap();
    let signed =
        HostAdmissionV1::sign(admission(Role::Commenter, vec![pinned]), &pinned_host).unwrap();
    assert_eq!(
        signed.verify(f.owner, t(10)),
        Err(HostAdmissionError::WrongRoom),
        "a delegation pinned to one resource admits nobody to the room"
    );
}

#[test]
fn the_host_may_attenuate_but_its_delegate_stays_capped() {
    let f = fixture();
    let (sub_host, sub_host_pk) = generate_keypair();
    let child = attenuate(
        &f.delegation[0],
        delegation_spec(sub_host_pk, Role::Viewer, &[Verb::Read]),
        &f.host,
    )
    .unwrap();
    let chain = vec![f.delegation[0].clone(), child];
    let viewer = HostAdmissionV1::sign(admission(Role::Viewer, chain.clone()), &sub_host).unwrap();
    assert_eq!(viewer.verify(f.owner, t(10)), Ok(Role::Viewer));
    let commenter = HostAdmissionV1::sign(admission(Role::Commenter, chain), &sub_host).unwrap();
    assert_eq!(
        commenter.verify(f.owner, t(10)),
        Err(HostAdmissionError::RoleTooHigh)
    );
}

#[test]
fn an_expired_delegation_admits_nobody_and_tampering_is_caught() {
    let (owner_sk, owner) = generate_keypair();
    let (host, host_pk) = generate_keypair();
    let expiring = mint_root(
        InviteSpec {
            not_after: Some(t(100)),
            ..delegation_spec(host_pk, Role::Commenter, &[Verb::Read, Verb::Comment])
        },
        &owner_sk,
    )
    .unwrap();
    let signed = HostAdmissionV1::sign(admission(Role::Commenter, vec![expiring]), &host).unwrap();
    assert_eq!(signed.verify(owner, t(99)), Ok(Role::Commenter));
    assert_eq!(
        signed.verify(owner, t(100)),
        Err(HostAdmissionError::Delegation(InviteError::Expired))
    );

    let mut retargeted = signed;
    let (_, someone_else) = generate_keypair();
    retargeted.member_user = someone_else;
    assert!(matches!(
        retargeted.verify(owner, t(10)),
        Err(HostAdmissionError::BadSignature(_))
    ));
}
