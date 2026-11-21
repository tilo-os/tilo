//! Names the workspace keeps for nobody it holds.

use crate::TransportOrigin;
use crate::admission_tests::{add_editor, seeded};

/// A person the workspace names goes with their name: removed, no name stays
/// for nobody the workspace holds, so its security basis, which every
/// device's write reads, stays readable. A name for nobody that a merge
/// leaves (the person named while they were removed) is dropped by the next
/// device that publishes governance.
#[test]
fn a_removed_persons_name_goes_with_them() {
    let (mut peer, authority_key, delegate_key, _, now) = seeded();
    let (_, root) = add_editor(&mut peer, &authority_key, now);
    assert!(
        peer.governance_mut()
            .name_person(root, "Ada", &authority_key, now)
            .unwrap()
    );
    let mut named = peer.clone();
    assert!(
        peer.governance_mut()
            .remove_member(root, &authority_key, now)
            .unwrap()
    );
    peer.security_basis()
        .unwrap()
        .expect("the basis reads after a removal");

    // Another device of the workspace's names them on a copy that has not heard
    // of the removal, and the two merge.
    assert!(
        named
            .governance_mut()
            .name_person(root, "Ada Lovelace", &delegate_key, now)
            .unwrap()
    );
    let origin = TransportOrigin::ServerDelta(delegate_key.verifying_key().to_bytes());
    peer.receive_from(origin, named.workspace_message(), now)
        .expect("the naming merges");
    assert_eq!(
        peer.security_basis().unwrap().unwrap_err().to_string(),
        format!(
            "the workspace keeps a name for {}, who is no longer a member nor one of its devices",
            hex::encode(root)
        ),
        "the refusal says what it found"
    );
    assert!(
        peer.governance_mut()
            .drop_names_of_nobody(&authority_key, now)
            .unwrap()
    );
    peer.security_basis()
        .unwrap()
        .expect("the basis reads once the name is dropped");
    assert!(
        !peer
            .governance_mut()
            .drop_names_of_nobody(&authority_key, now)
            .unwrap(),
        "nothing is left to drop"
    );
}

/// A person the workspace names by their root alone, held before any device of theirs joins, may
/// be handed the workspace: the handing over takes them out of its members, and their name names
/// its owner, so the workspace's security basis still reads, and no name is dropped as nobody's.
#[test]
fn a_named_person_handed_the_workspace_keeps_their_name() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32])
        .verifying_key()
        .to_bytes();
    assert!(
        peer.governance_mut()
            .declare_member(
                root,
                crate::join::Role::Editor,
                true,
                Some("Grace Hopper"),
                &authority_key,
                now,
            )
            .unwrap()
    );
    assert!(
        peer.governance_mut()
            .transfer_ownership(root, &authority_key, now)
            .expect("the owner hands the workspace to the person it names")
    );
    peer.security_basis()
        .unwrap()
        .expect("the basis reads with the new owner's name in it");
    assert!(
        !peer
            .governance_mut()
            .drop_names_of_nobody(&authority_key, now)
            .unwrap(),
        "the new owner's name is somebody's"
    );
}
