//! Who holds a workspace's keys, as the workspace's governance says.
use super::*;

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

/// A device certified by its own person's root, both keys derived from `n`.
fn person_device(n: usize) -> (SigningKey, DeviceCert) {
    let seed = |kind: u8| {
        let mut bytes = [kind; 32];
        bytes[..8].copy_from_slice(&u64::try_from(n).unwrap().to_le_bytes());
        bytes
    };
    let root = SigningKey::from_bytes(&seed(0x40));
    let key = SigningKey::from_bytes(&seed(0x80));
    let cert = crate::fixture::cert(
        &root,
        &key,
        tilo_user_identity::DeviceCapabilities::member(),
    );
    (key, cert)
}

/// Workspace `workspace` of `people` editors and its owner, whose grants keep the first
/// person from `named` records: the host peer, and the people's devices.
fn crowded_workspace(
    workspace: &str,
    people: usize,
    named: usize,
    now: SystemTime,
) -> (StatePeer, Vec<DeviceCert>) {
    let (owner_key, owner) = person_device(people);
    let people: Vec<DeviceCert> = (0..people).map(|n| person_device(n).1).collect();
    let roster: Vec<DeviceCert> = std::iter::once(owner.clone())
        .chain(people.iter().cloned())
        .collect();
    let mut doc =
        crate::workspace::candidate(None, &seed_workspace(workspace, &owner, &roster).unwrap())
            .unwrap();
    let hidden = hex::encode(people[0].user_public_key);
    let changes = people
        .iter()
        .map(|cert| tilo_action_policy::Change::AddMember {
            person: hex::encode(cert.user_public_key),
            role: Role::Editor,
        })
        .chain((0..named).map(|n| {
            tilo_action_policy::Change::Grant(Grant::new(
                format!("hidden-{n}"),
                Grantee::person(hidden.as_str()),
                collab_types::document_node(workspace, "so.tilo.chat", &format!("tab:{n}")),
                tilo_action_types::KindSet::READ,
                tilo_action_policy::Answer::Deny,
                Grantee::person(hex::encode(owner.user_public_key)),
            ))
        }))
        .collect();
    crate::members::op::tests_support::sign_into(
        &mut doc,
        &(owner_key.clone(), owner.clone()),
        crate::state::now_ns(now),
        changes,
    );
    doc.commit();
    let workspace_changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let mut peer = StatePeer::new(workspace, owner, roster, RevocationList::new(), now).unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes,
            documents: vec![],
        },
        &owner_key,
        now,
    )
    .unwrap();
    (peer, people)
}

/// The keys of a group some members read are held by those people's devices
/// among the workspace's key holders, and by no one else's.
#[test]
fn a_reader_sets_keys_are_held_by_its_peoples_devices_alone() {
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let (peer, people) = crowded_workspace(WORKSPACE, 3, 0, now);
    let readers: std::collections::BTreeSet<[u8; 32]> = people[..2]
        .iter()
        .map(|cert| cert.user_public_key)
        .chain([[0xee; 32]])
        .collect();
    let readers = collab_types::NamedReaders {
        role: None,
        people: readers,
    };
    let holders = peer.governance().reader_key_holders(&readers, now).unwrap();
    let theirs: std::collections::BTreeSet<[u8; 32]> = people[..2]
        .iter()
        .map(|cert| cert.device_public_key)
        .collect();
    assert_eq!(holders, theirs);
    assert!(!holders.contains(&people[2].device_public_key));
}

/// A set naming a role is read by its listed people and by every member whose role includes
/// it, though the set lists none of them: the workspace's owner reads a set naming moderators,
/// its devices hold the set's keys, and a device that follows its groups for them (a page) is
/// one of the set's readers; an editor it does not list is neither.
#[test]
fn a_role_reader_holds_the_keys_and_reads_without_being_named() {
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let (peer, people) = crowded_workspace(WORKSPACE, 3, 0, now);
    let owner = person_device(3).1;
    let readers = collab_types::NamedReaders {
        role: Some(Role::Moderator),
        people: std::collections::BTreeSet::from([people[0].user_public_key]),
    };
    let holders = peer.governance().reader_key_holders(&readers, now).unwrap();
    assert_eq!(
        holders,
        std::collections::BTreeSet::from([people[0].device_public_key, owner.device_public_key]),
        "the listed person and the owner, whose role includes moderating"
    );
    assert!(
        peer.governance()
            .reads_named(&readers, &owner.user_public_key, now),
        "the owner reads the set without being listed"
    );
    assert!(
        peer.governance()
            .reads_named(&readers, &people[0].user_public_key, now)
    );
    assert!(
        !peer
            .governance()
            .reads_named(&readers, &people[1].user_public_key, now),
        "an editor it does not list reads none of it"
    );
}
