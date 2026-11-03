use super::*;
use crate::workspace::saved_shortcut::tests::{Device, NOW_NS, WORKSPACE, device};

fn pair(ada: &Device, bob: &Device) -> (String, DirectPair) {
    let mut people = [ada.person(), bob.person()];
    people.sort_unstable();
    DirectPair::signed(WORKSPACE, people, &ada.cert, &ada.key).unwrap()
}

fn none() -> RevocationList {
    RevocationList::new()
}

#[test]
fn a_pairs_own_device_proves_which_two_people_it_is_between() {
    let ada = device(1, 2);
    let bob = device(3, 4);
    let (id, announcement) = pair(&ada, &bob);
    assert_eq!(
        proven_pair(WORKSPACE, &id, &announcement, &none(), NOW_NS),
        Some(announcement.people.clone())
    );
    assert_eq!(
        collab_types::direct_pair_id(WORKSPACE, &ada.person(), &bob.person()).as_deref(),
        Some(id.as_str())
    );
}

#[test]
fn a_pair_nobody_in_it_proved_is_no_pair() {
    let ada = device(1, 2);
    let bob = device(3, 4);
    let carol = device(5, 6);
    let (id, announcement) = pair(&ada, &bob);

    let mut unproven = announcement.clone();
    unproven.proof.signature = String::new();
    assert_eq!(
        proven_pair(WORKSPACE, &id, &unproven, &none(), NOW_NS),
        None
    );

    // Carol's device signing the same pair, which she is no part of.
    let mut people = [ada.person(), bob.person()];
    people.sort_unstable();
    let (forged_id, forged) =
        DirectPair::signed(WORKSPACE, people, &carol.cert, &carol.key).unwrap();
    assert_eq!(forged_id, id);
    assert_eq!(proven_pair(WORKSPACE, &id, &forged, &none(), NOW_NS), None);

    // A proof from another workspace, and under another pair id.
    let (_, other_workspace) = DirectPair::signed(
        "5e1d9a40-3c2b-4f6e-8a7d-1b2c3d4e5f60",
        announcement.people.clone(),
        &ada.cert,
        &ada.key,
    )
    .unwrap();
    assert_eq!(
        proven_pair(WORKSPACE, &id, &other_workspace, &none(), NOW_NS),
        None
    );
    let (unrelated, unrelated_announcement) = pair(&ada, &carol);
    assert_ne!(unrelated, id);
    assert_eq!(
        proven_pair(WORKSPACE, &id, &unrelated_announcement, &none(), NOW_NS),
        None
    );

    // Two people the id does not derive from.
    let mut swapped = announcement;
    swapped.people.swap(0, 1);
    assert_eq!(proven_pair(WORKSPACE, &id, &swapped, &none(), NOW_NS), None);
}

#[test]
fn a_device_revoked_after_it_proved_a_pair_proves_nothing_from_then() {
    let ada = device(1, 2);
    let bob = device(3, 4);
    let (id, announcement) = pair(&ada, &bob);
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: ada.cert.user_public_key,
        revoked_device_public_key: ada.cert.device_public_key,
        revoked_at_ns: NOW_NS,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &ada.root).unwrap();
    assert_eq!(
        proven_pair(WORKSPACE, &id, &announcement, &none(), NOW_NS),
        Some(announcement.people.clone())
    );
    let mut revocations = RevocationList::new();
    revocations.insert(entry).unwrap();
    assert_eq!(
        proven_pair(WORKSPACE, &id, &announcement, &revocations, NOW_NS),
        None
    );
}

#[test]
fn a_document_holds_its_pairs_as_written() {
    use automerge::transaction::Transactable as _;
    let ada = device(1, 2);
    let bob = device(3, 4);
    let (id, announcement) = pair(&ada, &bob);
    let mut doc = AutoCommit::new();
    let map = doc
        .put_object(automerge::ROOT, DIRECT_PAIRS_KEY, automerge::ObjType::Map)
        .unwrap();
    let bytes = serde_json::to_vec(&announcement).unwrap();
    doc.put(
        &map,
        id.as_str(),
        automerge::ScalarValue::Bytes(bytes.clone()),
    )
    .unwrap();
    assert_eq!(
        direct_pairs_of(&doc),
        std::collections::BTreeMap::from([(id, bytes)])
    );
}
