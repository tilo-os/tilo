//! End-to-end text sync over typed RPC + real `CollaborationManager`
//! (the actual send/poll wire path, not the in-memory relay): a late
//! joiner adopts a full snapshot, then an incremental text edit ("typing")
//! propagates. This is the transport+CRDT half of "type in one window, see
//! it in the other" — the App layer drives exactly these calls
//! (`broadcast_doc_delta` -> document incremental; poll -> `load_incremental`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::typed_collab::TypedBroker;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use tilo_ui::collaboration::CollabRequest;

fn text_of(doc: &AutoCommit) -> String {
    let Ok(Some((Value::Object(ObjType::Text), id))) = doc.get(&ROOT, "text") else {
        return String::new();
    };
    doc.text(&id).unwrap_or_default()
}

#[test]
fn live_text_edit_propagates_through_broker() {
    let broker = TypedBroker::new();
    let mut a = broker.manager("alice");
    let mut b = broker.manager("bob");
    let _ = a.poll();
    let _ = b.poll();

    let mut a_doc = AutoCommit::new();
    a_doc.set_actor(automerge::ActorId::from(b"alice".as_slice()));
    let t = a_doc.put_object(&ROOT, "text", ObjType::Text).unwrap();
    a_doc.splice_text(&t, 0, 0, "hello").unwrap();

    // Late-join handshake: alice ships a full snapshot; bob adopts it.
    a.send(CollabRequest::document_snapshot(a_doc.save()));
    let _ = a_doc.save_incremental(); // re-baseline incremental after full save

    let mut b_doc = AutoCommit::new();
    b_doc.set_actor(automerge::ActorId::from(b"bob".as_slice()));
    let mut adopted = false;
    tilo_test_env::wait_until("alice's snapshot at bob", || {
        for e in b.poll() {
            if let Some(Ok(collab_types::DocumentMutation::Snapshot(data))) = e.document_mutation()
            {
                let mut incoming = AutoCommit::load(&data).unwrap();
                b_doc.merge(&mut incoming).unwrap();
                adopted = true;
            }
        }
        adopted
    });
    assert!(adopted, "bob must receive alice's snapshot");
    assert_eq!(text_of(&b_doc), "hello", "bob adopts alice's text");

    // Alice "types": incremental edit -> bob applies it.
    a_doc.splice_text(&t, 5, 0, " world").unwrap();
    let delta = a_doc.save_incremental();
    assert!(!delta.is_empty(), "the edit must produce a delta");
    a.send(CollabRequest::document_incremental(delta));

    let mut got_edit = false;
    tilo_test_env::wait_until("alice's edit at bob", || {
        for e in b.poll() {
            if let Some(Ok(collab_types::DocumentMutation::Incremental(data))) =
                e.document_mutation()
            {
                b_doc.load_incremental(&data).unwrap();
                got_edit = true;
            }
        }
        got_edit && text_of(&b_doc) == "hello world"
    });
    assert_eq!(
        text_of(&b_doc),
        "hello world",
        "a live incremental text edit must propagate to the peer"
    );

    drop(a);
    drop(b);
}
