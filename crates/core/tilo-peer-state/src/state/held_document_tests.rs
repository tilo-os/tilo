//! A sealed document read again over what its last read merged reads as one read of its whole
//! history does, whatever came between: more writes, none, or a compaction of them.
use super::sealed_compaction_tests::{APP, RECORD as RESOURCE, fixture, open, person, sealer};
use super::*;
use collab_types::CommitNamed as _;

/// `peer`'s document with `key` set to `value`, sealed as the key holder's next write.
fn written(
    peer: &mut StatePeer,
    keys: &key_group::ContentKeys,
    signer: &SigningKey,
    (key, value): (&str, &str),
    now: SystemTime,
) {
    let opened = peer
        .publication()
        .opened_document(APP, RESOURCE, &mut |op| open(keys, op))
        .unwrap()
        .map(|opened| opened.document);
    let bytes = opened.or_else(|| peer.document(APP, RESOURCE)).unwrap();
    let mut document = AutoCommit::load(&bytes).unwrap();
    let heads = document.get_heads();
    document.put(automerge::ROOT, key, value).unwrap();
    document.commit_edit("fixture", key, &person(peer, signer));
    peer.publication_mut()
        .publish_sealed_incremental(
            APP,
            RESOURCE,
            document.save_after(&heads),
            &mut sealer(keys, RESOURCE),
            signer,
            now,
        )
        .unwrap();
}

/// What the document reads as from scratch, and over `prior`, and how many writes the read over
/// `prior` opened.
fn read_counted(
    peer: &StatePeer,
    keys: &key_group::ContentKeys,
    prior: Option<MergedDocument>,
) -> (Vec<u8>, Vec<u8>, Option<MergedDocument>, usize) {
    let whole = peer
        .publication()
        .opened_document(APP, RESOURCE, &mut |op| open(keys, op))
        .unwrap()
        .unwrap();
    let held = peer.publication().held_document(APP, RESOURCE).unwrap();
    let mut opened: usize = 0;
    let (again, next) = held
        .opened_over(
            &mut |op| {
                opened = opened.saturating_add(1);
                open(keys, op)
            },
            prior,
        )
        .unwrap();
    assert!(whole.unmerged.is_empty() && again.unmerged.is_empty());
    (whole.document, again.document, next, opened)
}

/// [`read_counted`] without the count.
fn read(
    peer: &StatePeer,
    keys: &key_group::ContentKeys,
    prior: Option<MergedDocument>,
) -> (Vec<u8>, Vec<u8>, Option<MergedDocument>) {
    let (whole, again, next, _) = read_counted(peer, keys, prior);
    (whole, again, next)
}

fn heads(bytes: &[u8]) -> Vec<automerge::ChangeHash> {
    AutoCommit::load(bytes).unwrap().get_heads()
}

#[test]
fn a_document_read_over_its_last_read_reads_as_its_whole_history_does() {
    let f = fixture();
    let keys = f.keys();
    let mut peer = f.replica.clone();
    for at in 0..3 {
        written(
            &mut peer,
            &keys,
            &f.owner,
            (&format!("first {at}"), "a"),
            f.now,
        );
    }
    let (whole, first, prior) = read(&peer, &keys, None);
    assert_eq!(heads(&first), heads(&whole));
    let prior = prior.expect("a read merging every write is kept for the next");

    // Nothing written since: the last read answers.
    let (whole, again, prior, opened) = read_counted(&peer, &keys, Some(prior));
    assert_eq!(opened, 0, "nothing written since is opened again");
    assert_eq!(again, first);
    assert_eq!(heads(&again), heads(&whole));
    assert!(
        prior.as_ref().is_some_and(MergedDocument::holds_merged),
        "a read that found nothing new keeps what the next write merges into"
    );

    // Writes since: only they are merged, and the document reads whole.
    for at in 0..2 {
        written(
            &mut peer,
            &keys,
            &f.owner,
            (&format!("then {at}"), "b"),
            f.now,
        );
    }
    let (whole, later, prior, opened) = read_counted(&peer, &keys, prior);
    assert_eq!(opened, 2, "only the writes since are opened");
    assert_eq!(heads(&later), heads(&whole));
    assert_ne!(heads(&later), heads(&first));

    // A read that kept only what it read opens the history again once it grows.
    let mut light = prior.unwrap();
    light.lighten();
    written(&mut peer, &keys, &f.owner, ("last", "c"), f.now);
    let (whole, last, _) = read(&peer, &keys, Some(light));
    assert_eq!(heads(&last), heads(&whole));
    let document = AutoCommit::load(&last).unwrap();
    for key in ["first 0", "first 2", "then 1", "last"] {
        assert!(
            document.get(automerge::ROOT, key).unwrap().is_some(),
            "{key}"
        );
    }
}

#[test]
fn a_read_of_another_document_state_is_not_continued() {
    let f = fixture();
    let keys = f.keys();
    let mut peer = f.replica.clone();
    let mut other = peer.clone();
    written(&mut peer, &keys, &f.owner, ("one", "a"), f.now);
    let (_, _, prior) = read(&peer, &keys, None);

    // Another replica's document, written differently: its history does not continue this one.
    written(&mut other, &keys, &f.other, ("two", "b"), f.now);
    written(&mut other, &keys, &f.other, ("three", "c"), f.now);
    let (whole, read_over, _) = read(&other, &keys, prior);
    assert_eq!(heads(&read_over), heads(&whole));
    let document = AutoCommit::load(&read_over).unwrap();
    assert!(document.get(automerge::ROOT, "one").unwrap().is_none());
}
