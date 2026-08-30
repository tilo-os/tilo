//! End-to-end text sync over the real broker + real `CollaborationManager`
//! (the actual send/poll wire path, not the in-memory relay): a late
//! joiner adopts a full snapshot, then an incremental text edit ("typing")
//! propagates. This is the transport+CRDT half of "type in one window, see
//! it in the other" — the App layer drives exactly these calls
//! (`broadcast_doc_delta` -> `SyncIncremental`; poll -> `load_incremental`).

use std::os::unix::net::UnixListener;
use std::thread;
use std::time::Duration;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};

fn text_of(doc: &AutoCommit) -> String {
    let Ok(Some((Value::Object(ObjType::Text), id))) = doc.get(&ROOT, "text") else {
        return String::new();
    };
    doc.text(&id).unwrap_or_default()
}

#[test]
#[serial_test::serial(quick_collab_sock)]
fn live_text_edit_propagates_through_broker() {
    let sock = std::env::temp_dir().join(format!("quick-collab-text-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind broker");
    thread::spawn(move || ui::collaboration::broker::serve(listener));
    unsafe {
        std::env::set_var("QUICK_COLLAB_SOCK", &sock);
    }

    let mut a = CollaborationManager::new();
    let mut b = CollaborationManager::new();
    thread::sleep(Duration::from_millis(150));
    let _ = a.poll();
    let _ = b.poll();

    let mut a_doc = AutoCommit::new();
    a_doc.set_actor(automerge::ActorId::from(b"alice".as_slice()));
    let t = a_doc.put_object(&ROOT, "text", ObjType::Text).unwrap();
    a_doc.splice_text(&t, 0, 0, "hello").unwrap();

    // Late-join handshake: alice ships a full snapshot; bob adopts it.
    a.send(CollabRequest::SyncCRDT { data: a_doc.save() });
    let _ = a_doc.save_incremental(); // re-baseline incremental after full save

    let mut b_doc = AutoCommit::new();
    b_doc.set_actor(automerge::ActorId::from(b"bob".as_slice()));
    let mut adopted = false;
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(50));
        for e in b.poll() {
            if let CollabEvent::CRDTUpdate { data } = e {
                let mut incoming = AutoCommit::load(&data).unwrap();
                b_doc.merge(&mut incoming).unwrap();
                adopted = true;
            }
        }
        if adopted {
            break;
        }
    }
    assert!(adopted, "bob must receive alice's snapshot");
    assert_eq!(text_of(&b_doc), "hello", "bob adopts alice's text");

    // Alice "types": incremental edit -> bob applies it.
    a_doc.splice_text(&t, 5, 0, " world").unwrap();
    let delta = a_doc.save_incremental();
    assert!(!delta.is_empty(), "the edit must produce a delta");
    a.send(CollabRequest::SyncIncremental { data: delta });

    let mut got_edit = false;
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(50));
        for e in b.poll() {
            if let CollabEvent::CRDTIncremental { data } = e {
                b_doc.load_incremental(&data).unwrap();
                got_edit = true;
            }
        }
        if got_edit && text_of(&b_doc) == "hello world" {
            break;
        }
    }
    assert_eq!(
        text_of(&b_doc),
        "hello world",
        "a live incremental text edit must propagate to the peer"
    );

    drop(a);
    drop(b);
    let _ = std::fs::remove_file(&sock);
}
