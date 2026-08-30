//! Attenuable invite tokens: a derived token can drop caps but adding a
//! cap the parent lacks is rejected; an expired token is rejected. All
//! proven on the real `mint_root` / `attenuate` / `verify_chain` path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use collab_types::caps::Verb;
use collab_types::{
    ArtifactId, InviteError, InviteSpec, Role, attenuate, generate_keypair, mint_root, verify_chain,
};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

fn verbs(vs: &[Verb]) -> BTreeSet<Verb> {
    vs.iter().copied().collect()
}

const WORKSPACE: u64 = 0xA11CE;

/// Root grant: `role` + `caps` for `subject`, whole-workspace, no
/// expiry unless overridden.
fn spec(subject: collab_types::Ed25519PublicKey, role: Role, caps: &[Verb]) -> InviteSpec {
    InviteSpec {
        subject,
        workspace: WORKSPACE,
        role,
        caps: verbs(caps),
        sub_resource: None,
        not_after: None,
    }
}

// Attenuation: a child that DROPS a cap is valid + verifies.

#[test]
fn derived_token_may_drop_caps() {
    let (root_sk, root_pk) = generate_keypair();
    let (alice_sk, alice_pk) = generate_keypair();
    let (_bob_sk, bob_pk) = generate_keypair();

    // Root grants Alice an Editor bundle: read/comment/write.
    let root = mint_root(
        spec(
            alice_pk,
            Role::Editor,
            &[Verb::Read, Verb::Comment, Verb::Write],
        ),
        &root_sk,
    )
    .unwrap();

    // Alice (the holder) attenuates: hand Bob a comment-only subset.
    let child = attenuate(
        &root,
        spec(bob_pk, Role::Commenter, &[Verb::Read, Verb::Comment]),
        &alice_sk,
    )
    .expect("dropping caps must be a valid attenuation");

    // The chain verifies and the effective (tightest) grant is Bob's
    // comment-only bundle.
    let chain = [root, child];
    let effective = verify_chain(&chain, t(10), &[root_pk]).expect("narrowing chain must verify");
    assert_eq!(effective.subject, bob_pk);
    assert_eq!(effective.caps, verbs(&[Verb::Read, Verb::Comment]));
    assert!(!effective.caps.contains(&Verb::Write), "write was dropped");
}

// Attenuation: adding a cap the parent LACKS is rejected at mint.

#[test]
fn derived_token_cannot_add_a_cap_the_parent_lacks() {
    let (root_sk, _root_pk) = generate_keypair();
    let (alice_sk, alice_pk) = generate_keypair();
    let (_bob_sk, bob_pk) = generate_keypair();

    // Root grants Alice comment-only (no Write).
    let root = mint_root(
        spec(alice_pk, Role::Commenter, &[Verb::Read, Verb::Comment]),
        &root_sk,
    )
    .unwrap();

    // Alice tries to hand Bob a token that ADDS Write — a widening.
    let err = attenuate(
        &root,
        spec(
            bob_pk,
            Role::Editor,
            &[Verb::Read, Verb::Comment, Verb::Write],
        ),
        &alice_sk,
    )
    .unwrap_err();
    assert!(
        matches!(err, InviteError::Widens(_)),
        "adding Write must be rejected as widening, got {err:?}"
    );
}

// Raising the ROLE above the parent is a widening too.

#[test]
fn derived_token_cannot_raise_role() {
    let (root_sk, _root_pk) = generate_keypair();
    let (alice_sk, alice_pk) = generate_keypair();
    let (_bob_sk, bob_pk) = generate_keypair();

    let root = mint_root(
        spec(alice_pk, Role::Commenter, &[Verb::Read, Verb::Comment]),
        &root_sk,
    )
    .unwrap();

    // Same caps, but role label raised to Owner — rejected.
    let err = attenuate(
        &root,
        spec(bob_pk, Role::Owner, &[Verb::Read, Verb::Comment]),
        &alice_sk,
    )
    .unwrap_err();
    assert!(
        matches!(err, InviteError::Widens(_)),
        "raising role must widen"
    );
}

// A child may pin to ONE sub-resource; it may not unpin or repoint.

#[test]
fn sub_resource_pin_only_narrows() {
    let (root_sk, root_pk) = generate_keypair();
    let (alice_sk, alice_pk) = generate_keypair();
    let (bob_sk, bob_pk) = generate_keypair();
    let (_carol_sk, carol_pk) = generate_keypair();
    let doc = ArtifactId::new();
    let other = ArtifactId::new();

    // Root: whole-workspace editor.
    let root = mint_root(
        spec(
            alice_pk,
            Role::Editor,
            &[Verb::Read, Verb::Comment, Verb::Write],
        ),
        &root_sk,
    )
    .unwrap();

    // Alice pins Bob to a single artifact — valid narrowing.
    let pinned = attenuate(
        &root,
        InviteSpec {
            sub_resource: Some(doc),
            ..spec(
                bob_pk,
                Role::Editor,
                &[Verb::Read, Verb::Comment, Verb::Write],
            )
        },
        &alice_sk,
    )
    .expect("pinning an unpinned parent is a narrowing");
    verify_chain(&[root, pinned.clone()], t(1), &[root_pk]).unwrap();

    // Bob cannot repoint his pinned token to a DIFFERENT artifact.
    let err = attenuate(
        &pinned,
        InviteSpec {
            sub_resource: Some(other),
            ..spec(
                carol_pk,
                Role::Editor,
                &[Verb::Read, Verb::Comment, Verb::Write],
            )
        },
        &bob_sk,
    )
    .unwrap_err();
    assert!(
        matches!(err, InviteError::Widens(_)),
        "repointing to a different artifact must widen, got {err:?}"
    );

    // Keeping the SAME pin is fine.
    let same = attenuate(
        &pinned,
        InviteSpec {
            sub_resource: Some(doc),
            ..spec(carol_pk, Role::Commenter, &[Verb::Read, Verb::Comment])
        },
        &bob_sk,
    )
    .expect("keeping the same pin + dropping caps is a narrowing");
    assert_eq!(same.sub_resource, Some(doc));
}

// An expired token is rejected at verification.

#[test]
fn expired_token_is_rejected() {
    let (root_sk, root_pk) = generate_keypair();
    let (_alice_sk, alice_pk) = generate_keypair();

    // Root valid until t=100.
    let root = mint_root(
        InviteSpec {
            not_after: Some(t(100)),
            ..spec(
                alice_pk,
                Role::Editor,
                &[Verb::Read, Verb::Comment, Verb::Write],
            )
        },
        &root_sk,
    )
    .unwrap();

    // Before expiry: verifies.
    verify_chain(std::slice::from_ref(&root), t(50), &[root_pk])
        .expect("token valid before expiry");
    // At/after expiry: rejected.
    assert_eq!(
        verify_chain(std::slice::from_ref(&root), t(100), &[root_pk]).unwrap_err(),
        InviteError::Expired
    );
    assert_eq!(
        verify_chain(std::slice::from_ref(&root), t(200), &[root_pk]).unwrap_err(),
        InviteError::Expired
    );
}

// A child may not outlive its parent's window.

#[test]
fn child_expiry_cannot_exceed_parent() {
    let (root_sk, _root_pk) = generate_keypair();
    let (alice_sk, alice_pk) = generate_keypair();
    let (_bob_sk, bob_pk) = generate_keypair();

    let root = mint_root(
        InviteSpec {
            not_after: Some(t(100)),
            ..spec(
                alice_pk,
                Role::Editor,
                &[Verb::Read, Verb::Comment, Verb::Write],
            )
        },
        &root_sk,
    )
    .unwrap();

    // Child claims a LATER expiry than the parent — widening.
    let err = attenuate(
        &root,
        InviteSpec {
            not_after: Some(t(500)),
            ..spec(
                bob_pk,
                Role::Editor,
                &[Verb::Read, Verb::Comment, Verb::Write],
            )
        },
        &alice_sk,
    )
    .unwrap_err();
    assert!(
        matches!(err, InviteError::Widens(_)),
        "later expiry must widen"
    );

    // A child that tightens the window (earlier expiry) is fine.
    let ok = attenuate(
        &root,
        InviteSpec {
            not_after: Some(t(80)),
            ..spec(
                bob_pk,
                Role::Editor,
                &[Verb::Read, Verb::Comment, Verb::Write],
            )
        },
        &alice_sk,
    )
    .expect("earlier expiry is a narrowing");
    assert_eq!(ok.not_after, Some(t(80)));
}

// Only the holder may attenuate; a non-holder is rejected.

#[test]
fn only_the_holder_may_attenuate() {
    let (root_sk, _root_pk) = generate_keypair();
    let (_alice_sk, alice_pk) = generate_keypair();
    let (mallory_sk, _mallory_pk) = generate_keypair();
    let (_bob_sk, bob_pk) = generate_keypair();

    let root = mint_root(
        spec(
            alice_pk,
            Role::Editor,
            &[Verb::Read, Verb::Comment, Verb::Write],
        ),
        &root_sk,
    )
    .unwrap();

    // Mallory (not the subject) tries to delegate Alice's token.
    let err = attenuate(
        &root,
        spec(bob_pk, Role::Commenter, &[Verb::Read, Verb::Comment]),
        &mallory_sk,
    )
    .unwrap_err();
    assert_eq!(err, InviteError::NotHolder);
}

// A chain whose root is not a trusted authority is rejected.

#[test]
fn untrusted_root_is_rejected() {
    let (root_sk, _root_pk) = generate_keypair();
    let (_alice_sk, alice_pk) = generate_keypair();
    let (_stranger_sk, stranger_pk) = generate_keypair();

    let root = mint_root(
        spec(
            alice_pk,
            Role::Editor,
            &[Verb::Read, Verb::Comment, Verb::Write],
        ),
        &root_sk,
    )
    .unwrap();

    // The verifier trusts `stranger`, not the actual signer.
    assert_eq!(
        verify_chain(std::slice::from_ref(&root), t(1), &[stranger_pk]).unwrap_err(),
        InviteError::UntrustedRoot
    );
}

// A forged wider child — re-signed with the delegator's real key so
// its OWN signature is valid — is still caught by verify_chain's
// narrowing check.

#[test]
fn forged_wider_child_is_caught_at_verification() {
    let (root_sk, root_pk) = generate_keypair();
    let (alice_sk, alice_pk) = generate_keypair();
    let (_bob_sk, bob_pk) = generate_keypair();

    let root = mint_root(
        spec(alice_pk, Role::Commenter, &[Verb::Read, Verb::Comment]),
        &root_sk,
    )
    .unwrap();

    // Alice mints a legitimately-narrow child (read-only)...
    let mut forged = attenuate(
        &root,
        spec(bob_pk, Role::Commenter, &[Verb::Read]),
        &alice_sk,
    )
    .unwrap();

    // ...then widens its caps and re-signs with her REAL key so the
    // signature is valid. `attenuate` would refuse this, so we mutate +
    // re-sign directly to prove verify_chain is the second line of
    // defence.
    forged.caps = verbs(&[Verb::Read, Verb::Comment, Verb::Write]);
    forged.sign_with(&alice_sk).unwrap();
    forged
        .verify_signature()
        .expect("the forged token's own signature is valid");

    let err = verify_chain(&[root, forged], t(1), &[root_pk]).unwrap_err();
    assert!(
        matches!(err, InviteError::Widens(_)),
        "verify_chain must reject a validly-signed but widened child, got {err:?}"
    );
}
