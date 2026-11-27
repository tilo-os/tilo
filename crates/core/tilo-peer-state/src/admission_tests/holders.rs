//! A holder of a workspace that is no member: it carries what members and
//! joining devices signed, byte for byte, and writes nothing of its own.
use super::*;
use crate::join::{Role, verify_connection_redemption};
use crate::state::DeviceRow;
use std::collections::BTreeSet;

/// `peer`, the owner's copy, with `holder` named among the workspace's
/// holders by its own signed address: another person's server keeping it.
fn holding(peer: &mut StatePeer, authority: &SigningKey, holder: (&SigningKey, &DeviceCert)) {
    let hint = tilo_workspace_join::sign_transport_hint(
        "iroh",
        "an address",
        holder.1.clone(),
        [3; 32],
        holder.0,
    )
    .unwrap();
    assert!(
        peer.governance_mut()
            .author_keeping_device(&hint, authority, tilo_clock::wall())
            .unwrap()
    );
}

/// The workspace's governance as a join answer carries it.
fn governance_of(peer: &StatePeer) -> Vec<Vec<u8>> {
    vec![postcard::to_stdvec(&peer.workspace_message()).unwrap()]
}

/// An invitation the owner (`authority`) issued for `guest`, redeemed by its
/// device under request `[96; 32]`.
fn invited(
    peer: &StatePeer,
    authority: &SigningKey,
    guest: &DeviceCert,
    now: SystemTime,
) -> crate::join::VerifiedRedemption {
    let signed = crate::fixture::invitation(peer, authority, now, |grant| {
        grant.nonce = [95; 16];
        grant.recipient_user_public_key = Some(guest.user_public_key);
    });
    let now = crate::fixture::ns(now);
    verify_connection_redemption(&signed, guest, [96; 32], &RevocationList::new(), now).unwrap()
}

/// What `held`'s copy took in since `heads`, as the changes it would carry on.
fn carried_since(held: &StatePeer, heads: &[automerge::ChangeHash]) -> BTreeSet<Vec<u8>> {
    held.workspace_changes_since(heads).into_iter().collect()
}

/// `changes` reaching `receiver` from `author`, as a holder relays a frame:
/// under the device that wrote it.
fn from_author(
    receiver: &mut StatePeer,
    author: [u8; 32],
    changes: Vec<Vec<u8>>,
    now: SystemTime,
) -> Result<(), String> {
    let workspace = receiver.workspace_id().to_owned();
    receiver
        .receive_from(
            TransportOrigin::ServerDelta(author),
            NeighbourMsg::SyncResp { changes, workspace },
            now,
        )
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The authors of the membership operations `peer` holds that `before` did not.
fn new_authors(before: &StatePeer, peer: &StatePeer) -> Vec<[u8; 32]> {
    let held = crate::members::ops(before.admitted_workspace().unwrap());
    crate::members::ops(peer.admitted_workspace().unwrap())
        .into_iter()
        .filter(|(id, _)| !held.contains_key(id))
        .map(|(_, op)| op.author.device_public_key)
        .collect()
}

/// A joining device signs its own admission, the invitation its authority. A
/// server of another person keeping the workspace takes it in as that device's
/// and carries it on byte for byte, and every member's device admits whom it
/// redeemed under the device that signed it: the server wrote no operation and
/// no row, and is no member for having carried it.
#[test]
fn a_joining_device_signs_its_own_admission_and_a_holder_carries_it() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let (holder_key, holder) = crate::fixture::device(91, 92);
    let (guest_key, guest) = crate::fixture::device(93, 94);
    holding(&mut peer, &authority_key, (&holder_key, &holder));
    let redemption = invited(&peer, &authority_key, &guest, now);
    let admission = crate::own_admission::own_admission(
        peer.workspace_id(),
        &governance_of(&peer),
        &redemption,
        Some("Guest"),
        &guest_key,
        now,
    )
    .unwrap();
    // A device signs only its own admission.
    assert!(
        crate::own_admission::own_admission(
            peer.workspace_id(),
            &governance_of(&peer),
            &redemption,
            None,
            &holder_key,
            now,
        )
        .is_err()
    );

    let heads = peer.workspace_heads().unwrap();
    let mut held = peer.clone();
    held.admit_own_redemption(&redemption, admission.clone(), now)
        .unwrap();
    assert_eq!(
        carried_since(&held, &heads),
        admission.iter().cloned().collect(),
        "the holder carries the device's admission byte for byte"
    );
    assert_eq!(new_authors(&peer, &held), vec![guest.device_public_key]);
    assert!(held.role(&holder.device_public_key, now).is_err());

    let mut member = peer.clone();
    from_author(
        &mut member,
        guest.device_public_key,
        carried_since(&held, &heads).into_iter().collect(),
        now,
    )
    .unwrap();
    assert_eq!(
        member.role(&guest.device_public_key, now).unwrap(),
        collab_types::Role::Editor
    );
    assert_eq!(
        new_authors(&peer, &member),
        vec![guest.device_public_key],
        "the one operation admitting the guest is the guest's own"
    );
    assert!(
        member.role(&holder.device_public_key, now).is_err(),
        "the holder is no member for having carried an admission"
    );
}

/// A member's further device and a key minted for the workspace each sign
/// their own row, admissible because their certificates chain to a root the
/// list names; a server of another person carries the row on, and every
/// receiver admits the device under its own operation. A device of a person
/// the workspace has no member of signs nothing in.
#[test]
fn a_further_device_signs_its_own_row_through_a_holder() {
    let (mut peer, authority_key, _, _, now) = seeded();
    let now_ns = crate::fixture::ns(now);
    let (holder_key, holder) = crate::fixture::device(91, 92);
    holding(&mut peer, &authority_key, (&holder_key, &holder));
    let (_, member, _) = add_member_with(&mut peer, &authority_key, now, 5, 6);
    let (further_key, further) = crate::fixture::device(5, 7);
    assert_ne!(member.device_public_key, further.device_public_key);
    let member_root = SigningKey::from_bytes(&[5; 32]);
    let backend_key = SigningKey::from_bytes(&[8; 32]);
    let mut backend = DeviceCert {
        user_public_key: member_root.verifying_key().to_bytes(),
        device_public_key: backend_key.verifying_key().to_bytes(),
        device_label: "backend".into(),
        capabilities: DeviceCapabilities::server_key_grant(),
        issued_at_ns: now_ns.saturating_sub(1),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut backend, &member_root).unwrap();
    let api_key_key = SigningKey::from_bytes(&[9; 32]);
    let mut api_key = tilo_user_identity::prepare_minted_grant(
        &backend,
        api_key_key.verifying_key().to_bytes(),
        *uuid::Uuid::parse_str(peer.workspace_id())
            .unwrap()
            .as_bytes(),
        "API key",
        now_ns,
        now_ns.saturating_add(3_600_000_000_000),
    );
    sign_device_cert(&mut api_key, &backend_key).unwrap();

    let mut receiver = peer.clone();
    for (key, cert) in [(&further_key, &further), (&api_key_key, &api_key)] {
        let row = crate::own_admission::further_device_row(
            peer.workspace_id(),
            &governance_of(&receiver),
            cert,
            key,
            now,
        )
        .unwrap();
        assert!(!row.is_empty());
        let mut held = receiver.clone();
        assert!(held.admit_own(cert, row.clone(), now).unwrap());
        assert!(held.role(&holder.device_public_key, now).is_err());
        let before = receiver.clone();
        from_author(&mut receiver, cert.device_public_key, row, now).unwrap();
        assert!(receiver.permissions(cert.device_public_key, now).is_ok());
        assert_eq!(
            new_authors(&before, &receiver),
            vec![cert.device_public_key],
            "the device's row is its own operation"
        );
    }
    // A device the workspace lists already writes nothing again.
    assert!(
        crate::own_admission::further_device_row(
            peer.workspace_id(),
            &governance_of(&receiver),
            &further,
            &further_key,
            now,
        )
        .unwrap()
        .is_empty()
    );

    let (stranger_key, stranger) = crate::fixture::device(93, 94);
    let refused = crate::own_admission::further_device_row(
        peer.workspace_id(),
        &governance_of(&peer),
        &stranger,
        &stranger_key,
        now,
    )
    .unwrap_err();
    assert!(refused.contains("not a member"), "{refused}");
}

/// A holder that is no member writes nothing of its own: not the row saying
/// where its copy stands, not a redemption it signed, not a renamed
/// workspace, not someone else's admission passed off as its own batch, not
/// an operation it backdated, and none of its record writes lands.
#[test]
fn a_holder_that_is_no_member_writes_nothing_of_its_own() {
    let (mut peer, authority_key, _, bc, now) = seeded();
    let workspace = peer.workspace_id().to_owned();
    let (holder_key, holder) = crate::fixture::device(91, 92);
    let (guest_key, guest) = crate::fixture::device(93, 94);
    holding(&mut peer, &authority_key, (&holder_key, &holder));
    let heads = peer.workspace_heads().unwrap();

    // Its own copy's row.
    says_nothing_of_its_copy(&peer, &holder, now);

    // A redemption it signs.
    let redemption = invited(&peer, &authority_key, &guest, now);
    assert!(
        peer.clone()
            .governance_mut()
            .admit_invited_member(&redemption, &holder_key, now)
            .is_err(),
        "a holder that is no member signs nobody in"
    );

    // The guest's own admission, sent as the holder's own batch.
    let admission = crate::own_admission::own_admission(
        &workspace,
        &governance_of(&peer),
        &redemption,
        None,
        &guest_key,
        now,
    )
    .unwrap();
    let mut receiver = peer.clone();
    assert!(
        from_author(&mut receiver, holder.device_public_key, admission, now).is_err(),
        "operations another device signed are no holder's own batch"
    );
    assert!(receiver.role(&guest.device_public_key, now).is_err());

    // An operation it signs itself, backdated to before anything it could
    // claim: its person held no role at any point.
    let mut doc = peer.admitted_workspace().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(holder.device_public_key.to_vec()));
    let op = crate::members::SignedMembershipOp::signed(
        &holder,
        &holder_key,
        crate::members::heads_of(&doc, &workspace),
        1,
        crate::members::MembershipOpCommand::Admit {
            invite: redemption.join().invite_id(),
            person: guest.user_public_key,
        },
    )
    .unwrap();
    crate::members::put_op(&mut doc, &op).unwrap();
    doc.commit();
    let backdated = doc
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    assert!(from_author(&mut receiver, holder.device_public_key, backdated, now).is_err());

    // A rename.
    let mut doc = peer.admitted_workspace().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(holder.device_public_key.to_vec()));
    doc.put(ROOT, "name", "Taken").unwrap();
    doc.commit_edit("rename", "name", &bc.user_public_key);
    let renamed = doc
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    assert!(from_author(&mut receiver, holder.device_public_key, renamed, now).is_err());
    assert_ne!(
        receiver.governance().workspace_name().as_deref(),
        Some("Taken")
    );
    assert_eq!(receiver.workspace_heads().unwrap(), heads);
}

/// A holder that is no member reports no copy of its own: neither through the
/// action that says where its copy stands nor as a change it authors.
fn says_nothing_of_its_copy(peer: &StatePeer, holder: &DeviceCert, now: SystemTime) {
    let held = crate::HeldCopy {
        catching_up_from: [[7; 32]].into_iter().collect(),
        ..Default::default()
    };
    assert!(
        peer.clone()
            .workspace_mut()
            .report_copy(holder, &held, now)
            .is_err(),
        "a holder that is no member says nothing of its copy in the workspace"
    );
    let heads = peer.workspace_heads().unwrap();
    let mut doc = peer.admitted_workspace().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(holder.device_public_key.to_vec()));
    let rows = match doc.get(ROOT, crate::HELD_COPIES_KEY).unwrap() {
        Some((_, rows)) => rows,
        None => doc
            .put_object(ROOT, crate::HELD_COPIES_KEY, automerge::ObjType::Map)
            .unwrap(),
    };
    doc.put(&rows, hex::encode(holder.device_public_key).as_str(), "{}")
        .unwrap();
    doc.commit();
    let copy_row = doc
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    assert!(from_author(&mut peer.clone(), holder.device_public_key, copy_row, now).is_err());
}

/// An entry the workspace never recorded admits nobody, whatever a device
/// signs: the receipt's digest must be the entry the workspace holds.
#[test]
fn an_admission_naming_an_entry_the_workspace_never_had_admits_nobody() {
    use automerge::transaction::Transactable as _;

    let (mut peer, authority_key, _, _, now) = seeded();
    let entry = opened(&mut peer, &authority_key, now);
    let forged = [0x77; 32];
    let (guest_key, guest) = crate::fixture::device(70, 71);
    let workspace = peer.workspace_id().to_owned();
    let now_ns = crate::fixture::ns(now);
    let op = crate::members::SignedMembershipOp::signed(
        &guest,
        &guest_key,
        Vec::new(),
        now_ns,
        crate::members::MembershipOpCommand::Enter {
            entry: forged,
            person: guest.user_public_key,
        },
    )
    .unwrap();
    let mut row = entry_row(&workspace, (&guest_key, &guest), entry);
    row.entry = Some(crate::state::EntryReceipt {
        entry: forged,
        role: Role::Commenter,
        request_id: [4; 32],
    });
    let changes = sender_changes(&peer, |doc| {
        crate::members::put_op(doc, &op).unwrap();
        let identities = doc
            .get(automerge::ROOT, "peer_identities_v1")
            .unwrap()
            .unwrap()
            .1;
        doc.put(
            &identities,
            hex::encode(guest.device_public_key).as_str(),
            automerge::ScalarValue::Bytes(row.encode().unwrap()),
        )
        .unwrap();
    });
    let refused = peer
        .admit_own(&guest, changes, now)
        .expect_err("an entry the workspace never recorded admits nobody");
    assert!(
        refused.contains("does not take this person in"),
        "{refused}"
    );
}

/// Entry admissions replay by the entry their own place in the log held, and
/// the entry is read once for the operation that settled it: many guests cost
/// their operations, not each a reading of the whole document.
#[test]
fn many_entry_admissions_replay_by_the_entry_their_place_held() {
    use crate::members::{MembershipOpCommand, SignedMembershipOp, put_op};

    const GUESTS: usize = 500;
    let (mut peer, authority_key, _, _, now) = seeded();
    let entry = tilo_action_policy::EntryPolicy {
        listing: Some(tilo_action_policy::ListingPolicy {
            description: "Everyone".into(),
            default_role: Role::Commenter,
        }),
        ..Default::default()
    };
    assert!(
        peer.governance_mut()
            .put_workspace_entry(&entry, &authority_key, now)
            .unwrap()
    );
    let workspace = peer.workspace_id().to_owned();
    let mut doc = peer.admitted_workspace().unwrap().clone();
    let digest = crate::state::entry_digest(&entry).unwrap();
    // The entry the owner settled last is the one it just wrote.
    let entry_op = crate::members::ops(&doc)
        .into_iter()
        .filter(|(_, op)| {
            matches!(&op.command, MembershipOpCommand::Governance { rows }
                if rows.contains(&crate::workspace::GovernanceRow::Settings))
        })
        .max_by_key(|(_, op)| op.at_ns)
        .map(|(id, _)| id)
        .expect("the entry is an operation the workspace holds");
    let now_ns = crate::fixture::ns(now);
    for index in 0..GUESTS {
        let mut seed = [0u8; 32];
        seed[..8].copy_from_slice(&(index as u64 + 1).to_le_bytes());
        let root = SigningKey::from_bytes(&seed);
        let mut device_seed = [0x5a; 32];
        device_seed[..8].copy_from_slice(&(index as u64 + 1).to_le_bytes());
        let key = SigningKey::from_bytes(&device_seed);
        let cert = crate::fixture::cert(
            &root,
            &key,
            tilo_user_identity::DeviceCapabilities::member(),
        );
        let op = SignedMembershipOp::signed(
            &cert,
            &key,
            vec![entry_op],
            now_ns.saturating_add(index as u64),
            MembershipOpCommand::Enter {
                entry: digest,
                person: cert.user_public_key,
            },
        )
        .unwrap();
        put_op(&mut doc, &op).unwrap();
    }
    doc.commit();
    let started = std::time::Instant::now();
    let policy = crate::members::replay(&doc, &workspace);
    let elapsed = started.elapsed();
    assert!(
        policy.members.len() >= GUESTS,
        "every guest came in at the entry's role: {} members",
        policy.members.len()
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "replaying {GUESTS} entry admissions took {elapsed:?}"
    );
}

/// The open entry of `peer` (its owner's copy), and the entry's digest.
fn opened(peer: &mut StatePeer, authority: &SigningKey, now: SystemTime) -> [u8; 32] {
    let entry = tilo_action_policy::EntryPolicy {
        listing: Some(tilo_action_policy::ListingPolicy {
            description: "Everyone".into(),
            default_role: Role::Commenter,
        }),
        ..Default::default()
    };
    assert!(
        peer.governance_mut()
            .put_workspace_entry(&entry, authority, now)
            .unwrap()
    );
    crate::state::entry_digest(&entry).unwrap()
}

/// The changes `peer` would carry from `author`'s `write`, as a holder relays
/// them: what a receiver takes in on its own.
fn sender_changes(
    peer: &StatePeer,
    write: impl FnOnce(&mut automerge::AutoCommit),
) -> Vec<Vec<u8>> {
    let mut doc = peer.admitted_workspace().unwrap().clone();
    let heads = doc.get_heads();
    write(&mut doc);
    doc.commit();
    doc.get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// A device's identity row that came in under `entry`, as its own device
/// wrote it.
fn entry_row(
    workspace: &str,
    (key, cert): (&SigningKey, &DeviceCert),
    entry: [u8; 32],
) -> DeviceRow {
    let _ = (key, workspace);
    DeviceRow {
        device_cert: cert.clone(),
        revocations: Vec::new(),
        admission: None,
        admissions: Vec::new(),
        delegate: None,
        workspace_delegations: Vec::new(),
        removed: false,
        entry: Some(crate::state::EntryReceipt {
            entry,
            role: Role::Commenter,
            request_id: [4; 32],
        }),
    }
}

/// A server that keeps the workspace signs no guest in: the admission's
/// author is the guest's own device, and one any other key signs admits
/// nobody.
#[test]
fn a_server_signing_a_guests_admission_admits_nobody() {
    use crate::members::{MembershipOpCommand, SignedMembershipOp, put_op};
    use automerge::transaction::Transactable as _;

    let (mut peer, authority_key, _, _, now) = seeded();
    let entry = opened(&mut peer, &authority_key, now);
    let (holder_key, holder) = crate::fixture::device(91, 92);
    let (guest_key, guest) = crate::fixture::device(93, 94);
    let workspace = peer.workspace_id().to_owned();
    let now_ns = crate::fixture::ns(now);
    let op = SignedMembershipOp::signed(
        &holder,
        &holder_key,
        Vec::new(),
        now_ns,
        MembershipOpCommand::Enter {
            entry,
            person: guest.user_public_key,
        },
    )
    .unwrap();
    let row = entry_row(&workspace, (&guest_key, &guest), entry);
    let changes = sender_changes(&peer, |doc| {
        put_op(doc, &op).unwrap();
        let identities = doc
            .get(automerge::ROOT, "peer_identities_v1")
            .unwrap()
            .unwrap()
            .1;
        doc.put(
            &identities,
            hex::encode(guest.device_public_key).as_str(),
            automerge::ScalarValue::Bytes(row.encode().unwrap()),
        )
        .unwrap();
    });
    let refused = peer
        .admit_own(&guest, changes, now)
        .expect_err("a server's key admits no guest");
    assert!(refused.contains("write forbidden"), "{refused}");
}

/// The entry the owner listed is the most its admissions hand out: a receipt
/// naming a role above it admits nobody.
#[test]
fn a_receipt_naming_a_role_above_the_entry_admits_nobody() {
    use automerge::transaction::Transactable as _;

    let (mut peer, authority_key, _, _, now) = seeded();
    let entry = opened(&mut peer, &authority_key, now);
    let (guest_key, guest) = crate::fixture::device(98, 99);
    let workspace = peer.workspace_id().to_owned();
    let now_ns = crate::fixture::ns(now);
    let op = crate::members::SignedMembershipOp::signed(
        &guest,
        &guest_key,
        Vec::new(),
        now_ns,
        crate::members::MembershipOpCommand::Enter {
            entry,
            person: guest.user_public_key,
        },
    )
    .unwrap();
    let mut row = entry_row(&workspace, (&guest_key, &guest), entry);
    row.entry = Some(crate::state::EntryReceipt {
        entry,
        role: Role::Editor,
        request_id: [4; 32],
    });
    let changes = sender_changes(&peer, |doc| {
        crate::members::put_op(doc, &op).unwrap();
        let identities = doc
            .get(automerge::ROOT, "peer_identities_v1")
            .unwrap()
            .unwrap()
            .1;
        doc.put(
            &identities,
            hex::encode(guest.device_public_key).as_str(),
            automerge::ScalarValue::Bytes(row.encode().unwrap()),
        )
        .unwrap();
    });
    let refused = peer
        .admit_own(&guest, changes, now)
        .expect_err("a role above the entry's admits nobody");
    assert!(
        refused.contains("does not take this person in"),
        "{refused}"
    );
}

/// A visitor who has not named themselves yet comes in through an open entry:
/// a blank name is none given, as through an invitation, and the device
/// still signs its own admission.
#[test]
fn a_visitor_with_no_name_yet_comes_in_through_the_open_entry() {
    let (mut peer, authority_key, _, _, now) = seeded();
    opened(&mut peer, &authority_key, now);
    let (guest_key, guest) = crate::fixture::device(95, 96);
    for blank in [Some(""), Some("  "), None] {
        crate::own_admission::entry_admission_turn(
            peer.workspace_id(),
            (&governance_of(&peer), &[]),
            &guest,
            [7; 32],
            blank,
            &guest_key,
            now,
        )
        .unwrap_or_else(|error| panic!("a visitor named {blank:?} was refused: {error}"));
    }
}

/// A device of the workspace's owner redeems an invitation and, before the host took that in,
/// proves its membership as one of the owner's devices, or the other way round: two rows of its
/// own racing for one place. Each is signed on top of what the device signed before, so the
/// second builds on the first rather than beside it: in either order, every copy takes both in,
/// which it does only where the device's row holds its redemption.
#[test]
fn a_devices_redemption_and_its_further_row_race_in_either_order() {
    let (peer, owner, _, _, now) = seeded();
    let (phone_key, phone) = crate::fixture::device(1, 9);
    let redemption = invited(&peer, &owner, &phone, now);
    let governance = governance_of(&peer);
    let workspace = peer.workspace_id().to_owned();
    let redeemed = |earlier: &[Vec<u8>]| {
        crate::own_admission::own_admission_turn(
            &workspace,
            (&governance, earlier),
            &redemption,
            None,
            &phone_key,
            now,
        )
        .unwrap()
    };
    let further = |earlier: &[Vec<u8>]| {
        crate::own_admission::further_device_row_turn(
            &workspace,
            (&governance, earlier),
            &phone,
            &phone_key,
            now,
        )
        .unwrap()
    };
    // Signed beside each other, on the governance alone, the second to arrive is refused: the
    // device gave two changes one number.
    let (beside, other) = (redeemed(&[]), further(&[]));
    for (first, second) in [(&beside, &other), (&other, &beside)] {
        let mut copy = peer.clone();
        from_author(&mut copy, phone.device_public_key, first.clone(), now).unwrap();
        assert!(from_author(&mut copy, phone.device_public_key, second.clone(), now).is_err());
    }

    // The redemption first: the row the membership proof would sign is the redemption's own.
    let first = redeemed(&[]);
    assert_eq!(
        further(&first).into_iter().collect::<BTreeSet<_>>(),
        first.iter().cloned().collect(),
        "a device whose redemption lists it signs no second row"
    );
    let mut copy = peer.clone();
    from_author(&mut copy, phone.device_public_key, first, now).unwrap();
    assert!(copy.role(&phone.device_public_key, now).is_ok());

    // The further row first: the redemption is signed on top of it, and lands after it.
    let row = further(&[]);
    assert!(!row.is_empty());
    let after = redeemed(&row);
    let mut copy = peer.clone();
    from_author(&mut copy, phone.device_public_key, row, now).unwrap();
    from_author(&mut copy, phone.device_public_key, after, now).unwrap();
    assert!(copy.role(&phone.device_public_key, now).is_ok());
}
