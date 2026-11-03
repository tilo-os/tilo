//! A pair record is its two people's: their devices read and write it, and
//! nobody else's do — the workspace's owner and its other members included.
use super::*;
use crate::admission_tests::{add_member_with, seeded};
use automerge::{AutoCommit, ROOT as DOC_ROOT, transaction::Transactable};
use collab_types::CommitNamed as _;
use collab_types::{ChannelRoute, direct_pair_id, direct_record};

const APP: &str = "test.app";

struct Paired {
    peer: StatePeer,
    authority: SigningKey,
    owner_device: DeviceCert,
    now: SystemTime,
    first: (SigningKey, DeviceCert, [u8; 32]),
    second: (SigningKey, DeviceCert, [u8; 32]),
    second_root: SigningKey,
    record: String,
}

impl Paired {
    fn person(&self, which: usize) -> [u8; 32] {
        if which == 0 {
            self.first.2
        } else {
            self.second.2
        }
    }
    fn route(&self) -> ChannelRoute {
        ChannelRoute::new(APP, self.record.as_str())
    }
}

/// `seeded`, with two more people in it, a pair announced by the first between
/// the two, and the pair's record id.
fn paired_workspace() -> Paired {
    let (mut peer, authority, _, owner_device, now) = seeded();
    let first = add_member_with(&mut peer, &authority, now, 5, 6);
    let second_root = SigningKey::from_bytes(&[7; 32]);
    let second = add_member_with(&mut peer, &authority, now, 7, 8);
    let pair = direct_pair_id(
        peer.workspace_id(),
        &hex::encode(first.2),
        &hex::encode(second.2),
    )
    .expect("two distinct people name a pair");
    assert!(
        peer.governance_mut()
            .author_direct_pair(&first.1, &second.2, &first.0, now)
            .unwrap()
    );
    let record = direct_record(&pair);
    // The workspace lets its people author records of this app, so creating a
    // record is decided by the pair rule, not by app authorship.
    let grant = tilo_action_policy::roles::app_authorship(
        peer.workspace_id(),
        APP,
        tilo_action_policy::Subject::Person(hex::encode(authority.verifying_key().to_bytes())),
    );
    peer.governance_mut()
        .put_governed_grants(&[grant], &authority, now)
        .unwrap();
    Paired {
        peer,
        authority,
        owner_device,
        now,
        first,
        second,
        second_root,
        record,
    }
}

#[test]
fn a_pair_record_is_read_and_written_by_its_two_people_alone() {
    let mut paired = paired_workspace();
    let (now, record) = (paired.now, paired.record.clone());
    for member in [&paired.first.1, &paired.second.1] {
        paired
            .peer
            .check_document_read(member.device_public_key, APP, &record, now)
            .expect("a pair's person reads its record");
    }
    assert!(
        paired
            .peer
            .check_document_read(paired.owner_device.device_public_key, APP, &record, now)
            .is_err(),
        "the workspace's owner reads nothing of a pair it is not in"
    );
    // A member's first write to a record it creates is the empty one, then
    // changes on top of it.
    let empty = AutoCommit::new().save();
    paired
        .peer
        .publication_mut()
        .publish_document(APP, &record, empty, &paired.first.0, now)
        .expect("a pair's person creates its record");
    let mut doc = AutoCommit::load(&paired.peer.document(APP, &record).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        paired.second.0.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(DOC_ROOT, "text", "hello").unwrap();
    doc.commit_edit("fixture", "text");
    paired
        .peer
        .publication_mut()
        .publish_incremental(APP, &record, doc.save_after(&heads), &paired.second.0, now)
        .expect("a pair's other person writes to it");
    assert!(paired.peer.document(APP, &record).is_some());
}

#[test]
fn a_third_person_reads_no_pair_record_and_writes_none() {
    let mut paired = paired_workspace();
    let (now, record) = (paired.now, paired.record.clone());
    let (third_key, third, _) = add_member_with(&mut paired.peer, &paired.authority, now, 9, 10);
    assert!(
        paired
            .peer
            .check_document_read(third.device_public_key, APP, &record, now)
            .is_err()
    );
    let empty = AutoCommit::new().save();
    paired
        .peer
        .publication_mut()
        .publish_document(APP, "tab:9", empty, &third_key, now)
        .expect("a third person creates its own app record");
    let empty = AutoCommit::new().save();
    assert!(
        paired
            .peer
            .publication_mut()
            .publish_document(APP, &record, empty, &third_key, now)
            .is_err(),
        "a third person creates no pair record"
    );
    let empty = AutoCommit::new().save();
    paired
        .peer
        .publication_mut()
        .publish_document(APP, &record, empty, &paired.first.0, now)
        .unwrap();
    let mut doc = AutoCommit::load(&paired.peer.document(APP, &record).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        third_key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(DOC_ROOT, "text", "intruder").unwrap();
    doc.commit_edit("fixture", "text");
    let change = doc.save_after(&heads);
    assert!(
        paired
            .peer
            .publication_mut()
            .publish_incremental(APP, &record, change.clone(), &third_key, now)
            .is_err(),
        "a third person writes nothing into a pair's record"
    );
    assert!(
        paired
            .peer
            .publication_mut()
            .publish_incremental(APP, &record, change, &paired.authority, now)
            .is_err(),
        "the workspace's owner writes nothing into a pair's record either"
    );
}

#[test]
fn only_a_pair_records_readers_hold_its_channel() {
    let mut paired = paired_workspace();
    let (now, record) = (paired.now, paired.record.clone());
    let route = paired.route();
    let channel = route.channel(paired.peer.workspace_id());
    let readers = paired
        .peer
        .governance()
        .channel_readers(now, channel, Some(&route))
        .unwrap()
        .expect("a pair record's channel has readers");
    assert_eq!(
        readers,
        [
            paired.first.1.device_public_key,
            paired.second.1.device_public_key
        ]
        .into_iter()
        .collect(),
        "the pair's two devices hold the channel, no one else"
    );
    assert!(!readers.contains(&paired.owner_device.device_public_key));
    // A device of one of the two people, admitted later, holds it too.
    let root = SigningKey::from_bytes(&paired.second_root.to_bytes());
    let later_key = SigningKey::from_bytes(&[9; 32]);
    let mut later = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: later_key.verifying_key().to_bytes(),
        device_label: "later".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut later, &root).unwrap();
    assert_eq!(later.user_public_key, paired.person(1));
    assert!(
        paired
            .peer
            .governance_mut()
            .admit_member_device(&later, &paired.authority, now)
            .unwrap()
    );
    paired
        .peer
        .check_document_read(later.device_public_key, APP, &record, now)
        .expect("a device of a pair's person reads the pair's record");
    let readers = paired
        .peer
        .governance()
        .channel_readers(now, channel, Some(&route))
        .unwrap()
        .unwrap();
    assert_eq!(
        readers,
        [
            paired.first.1.device_public_key,
            paired.second.1.device_public_key,
            later.device_public_key
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn the_workspace_keeps_syncing_and_reading_every_other_record() {
    let paired = paired_workspace();
    let (now, record) = (paired.now, paired.record.clone());
    assert!(
        paired
            .peer
            .check_document_read(paired.owner_device.device_public_key, APP, "tab:1", now)
            .is_ok(),
        "the owner keeps reading records of its workspace"
    );
    for member in [&paired.first.1, &paired.second.1] {
        assert!(
            paired
                .peer
                .check_document_read(member.device_public_key, APP, "tab:1", now)
                .is_ok(),
            "a pair's people keep reading their workspace's records"
        );
    }
    let ordinary = ChannelRoute::new(APP, "tab:1");
    let readers = paired
        .peer
        .governance()
        .channel_readers(
            now,
            ordinary.channel(paired.peer.workspace_id()),
            Some(&ordinary),
        )
        .unwrap()
        .unwrap();
    assert!(
        readers.contains(&paired.owner_device.device_public_key),
        "a record no grant and no pair covers has every member device as a reader"
    );
    let pairs = paired.peer.governance().direct_pairs(now);
    assert_eq!(pairs.len(), 1);
    let (pair, people) = pairs.into_iter().next().unwrap();
    assert_eq!(direct_record(&pair), record);
    let mut expected = [hex::encode(paired.person(0)), hex::encode(paired.person(1))];
    expected.sort_unstable();
    assert_eq!(people, expected);
}

#[test]
fn a_pair_is_announced_once_by_a_device_in_it() {
    let mut paired = paired_workspace();
    let (now, record) = (paired.now, paired.record.clone());
    assert!(
        !paired
            .peer
            .governance_mut()
            .author_direct_pair(&paired.second.1, &paired.first.2, &paired.second.0, now)
            .unwrap(),
        "the pair the workspace holds stays as it is"
    );
    // A device that is not the caller's signs nothing.
    assert!(
        paired
            .peer
            .governance_mut()
            .author_direct_pair(&paired.first.1, &paired.second.2, &paired.second.0, now)
            .is_err()
    );
    assert!(paired.peer.direct_pair(&record, now).is_some());
    let pair = collab_types::direct_pair(&record).unwrap().to_owned();
    let mut expected = [hex::encode(paired.person(0)), hex::encode(paired.person(1))];
    expected.sort_unstable();
    assert_eq!(paired.peer.governance().direct_pairs(now)[&pair], expected);
}
