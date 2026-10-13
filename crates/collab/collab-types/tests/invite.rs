//! Attenuable invite tokens: a derived token can lower the role but raising
//! it above the parent's is rejected; an expired token is rejected. All
//! proven on the real `mint_root` / `attenuate` / `verify_chain` path.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, SystemTime};

use collab_types::{
    ArtifactId, InviteError, InviteSpec, Role, attenuate, generate_keypair, mint_root, verify_chain,
};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

const WORKSPACE: u64 = 0xA11CE;

/// Root grant: `role` for `subject`, whole-workspace, no expiry unless
/// overridden.
fn spec(subject: collab_types::Ed25519PublicKey, role: Role) -> InviteSpec {
    InviteSpec {
        subject,
        workspace: WORKSPACE,
        role,
        sub_resource: None,
        not_after: None,
    }
}

/// `spec` with an expiry.
fn expiring(spec: InviteSpec, not_after: SystemTime) -> InviteSpec {
    InviteSpec {
        not_after: Some(not_after),
        ..spec
    }
}

/// `spec` pinned to one artifact.
fn pinned_to(spec: InviteSpec, doc: ArtifactId) -> InviteSpec {
    InviteSpec {
        sub_resource: Some(doc),
        ..spec
    }
}

/// Attenuation that narrows is valid: lowering the role (and the chain then
/// verifies to the tightest grant), pinning to one sub-resource or keeping
/// the same pin, and tightening the expiry window.
#[test]
fn a_holder_may_attenuate_by_lowering_the_role_pinning_or_shortening_expiry() {
    // Case: a derived token may lower the role.
    let (root_sk, root_pub) = generate_keypair();
    let (alice_sk, alice_pub) = generate_keypair();
    let (bob_sk, bob_pub) = generate_keypair();
    let (_carol_sk, carol_pub) = generate_keypair();

    // Root admits Alice as an Editor.
    let root = mint_root(spec(alice_pub, Role::Editor), &root_sk).unwrap();
    // Alice (the holder) attenuates: hand Bob the Commenter role.
    let child = attenuate(&root, spec(bob_pub, Role::Commenter), &alice_sk)
        .expect("lowering the role must be a valid attenuation");
    // The chain verifies and the effective (tightest) grant is Bob's
    // Commenter role.
    let chain = [root.clone(), child];
    let effective = verify_chain(&chain, t(10), &[root_pub]).expect("narrowing chain must verify");
    assert_eq!(effective.subject, bob_pub);
    assert_eq!(effective.role, Role::Commenter);

    // Case: a sub-resource pin only narrows. A child may pin to ONE
    // sub-resource; it may not unpin or repoint.
    let doc = ArtifactId::new();
    let other = ArtifactId::new();
    // Alice pins Bob to a single artifact (from the whole-workspace editor
    // root) — valid narrowing.
    let pinned = attenuate(
        &root,
        pinned_to(spec(bob_pub, Role::Editor), doc),
        &alice_sk,
    )
    .expect("pinning an unpinned parent is a narrowing");
    verify_chain(&[root, pinned.clone()], t(1), &[root_pub]).unwrap();
    // Bob cannot repoint his pinned token to a DIFFERENT artifact.
    let err = attenuate(
        &pinned,
        pinned_to(spec(carol_pub, Role::Editor), other),
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
        pinned_to(spec(carol_pub, Role::Commenter), doc),
        &bob_sk,
    )
    .expect("keeping the same pin + lowering the role is a narrowing");
    assert_eq!(same.sub_resource, Some(doc));

    // Case: a child that tightens the window (earlier expiry) is fine.
    let (root_sk, _root_pub) = generate_keypair();
    let root = mint_root(expiring(spec(alice_pub, Role::Editor), t(100)), &root_sk).unwrap();
    let ok = attenuate(
        &root,
        expiring(spec(bob_pub, Role::Editor), t(80)),
        &alice_sk,
    )
    .expect("earlier expiry is a narrowing");
    assert_eq!(ok.not_after, Some(t(80)));
}

/// Attenuation that widens — raising the role, or outliving the parent's window — is rejected at mint, and only
/// the holder may attenuate at all.
#[test]
fn attenuation_rejects_every_widening_and_every_non_holder() {
    type Specs = fn(
        collab_types::Ed25519PublicKey,
        collab_types::Ed25519PublicKey,
    ) -> (InviteSpec, InviteSpec);
    // Root admits Alice as a Commenter; Alice tries to hand Bob the Editor
    // role — a widening.
    let adds_write: Specs = |alice, bob| (spec(alice, Role::Commenter), spec(bob, Role::Editor));
    // Raising the role to Owner is a widening too.
    let raises_role: Specs = |alice, bob| (spec(alice, Role::Commenter), spec(bob, Role::Owner));
    // A child may not outlive its parent's window: the child claims a LATER
    // expiry than the parent.
    let later_expiry: Specs = |alice, bob| {
        (
            expiring(spec(alice, Role::Editor), t(100)),
            expiring(spec(bob, Role::Editor), t(500)),
        )
    };
    for (why, specs) in [
        ("adding Write must be rejected as widening", adds_write),
        ("raising role must widen", raises_role),
        ("later expiry must widen", later_expiry),
    ] {
        let (root_sk, _root_pub) = generate_keypair();
        let (alice_sk, alice_pub) = generate_keypair();
        let (_bob_sk, bob_pub) = generate_keypair();
        let (parent, child) = specs(alice_pub, bob_pub);
        let root = mint_root(parent, &root_sk).unwrap();
        let err = attenuate(&root, child, &alice_sk).unwrap_err();
        assert!(matches!(err, InviteError::Widens(_)), "{why}, got {err:?}");
    }

    // Case: only the holder may attenuate; Mallory (not the subject) tries
    // to delegate Alice's token.
    let (root_sk, _root_pub) = generate_keypair();
    let (_alice_sk, alice_pub) = generate_keypair();
    let (mallory_sk, _mallory_pub) = generate_keypair();
    let (_bob_sk, bob_pub) = generate_keypair();
    let root = mint_root(spec(alice_pub, Role::Editor), &root_sk).unwrap();
    let err = attenuate(&root, spec(bob_pub, Role::Commenter), &mallory_sk).unwrap_err();
    assert_eq!(err, InviteError::NotHolder);
}

/// `verify_chain` rejects an expired token, a chain whose root is not a
/// trusted authority, and a forged wider child whose own signature is valid.
#[test]
fn verify_chain_rejects_expired_untrusted_or_forged_wider_chains() {
    // Case: an expired token is rejected at verification. Root valid until
    // t=100.
    let (root_sk, root_pub) = generate_keypair();
    let (_alice_sk, alice_pub) = generate_keypair();
    let root = mint_root(expiring(spec(alice_pub, Role::Editor), t(100)), &root_sk).unwrap();
    // Before expiry: verifies.
    verify_chain(std::slice::from_ref(&root), t(50), &[root_pub])
        .expect("token valid before expiry");
    // At/after expiry: rejected.
    for now in [t(100), t(200)] {
        assert_eq!(
            verify_chain(std::slice::from_ref(&root), now, &[root_pub]).unwrap_err(),
            InviteError::Expired
        );
    }

    // Case: a chain whose root is not a trusted authority is rejected. The
    // verifier trusts `stranger`, not the actual signer.
    let (root_sk, _root_pub) = generate_keypair();
    let (_alice_sk, alice_pub) = generate_keypair();
    let (_stranger_sk, stranger_pub) = generate_keypair();
    let root = mint_root(spec(alice_pub, Role::Editor), &root_sk).unwrap();
    assert_eq!(
        verify_chain(std::slice::from_ref(&root), t(1), &[stranger_pub]).unwrap_err(),
        InviteError::UntrustedRoot
    );

    // Case: a forged wider child — re-signed with the delegator's real key
    // so its OWN signature is valid — is still caught by verify_chain's
    // narrowing check.
    let (root_sk, root_pub) = generate_keypair();
    let (alice_sk, alice_pub) = generate_keypair();
    let (_bob_sk, bob_pub) = generate_keypair();
    let root = mint_root(spec(alice_pub, Role::Commenter), &root_sk).unwrap();
    // Alice mints a legitimately-narrow child (Viewer)...
    let mut forged = attenuate(&root, spec(bob_pub, Role::Viewer), &alice_sk).unwrap();
    // ...then raises its role and re-signs with her REAL key so the
    // signature is valid. `attenuate` would refuse this, so we mutate +
    // re-sign directly to prove verify_chain is the second line of
    // defence.
    forged.role = Role::Editor;
    forged.sign_with(&alice_sk).unwrap();
    forged
        .verify_signature()
        .expect("the forged token's own signature is valid");
    let err = verify_chain(&[root, forged], t(1), &[root_pub]).unwrap_err();
    assert!(
        matches!(err, InviteError::Widens(_)),
        "verify_chain must reject a validly-signed but widened child, got {err:?}"
    );
}
