//! A workspace's owner hands it to another person: from the transfer on,
//! every device that replays it holds the new owner, whenever the transfer
//! reached it, and the owner it replaced stays as an administrator.
use super::*;
use crate::admission_tests::{add_editor, add_member_with, seeded};
use tilo_action_policy::roles;

/// A replica of the workspace `owner_copy` hosts, joined through `through`, a
/// device of the host's, built from what it publishes now.
fn replica(
    owner_copy: &mut StatePeer,
    key: &SigningKey,
    through: &DeviceCert,
    now: SystemTime,
) -> StatePeer {
    let mut replica = StatePeer::from_join(
        owner_copy.workspace_id(),
        through.clone(),
        owner_copy.certificates(),
        &owner_copy.revocations(),
        now,
    )
    .unwrap();
    take_in(&mut replica, owner_copy, key, through, now);
    replica
}

/// `replica` takes in the governance `owner_copy` publishes now, from `through`.
fn take_in(
    replica: &mut StatePeer,
    owner_copy: &mut StatePeer,
    key: &SigningKey,
    through: &DeviceCert,
    now: SystemTime,
) {
    for message in owner_copy
        .publication_mut()
        .bootstrap_messages(key, now)
        .unwrap()
    {
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            continue;
        }
        replica
            .receive_from(
                crate::TransportOrigin::RetainedServerDelta(through.device_public_key),
                message,
                now,
            )
            .unwrap();
    }
}

tilo_test_env::scenario! {
    fn an_owner_hands_the_workspace_to_another_person;

    phase every_replica_holds_the_new_owner_whenever_the_transfer_reached_it;
    {
        let (mut owner_copy, owner_key, _, _, now) = seeded();
        let owner_cert = owner_copy
            .owner_certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == owner_key.verifying_key().to_bytes())
            .unwrap();
        let first_owner = owner_copy.owner();
        let (editor, person) = add_editor(&mut owner_copy, &owner_key, now);
        let editor_key = SigningKey::from_bytes(&[6; 32]);
        // A replica made before the transfer, and one made after it.
        let mut early = replica(&mut owner_copy, &owner_key, &owner_cert, now);
        assert_eq!(early.owner(), first_owner);

        // A member hands over nothing that is not theirs.
        assert!(
            owner_copy
                .clone()
                .governance_mut()
                .transfer_ownership(person, &editor_key, now)
                .is_err()
        );
        assert!(
            owner_copy
                .governance_mut()
                .transfer_ownership(person, &owner_key, now)
                .unwrap()
        );
        assert!(
            !owner_copy
                .governance_mut()
                .transfer_ownership(person, &owner_key, now)
                .unwrap(),
            "handing it to its owner changes nothing"
        );
        assert_eq!(owner_copy.owner(), person);
        let policy = owner_copy.governance().member_policy().unwrap();
        let (new, old) = (hex::encode(person), hex::encode(first_owner));
        assert!(policy.is_owner(&new));
        assert!(!policy.members.contains_key(&new), "an owner is no member");
        assert!(!policy.is_administrator(&new));
        assert_eq!(policy.role_of(&old), Some(Role::Editor));
        assert!(policy.is_administrator(&old), "the owner it replaced administers it");
        let grant = policy
            .grants
            .iter()
            .find(|grant| grant.id == roles::administrator_grant_id(&old))
            .unwrap();
        assert_eq!(grant.granted_by, tilo_action_policy::Grantee::person(old.clone()));

        let late = replica(&mut owner_copy, &owner_key, &owner_cert, now);
        take_in(&mut early, &mut owner_copy, &owner_key, &owner_cert, now);
        for (who, peer) in [("early", &early), ("late", &late)] {
            assert_eq!(peer.owner(), person, "the {who} replica's owner");
            assert_eq!(
                peer.governance().member_policy().unwrap(),
                policy,
                "the {who} replica's governance"
            );
            assert_eq!(
                peer.governance().key_holders(now).unwrap(),
                owner_copy.governance().key_holders(now).unwrap(),
                "the {who} replica's key holders"
            );
            assert_eq!(
                peer.owner_certificates()
                    .iter()
                    .map(|cert| cert.device_public_key)
                    .collect::<Vec<_>>(),
                [editor.device_public_key],
                "the {who} replica's owner devices"
            );
        }
        assert!(
            owner_copy
                .governance()
                .key_holders(now)
                .unwrap()
                .contains(&owner_cert.device_public_key),
            "the owner it replaced keeps the keys as a member"
        );

        // Kept and read back, the workspace is still the new owner's.
        let restored = StatePeer::restore(
            &owner_copy.durable().unwrap(),
            owner_copy.workspace_id(),
            owner_cert.clone(),
            owner_copy.certificates(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        assert_eq!(restored.owner(), person);

        // The owner it replaced hands it on no more; the new owner does.
        assert!(
            owner_copy
                .clone()
                .governance_mut()
                .transfer_ownership(first_owner, &owner_key, now)
                .is_err()
        );
        let mut back = owner_copy.clone();
        assert!(
            back.governance_mut()
                .transfer_ownership(first_owner, &editor_key, now)
                .unwrap()
        );
        assert_eq!(back.owner(), first_owner);
        assert!(
            back.governance()
                .member_policy()
                .unwrap()
                .is_administrator(&new)
        );
    }
    phase the_owner_it_replaced_keeps_serving_as_an_administrator;
    {
        let (mut owner_copy, host_key, _, _, now) = seeded();
        let host = owner_copy
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == host_key.verifying_key().to_bytes())
            .unwrap();
        let (_, person) = add_editor(&mut owner_copy, &host_key, now);
        owner_copy
            .governance_mut()
            .transfer_ownership(person, &host_key, now)
            .unwrap();
        assert_ne!(owner_copy.owner(), host.user_public_key);
        assert!(owner_copy.administers_history(&host.user_public_key));
        assert!(
            owner_copy.signed_by_administrator(&host.device_public_key),
            "its checkpoints are the workspace's"
        );
        // It still admits whom an invitation names, and every replica takes
        // the admission it signed.
        let (_, _, bob) = add_member_with(&mut owner_copy, &host_key, now, 30, 31);
        let late = replica(&mut owner_copy, &host_key, &host, now);
        assert!(
            late.governance()
                .member_policy()
                .unwrap()
                .members
                .contains_key(&hex::encode(bob))
        );
        assert_eq!(late.owner(), person);
        // Withdrawn as an administrator by the owner, it keeps nothing.
        let owner_key = SigningKey::from_bytes(&[6; 32]);
        owner_copy
            .governance_mut()
            .set_member_role(host.user_public_key, crate::join::Role::Editor, &owner_key, now)
            .unwrap();
        assert!(!owner_copy.administers_history(&host.user_public_key));
        assert!(!owner_copy.signed_by_administrator(&host.device_public_key));
    }
    phase the_new_owner_keeps_the_name_the_workspace_gave_them;
    {
        // A host hands a person's workspace to them and names them, as it
        // does for a workspace it made for them: the owner is no member beside
        // it, and the name kept under their root still names somebody, so
        // the security basis every device's write reads stays readable.
        let (mut owner_copy, host_key, _, _, now) = seeded();
        let host = owner_copy
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == host_key.verifying_key().to_bytes())
            .unwrap();
        let (_, person) = add_editor(&mut owner_copy, &host_key, now);
        owner_copy
            .governance_mut()
            .transfer_ownership(person, &host_key, now)
            .unwrap();
        assert!(
            owner_copy
                .governance_mut()
                .name_person(person, "Ada", &host_key, now)
                .unwrap()
        );
        owner_copy
            .security_basis()
            .unwrap()
            .expect("the basis reads with the owner's name in it");
        let late = replica(&mut owner_copy, &host_key, &host, now);
        late.security_basis()
            .unwrap()
            .expect("a replica reads it alike");
    }
}

/// The owner's device, among the certificates a seeded workspace holds.
fn owner_device(owner_copy: &StatePeer, key: &SigningKey) -> DeviceCert {
    owner_copy
        .owner_certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == key.verifying_key().to_bytes())
        .expect("the owner's device")
}

/// A change of the workspace document written as `cert`'s device, on the
/// document `doc` now holds: the batch that would carry it.
fn written_by(
    doc: &AutoCommit,
    cert: &DeviceCert,
    write: impl FnOnce(&mut AutoCommit),
) -> Vec<Vec<u8>> {
    let mut next = doc.clone();
    let base = next.get_heads();
    next.set_actor(automerge::ActorId::from(cert.device_public_key.to_vec()));
    write(&mut next);
    commit_governance(&mut next, wall_secs(), None);
    next.get_changes(&base)
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// A governance state that names another owner than the one a replica holds,
/// carrying no ownership transfer, is no state of its workspace: taken in, it
/// would make the person the replica knows as its owner one of its members
/// instead.
#[test]
fn a_state_that_names_another_owner_without_a_transfer_is_refused() {
    let (mut owner_copy, owner_key, _, _, now) = seeded();
    let owner = owner_device(&owner_copy, &owner_key);
    let workspace = owner_copy.workspace_id().to_owned();
    let mut replica = replica(&mut owner_copy, &owner_key, &owner, now);
    let held = replica.owner();
    let governance = owner_copy
        .core
        .workspace_doc
        .as_deref()
        .expect("the workspace's governance");

    // The owner's own device writes someone else into the workspace's `owner`
    // row, and nothing else, so the whole replay names the other person.
    let changes = written_by(governance, &owner, |doc| {
        doc.put(
            ROOT,
            tilo_desktop_model::projection::WORKSPACE_OWNER_KEY,
            hex::encode([0x99; 32]),
        )
        .unwrap();
    });
    let refused = replica.receive(
        owner_key.verifying_key().to_bytes(),
        NeighbourMsg::SyncResp { changes, workspace },
        now,
    );
    assert!(
        refused.is_err(),
        "a state that names another owner without the transfer that hands it over is refused: {refused:?}"
    );
    assert_eq!(
        replica.owner(),
        held,
        "the replica keeps the owner it holds"
    );
    assert_eq!(
        replica
            .governance()
            .member_policy()
            .expect("its governance")
            .owner()
            .map(str::to_owned),
        Some(hex::encode(held)),
        "and the owner its governance names"
    );
}

/// A state that names no owner is a partial view of a workspace, not the
/// workspace: a replica that holds none yet takes none in, so it neither
/// forgets the owner it was joined as nor reads it as a member.
#[test]
fn a_state_that_names_no_owner_is_not_adopted() {
    let (owner_copy, owner_key, _, _, now) = seeded();
    let owner = owner_device(&owner_copy, &owner_key);
    let workspace = owner_copy.workspace_id().to_owned();
    let mut replica = StatePeer::from_join(
        &workspace,
        owner.clone(),
        owner_copy.certificates(),
        &owner_copy.revocations(),
        now,
    )
    .unwrap();
    let held = replica.owner();
    let governance = owner_copy
        .core
        .workspace_doc
        .as_deref()
        .expect("the workspace's governance");

    // One change on a history this replica holds nothing of: its dependencies
    // are absent, so the state it makes is empty and names no owner.
    let changes = written_by(governance, &owner, |doc| {
        doc.put(ROOT, "name", "A name no owner made").unwrap();
    });
    let refused = replica.receive(
        owner_key.verifying_key().to_bytes(),
        NeighbourMsg::SyncResp { changes, workspace },
        now,
    );
    assert!(
        refused.is_err(),
        "a state that names no owner is refused: {refused:?}"
    );
    assert!(
        replica.core.workspace_doc.is_none(),
        "nothing of it is taken in"
    );
    assert_eq!(replica.owner(), held, "the owner it was joined as stays");
}
