//! `has_change` answers whether a document holds a change as `get_change_by_hash` being `Some`
//! does, from the change graph alone (vendor/automerge/TILO-PATCH.md): a reader asking whether
//! its heads are still in a large document does not rebuild the change to find out.

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ChangeHash, ROOT, ReadDoc};
use tilo_automerge::ApplyIncoming;

fn edit(doc: &mut AutoCommit, key: &str, value: i64) -> ChangeHash {
    doc.put(ROOT, key, value).unwrap();
    doc.commit().unwrap()
}

fn agrees(doc: &mut AutoCommit, hash: ChangeHash) -> bool {
    let held = doc.has_change(&hash);
    assert_eq!(held, doc.get_change_by_hash(&hash).is_some());
    assert_eq!(held, ReadDoc::has_change(doc.committed_document(), &hash));
    held
}

#[test]
fn has_change_answers_as_get_change_by_hash_does() {
    let mut ours = AutoCommit::new().with_actor(ActorId::from(b"ours".as_slice()));
    let first = edit(&mut ours, "a", 1);
    let mut theirs = ours.fork().with_actor(ActorId::from(b"theirs".as_slice()));
    let second = edit(&mut theirs, "b", 2);
    let third = edit(&mut theirs, "c", 3);

    assert!(agrees(&mut ours, first));
    assert!(!agrees(&mut ours, second));

    // A change waiting for one it depends on is not held yet.
    let waiting = theirs.get_change_by_hash(&third).unwrap();
    ours.apply_incoming(vec![waiting]).unwrap();
    assert!(!agrees(&mut ours, third));

    let arrived = theirs.get_change_by_hash(&second).unwrap();
    ours.apply_incoming(vec![arrived]).unwrap();
    assert!(agrees(&mut ours, second));
    assert!(agrees(&mut ours, third));

    let mut loaded = AutoCommit::load(&ours.save()).unwrap();
    for hash in [first, second, third] {
        assert!(agrees(&mut loaded, hash));
    }
    assert!(!agrees(&mut loaded, ChangeHash([7; 32])));
}
