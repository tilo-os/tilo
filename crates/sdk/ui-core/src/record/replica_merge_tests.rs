//! What a replica of an open record keeps when the room delivers it
//! changes out of order or the host's empty document for a new record.

use super::*;
use durable_store::MemoryStore;

fn put_text(doc: &mut AutoCommit, key: &str, value: &str) {
    let _ = doc.put(&ROOT, key, value);
}

fn text(doc: &AutoCommit, key: &str) -> Option<String> {
    str_at(doc, &ROOT, key)
}

/// The host takes up a record a member created with an empty document of
/// its own before the member's content: merged into the open record it
/// changes nothing and is no error, and the record stays as it was.
#[test]
fn the_hosts_empty_document_for_a_new_record_merges_as_nothing() {
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open("so.tilo.test", "room-a").unwrap();
    let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    store
        .reserve_record(&want)
        .unwrap()
        .initialize(|doc| {
            put_text(doc, "body", "mine");
            Ok(())
        })
        .unwrap();
    let empty = AutoCommit::new().save();
    let (changed, observed) = store
        .merge_replica(&want, collab_types::DocumentMutation::Snapshot(empty))
        .unwrap();
    assert!(!changed);
    assert!(observed.is_empty());
    assert_eq!(
        store
            .read(&want.id, |doc| text(doc, "body"))
            .unwrap()
            .as_deref(),
        Some("mine")
    );
}

#[test]
fn a_change_that_arrives_before_the_one_it_builds_on_survives_a_snapshot() {
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open("so.tilo.test", "room-a").unwrap();
    let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    store
        .reserve_record(&want)
        .unwrap()
        .initialize(|doc| {
            put_text(doc, "body", "mine");
            Ok(())
        })
        .unwrap();
    // Another member seats a player, then deals on top of that seat.
    let mut member = store.read(&want.id, |doc| doc.clone().fork()).unwrap();
    put_text(&mut member, "seat", "grace");
    member.commit();
    let seated = member.get_heads();
    put_text(&mut member, "deal", "hand 1");
    member.commit();
    let deal = member.save_after(&seated);
    // A third member's snapshot holds the seat and a change of its own.
    let mut third = member.fork_at(&seated).unwrap();
    put_text(&mut third, "chat", "hello");
    third.commit();

    let (changed, _) = store
        .merge_replica(&want, collab_types::DocumentMutation::Incremental(deal))
        .unwrap();
    assert!(!changed, "the deal waits for the seat it builds on");
    store
        .merge_replica(
            &want,
            collab_types::DocumentMutation::Snapshot(third.save()),
        )
        .unwrap();
    let read = |key: &'static str| store.read(&want.id, |doc| text(doc, key)).unwrap();
    assert_eq!(read("seat").as_deref(), Some("grace"));
    assert_eq!(read("chat").as_deref(), Some("hello"));
    assert_eq!(
        read("deal").as_deref(),
        Some("hand 1"),
        "the waiting deal applied"
    );
    assert_eq!(read("body").as_deref(), Some("mine"));
}

/// A window reading the store is woken when another replica's change
/// arrives in it, merged or kept, and not by a delivery that changes
/// nothing; a closed window is not woken.
#[test]
fn a_change_from_another_replica_wakes_the_windows_reading_the_store() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open("so.tilo.test", "room-a").unwrap();
    let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    store
        .reserve_record(&want)
        .unwrap()
        .initialize(|doc| {
            put_text(doc, "body", "mine");
            Ok(())
        })
        .unwrap();
    let mut window = crate::WakeScope::new();
    let woken = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&woken);
    window
        .bind(move |event| {
            if event.requires_sync() {
                count.fetch_add(1, Ordering::SeqCst);
            }
        })
        .unwrap();
    store.wake_on_arrival(window.handle());
    store.wake_on_arrival(window.handle());

    let empty = AutoCommit::new().save();
    store
        .merge_replica(&want, collab_types::DocumentMutation::Snapshot(empty))
        .unwrap();
    assert_eq!(woken.load(Ordering::SeqCst), 0, "nothing arrived");

    let peer_change = |key: &str| {
        let mut peer = store.read(&want.id, |doc| doc.clone().fork()).unwrap();
        let before = peer.get_heads();
        put_text(&mut peer, key, "theirs");
        peer.commit();
        peer.save_after(&before)
    };
    let (changed, _) = store
        .merge_replica(
            &want,
            collab_types::DocumentMutation::Incremental(peer_change("seat")),
        )
        .unwrap();
    assert!(changed);
    assert_eq!(
        woken.load(Ordering::SeqCst),
        1,
        "woken once, though registered twice"
    );

    store.note_kept();
    assert_eq!(
        woken.load(Ordering::SeqCst),
        2,
        "a record kept for the window"
    );

    window.close();
    store
        .merge_replica(
            &want,
            collab_types::DocumentMutation::Incremental(peer_change("deal")),
        )
        .unwrap();
    assert_eq!(
        woken.load(Ordering::SeqCst),
        2,
        "a closed window is not woken"
    );
}

/// The room's hosts saying they sent all they hold of a record wakes the
/// windows reading the store, and holds until something of it arrives.
#[test]
fn the_room_answering_for_a_record_wakes_its_readers_until_it_arrives() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open("so.tilo.test", "room-a").unwrap();
    let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    store
        .reserve_record(&want)
        .unwrap()
        .initialize(|doc| {
            put_text(doc, "body", "mine");
            Ok(())
        })
        .unwrap();
    let mut window = crate::WakeScope::new();
    let woken = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&woken);
    window
        .bind(move |event| {
            if event.requires_sync() {
                count.fetch_add(1, Ordering::SeqCst);
            }
        })
        .unwrap();
    store.wake_on_arrival(window.handle());
    let kept = store.kept_revision();

    assert!(!store.room_answered(&want.id));
    store.note_room_answered(want.id.clone());
    assert!(store.room_answered(&want.id));
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    assert_ne!(
        store.kept_revision(),
        kept,
        "what follows the store looks again"
    );

    let mut peer = store.read(&want.id, |doc| doc.clone().fork()).unwrap();
    let before = peer.get_heads();
    put_text(&mut peer, "body", "theirs");
    peer.commit();
    store
        .merge_replica(
            &want,
            collab_types::DocumentMutation::Incremental(peer.save_after(&before)),
        )
        .unwrap();
    assert!(!store.room_answered(&want.id), "something of it arrived");
}

fn large_text_record() -> (RecordOwners, RecordStore, RecordCheckout, ObjId, String) {
    let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
    let store = owners.open("so.tilo.test", "room-a").unwrap();
    let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    let source = "fn source() { /* café */ }\n".repeat(900);
    store
        .reserve_record(&want)
        .unwrap()
        .initialize(|doc| {
            let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            doc.splice_text(&body, 0, 0, &source).unwrap();
            put_text(doc, "beside", "unchanged");
            Ok(())
        })
        .unwrap();
    let body = store
        .read(&want.id, |doc| doc.get(&ROOT, "body").unwrap().unwrap().1)
        .unwrap();
    (owners, store, want, body, source)
}

#[test]
fn an_ahead_snapshot_keeps_exact_text_history_actor_and_bystander() {
    let (_owners, store, want, body, source) = large_text_record();
    let other = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    store
        .reserve_record(&other)
        .unwrap()
        .initialize(|doc| {
            put_text(doc, "body", "another record");
            Ok(())
        })
        .unwrap();
    let bystander = store.with(&other.id, AutoCommit::save).unwrap();
    let actor = store.read(&want.id, |doc| doc.get_actor().clone()).unwrap();
    let mut peer = store.read(&want.id, |doc| doc.clone().fork()).unwrap();
    let before = peer.get_heads();
    // Metadata-only lookup must test the same existing-change condition as
    // the old full-change lookup, including the large creation change.
    for hash in &before {
        assert_eq!(
            peer.get_change_by_hash(hash).unwrap().hash(),
            peer.get_change_meta_by_hash(hash).unwrap().hash
        );
    }
    peer.splice_text(&body, 0, 0, "// remote\n").unwrap();
    peer.commit();
    let heads = peer.get_heads();
    store
        .with(&want.id, |doc| {
            for hash in &heads {
                assert!(doc.get_change_by_hash(hash).is_none());
                assert!(doc.get_change_meta_by_hash(hash).is_none());
            }
        })
        .unwrap();
    let snapshot = peer.save();
    let (changed, observed) = store
        .merge_replica(
            &want,
            collab_types::DocumentMutation::Snapshot(snapshot.clone()),
        )
        .unwrap();
    assert!(changed);
    assert_eq!(observed, heads);
    store
        .with(&want.id, |doc| {
            assert_eq!(doc.text(&body).unwrap(), format!("// remote\n{source}"));
            assert_eq!(doc.get_heads(), heads);
            assert_eq!(doc.get_actor(), &actor);
            assert_eq!(identity(doc).as_ref(), Some(&want));
            assert_eq!(text(doc, "beside").as_deref(), Some("unchanged"));
            assert_eq!(doc.get_changes_meta(&before).len(), 1);
            assert!(doc.get_missing_deps(&[]).is_empty());
        })
        .unwrap();
    let (changed, observed) = store
        .merge_replica(&want, collab_types::DocumentMutation::Snapshot(snapshot))
        .unwrap();
    assert!(!changed, "an identical snapshot is a no-op");
    assert_eq!(observed, heads);
    assert_eq!(store.with(&other.id, AutoCommit::save).unwrap(), bystander);
}

#[test]
fn a_divergent_snapshot_preserves_both_branches_and_local_actor() {
    let (_owners, store, want, body, source) = large_text_record();
    let actor = store.read(&want.id, |doc| doc.get_actor().clone()).unwrap();
    let mut peer = store.read(&want.id, |doc| doc.clone().fork()).unwrap();
    let before = peer.get_heads();
    peer.splice_text(&body, source.chars().count(), 0, "// remote\n")
        .unwrap();
    peer.commit();
    let remote_heads = peer.get_heads();
    store
        .with(&want.id, |doc| {
            doc.splice_text(&body, 0, 0, "// local\n").unwrap();
            doc.commit();
        })
        .unwrap();
    let local_heads = store.with(&want.id, AutoCommit::get_heads).unwrap();
    let snapshot = peer.save();
    let (changed, observed) = store
        .merge_replica(
            &want,
            collab_types::DocumentMutation::Snapshot(snapshot.clone()),
        )
        .unwrap();
    assert!(changed);
    assert_eq!(observed, remote_heads);
    store
        .with(&want.id, |doc| {
            assert_eq!(
                doc.text(&body).unwrap(),
                format!("// local\n{source}// remote\n")
            );
            assert_eq!(doc.get_actor(), &actor);
            let heads = doc.get_heads();
            assert_eq!(heads.len(), 2);
            assert!(heads.contains(&local_heads[0]));
            assert!(heads.contains(&remote_heads[0]));
            assert_eq!(doc.get_changes_meta(&before).len(), 2);
            assert!(doc.get_missing_deps(&[]).is_empty());
        })
        .unwrap();
    assert!(
        !store
            .merge_replica(&want, collab_types::DocumentMutation::Snapshot(snapshot))
            .unwrap()
            .0
    );
}
