//! A key holder that holds only part of what a compaction covers takes the compaction with its
//! keys, and only when the compaction holds every change the key holder holds.
//!
//! A replica without keys refuses a compaction that covers writes it does not hold: nothing
//! tells it from one that leaves writes out. A device that was away, or was ended before it had
//! kept what it was sent, holds only part of what its workspace compacted meanwhile, and those
//! writes are nowhere but inside the compaction from then on. The device decides it once more
//! with its keys.
use super::sealed_compaction_tests as sealing;
use super::sealed_compaction_tests::{
    Author, Fixture, admit, captured, device, envelope_of, fixture, forged, held, mint, open,
    origin, refused_with, text, wrapper, write,
};
use super::*;
use collab_types::SealedCompaction;
use key_group::ContentKeys;

/// A replica that holds three sealed writes of one device, the replica as it stood before any
/// of them, the writes, and their author's copy of the document.
fn three_writes(
    f: &Fixture,
    keys: &ContentKeys,
) -> (StatePeer, StatePeer, Vec<NeighbourMsg>, Author) {
    let before = f.replica.clone();
    let mut author = Author::new(&before);
    let mut holder = before.clone();
    let writes = ["1", "2", "3"]
        .iter()
        .map(|value| {
            let message = write(&holder, &f.other, keys, &author.edit("value", value), f.now);
            admit(&mut holder, &f.other, message.clone(), f.now);
            message
        })
        .collect();
    (holder, before, writes, author)
}

/// `peer`, a key holder, decides `compaction` once more with its keys.
fn take_behind(
    peer: &mut StatePeer,
    keys: &ContentKeys,
    compaction: &NeighbourMsg,
    now: SystemTime,
) -> Result<Option<SignedOp>, String> {
    peer.publication_mut().take_compaction_behind(
        (sealing::APP, sealing::RECORD),
        envelope_of(compaction),
        &mut |op| open(keys, op),
        now,
    )
}

tilo_test_env::scenario! {
    fn a_key_holder_behind_a_compaction_takes_it_only_when_it_holds_all_the_key_holder_holds;

    phase a_replica_that_kept_less_than_a_compaction_covers_takes_it_with_its_keys;
    {
        // One device holds every write and folds them; another was ended when it had kept
        // only the first.
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let (mut holder, mut behind, writes, mut author) = three_writes(&f, &keys);
        admit(&mut behind, &f.other, writes[0].clone(), now);
        let fold = mint(&mut holder, &keys, &f.owner, now).unwrap();
        // Without the keys it stays refused: nothing tells it from one that left writes out.
        refused_with(&mut behind, &f.owner, fold.clone(), now, "has not admitted");
        assert_eq!(
            take_behind(&mut behind, &keys, &fold, now).unwrap(),
            Some(envelope_of(&fold))
        );
        assert_eq!(
            held(&behind),
            held(&holder),
            "its history is the one a replica that took the compaction first holds"
        );
        // What is written next lands, and the whole of it reads.
        let next = write(&holder, &f.other, &keys, &author.edit("value", "4"), now);
        admit(&mut holder, &f.other, next.clone(), now);
        admit(&mut behind, &f.other, next, now);
        assert_eq!(
            text(&captured(&f, &behind, &keys).unwrap(), "value").as_deref(),
            Some("4")
        );
        let kept = behind.durable().unwrap();
        assert_eq!(held(&f.restore(&kept).unwrap()), held(&behind));
    }

    phase a_replica_that_went_on_without_the_compaction_takes_it_when_it_comes_again;
    {
        // A device that refused the compaction took the writes after it all the same, and read
        // none of them: each builds on changes that are only inside the compaction. That is how
        // a device stands that stopped hearing a conversation. Its workspace sends the
        // compaction again when the device next starts.
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let (mut holder, mut behind, writes, mut author) = three_writes(&f, &keys);
        admit(&mut behind, &f.other, writes[0].clone(), now);
        let fold = mint(&mut holder, &keys, &f.owner, now).unwrap();
        refused_with(&mut behind, &f.owner, fold.clone(), now, "has not admitted");
        let next = write(&holder, &f.other, &keys, &author.edit("value", "4"), now);
        admit(&mut holder, &f.other, next.clone(), now);
        let _ = behind.receive_from_report(origin(&f.other), next.clone(), now);
        let reads = |peer: &StatePeer| {
            captured(&f, peer, &keys)
                .ok()
                .and_then(|document| text(&document, "value"))
        };
        assert_ne!(
            reads(&behind).as_deref(),
            Some("4"),
            "what was written after the compaction does not read without it"
        );
        take_behind(&mut behind, &keys, &fold, now).unwrap();
        // The write after it is sent again with it, and is the same write.
        let _ = behind.receive_from_report(origin(&f.other), next, now);
        assert_eq!(held(&behind), held(&holder));
        assert_eq!(reads(&behind).as_deref(), Some("4"));
    }

    phase a_compaction_that_lacks_a_change_the_replica_holds_is_refused_with_keys_too;
    {
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let (holder, mut behind, writes, _) = three_writes(&f, &keys);
        let unwritten = behind.clone();
        admit(&mut behind, &f.other, writes[0].clone(), now);
        admit(&mut behind, &f.other, writes[1].clone(), now);
        // It names all three writes as covered and holds none of them: its snapshot is the
        // document as it stood before any of them. Taken, the two writes this replica holds
        // would be gone from it.
        let forgery = forged(
            &unwritten,
            &f.owner,
            &wrapper(&unwritten, &keys, &held(&holder)),
            now,
        );
        let before = behind.durable().unwrap();
        let refusal = take_behind(&mut behind, &keys, &forgery, now).unwrap_err();
        assert!(refusal.contains("lacks changes"), "{refusal}");
        assert_eq!(behind.durable().unwrap(), before, "the replica is unchanged");
        // Nor without keys.
        refused_with(&mut behind, &f.owner, forgery, now, "has not admitted");
    }

    phase a_compaction_signed_by_a_device_the_workspace_does_not_admit_is_refused_with_keys_too;
    {
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let (mut holder, mut behind, writes, _) = three_writes(&f, &keys);
        admit(&mut behind, &f.other, writes[0].clone(), now);
        // The compaction a member made, signed again by a device of nobody the workspace
        // knows: what it holds is whole, and whose it is decides.
        let genuine = envelope_of(&mint(&mut holder, &keys, &f.owner, now).unwrap());
        let (stranger, _) = device(120, 121);
        let forgery = forged(
            &holder,
            &stranger,
            &(
                SealedCompaction::parse(&genuine).unwrap(),
                genuine.encryption.unwrap(),
            ),
            now,
        );
        let before = behind.durable().unwrap();
        assert!(
            take_behind(&mut behind, &keys, &forgery, now).is_err(),
            "a compaction nobody of the workspace signed was taken"
        );
        assert_eq!(behind.durable().unwrap(), before, "the replica is unchanged");
    }

    phase a_removed_device_hands_a_device_behind_it_its_history_and_nothing_it_signs_after;
    {
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let phone = f.other.clone();
        let (_, mut governor, _, _) = three_writes(&f, &keys);
        let mut author = Author::new(&governor);
        let first = write(&governor, &phone, &keys, &author.edit("first", "written"), now);
        admit(&mut governor, &phone, first, now);
        // A device that was ended holds the phone's first write and not its second.
        let mut behind = governor.clone();
        let second = write(&governor, &phone, &keys, &author.edit("second", "written"), now);
        admit(&mut governor, &phone, second, now);
        let whole = governor.clone();
        // The phone folds both into a compaction the workspace takes, and is removed: the
        // removal's cut keeps that compaction as the phone's history.
        let taken = mint(&mut governor, &keys, &phone, now).unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        let mut removal = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: phone.verifying_key().to_bytes(),
            revoked_at_ns: now_ns(now),
            reason: tilo_user_identity::RevocationReason::Stolen,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut removal, &root).unwrap();
        assert!(
            governor
                .governance_mut()
                .publish_governed_revocations(std::slice::from_ref(&removal), &f.owner, now)
                .unwrap()
        );
        behind
            .receive_from(origin(&f.owner), governor.workspace_message(), now)
            .unwrap();
        // Removed, the phone signs another compaction of the same two writes, under the
        // number it used: a device behind it takes nothing of it, with its keys or without.
        let forgery = forged(&whole, &phone, &wrapper(&whole, &keys, &held(&whole)), now);
        let before = behind.durable().unwrap();
        let refusal = take_behind(&mut behind, &keys, &forgery, now).unwrap_err();
        assert!(refusal.contains("revoked"), "{refusal}");
        assert_eq!(behind.durable().unwrap(), before, "the replica is unchanged");
        // The one the workspace had taken is history: the device behind it takes it.
        take_behind(&mut behind, &keys, &taken, now).unwrap();
        assert_eq!(held(&behind), held(&governor));
        assert_eq!(
            text(&captured(&f, &behind, &keys).unwrap(), "second").as_deref(),
            Some("written")
        );
    }
}
