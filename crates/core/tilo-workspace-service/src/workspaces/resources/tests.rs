//! A workspace's store answers from the index its start read, without opening its documents.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test failing is its assertion failing"
)]

use std::collections::BTreeMap;

use collab_types::store::{Cell, Resource, Value};
use tilo_resource_store::Stamp;

/// A workspace keeps its chain as parts of its store: 26 documents, a thousand
/// resources each. The process is killed rather than stopped, so nothing is kept as it
/// goes: a start reads each document's snapshot of the index, not the document, and a
/// read is answered from that index.
#[test]
fn a_store_a_hard_stop_left_answers_from_its_index() {
    /// Parts of the chain.
    const PARTS: usize = 26;
    /// Resources each part holds.
    const EACH: usize = 1_000;
    let dir = tempfile::tempdir().unwrap();
    let id = tilo_server_protocol::WorkspaceId::new();
    let file = dir.path().join("workspace.workspace");
    let mut resources = super::Resources::open(&file, id);
    let stamp = Stamp::default();
    for part in 0..PARTS {
        let kind = format!("issue{part}");
        let before = resources.heads(&kind);
        {
            let mut store = resources.store();
            for index in 0..EACH {
                let resource = Resource {
                    readers: "*".into(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::from([(
                        "title".to_owned(),
                        Cell::One(Value::Text(format!("Issue {part}-{index}"))),
                    )]),
                };
                store
                    .put(&kind, &format!("i{part}-{index}"), resource, &stamp)
                    .unwrap();
            }
            drop(store);
        }
        let doc = format!("*/{kind}#0");
        resources.kept(&doc, &before).unwrap();
    }
    // A hard stop: the process is killed, so nothing writes the store out.
    std::mem::forget(resources);

    let started = std::time::Instant::now();
    let reopened = super::Resources::open(&file, id);
    let opened = started.elapsed();
    let store = reopened.store();
    assert_eq!(
        store.in_memory(),
        0,
        "a start reads the index's snapshots, not the store's documents"
    );
    assert_eq!(
        store.count("issue13", &[], &|_| true),
        EACH,
        "the index answers for a part of the chain"
    );
    assert!(
        store
            .get("issue13", "i13-7", &|_| true)
            .is_some_and(|held| held.scalar("title") == Value::Text("Issue 13-7".into())),
        "a resource of a part reads from the index"
    );
    assert_eq!(store.in_memory(), 0, "reading it opened no document");
    drop(store);
    assert!(
        opened < std::time::Duration::from_secs(2),
        "the start took {opened:?}"
    );
}

/// A document whose id is longer than a file system names one file is kept, and read again by
/// the next start, as any other is.
#[test]
fn a_document_with_a_long_id_is_kept_across_a_start() {
    let dir = tempfile::tempdir().unwrap();
    let id = tilo_server_protocol::WorkspaceId::new();
    let file = dir.path().join("workspace.workspace");
    let kind = "so.tilo.example.a-kind-with-a-long-name.".repeat(8);
    let mut resources = super::Resources::open(&file, id);
    let doc = {
        let mut store = resources.store();
        let resource = Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields: BTreeMap::from([(
                "title".to_owned(),
                Cell::One(Value::Text("Long named".into())),
            )]),
        };
        store.put(&kind, "p1", resource, &Stamp::default()).unwrap();
        store.index().get(&kind, "p1").unwrap().doc.clone()
    };
    assert!(
        hex::encode(&doc).len() > 255,
        "the id is longer than a name"
    );
    resources.kept(&doc, &[]).unwrap();
    std::mem::forget(resources);

    let reopened = super::Resources::open(&file, id);
    let store = reopened.store();
    assert!(
        store
            .get(&kind, "p1", &|_| true)
            .is_some_and(|held| held.scalar("title") == Value::Text("Long named".into())),
        "the resource reads again after the start"
    );
}
