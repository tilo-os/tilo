//! A record split over chunks: the store holds each chunk as a document of its own, reads and
//! writes every resource in the chunk the record's manifest places it in, and derives each index
//! row from the chunk that holds it.

use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::Action as _;
use tilo_resource::{Edit, EditOp, Model as _, Segment};
use tilolang_runtime::Principal;
use tilolang_runtime::meta::Meta;
use tilolang_runtime::store::NewRecord;
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::chunk::{self, ChunkNo};
use tilolang_tilo::host::Host;
use tilolang_tilo::record::{DynRecord, DynValue};
use tilolang_tilo::schema::lookup;

use crate::fixtures::{Replica, TODO, field_id};

const APP: &str = "so.tilo.todo";

/// The todo package's metadata, with its schemas registered: its `TodoList` type's id, and the
/// id of the type's title field.
fn todo() -> (Meta, String, String) {
    let (replica, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    drop(replica);
    let meta = packages[0].meta.clone();
    let list = meta
        .types
        .iter()
        .find(|declared| declared.name == "TodoList")
        .unwrap()
        .id
        .clone();
    (meta.clone(), list, field_id(&meta, "TodoList", "title"))
}

/// Make the `TodoList` `id` in `doc`, titled `title`, as one change.
fn create(doc: &mut AutoCommit, family: &(Meta, String, String), id: &str, title: &str) {
    let (_, list, title_field) = family;
    DynAction {
        app: APP.to_owned(),
        kind: list.clone(),
        action_id: "create".to_owned(),
        args: json!({}),
        authorship: Authorship::default(),
        edits: vec![
            DynEdit::Create {
                id: id.to_owned(),
                record: DynRecord::empty(lookup(list).unwrap()).unwrap().to_json(),
            },
            DynEdit::Edit {
                id: id.to_owned(),
                edit: Edit::new(
                    vec![Segment::Field(title_field.clone())],
                    EditOp::Replace {
                        text: title.to_owned(),
                    },
                ),
            },
        ],
    }
    .apply(doc)
    .unwrap();
    doc.commit();
}

/// The title the store reads of the record `id`.
fn title(host: &Host, id: &str, title_field: &str) -> String {
    match host.store().record(id).unwrap().get(title_field) {
        Some(DynValue::Text(text)) => text.clone(),
        other => panic!("no text: {other:?}"),
    }
}

/// The workspace record: it holds `first` in chunk 0 and its manifest places `second`, made in
/// the chunk's own document, and `third`, made in the store later, in chunk 1; and the host over
/// it, with chunk 1 sent to it.
fn split() -> (Host, (Meta, String, String)) {
    let family = todo();
    let mut record = AutoCommit::new();
    create(&mut record, &family, "first", "first");
    chunk::place(&mut record, "second", 1).unwrap();
    chunk::place(&mut record, "third", 1).unwrap();
    chunk::place(&mut record, "fourth", 1).unwrap();
    record.commit();
    let mut second = AutoCommit::new();
    create(&mut second, &family, "second", "second");

    let host = Host::new(&record, Principal::new("ada"), || None).unwrap();
    host.load_builtin(TODO, "").unwrap();
    assert!(host.sync_chunk(1, &second), "chunk 1 is new to the store");
    (host, family)
}

/// What the store holds of each chunk: its changes, and its open writes.
fn held(host: &Host) -> Vec<(ChunkNo, usize, usize)> {
    host.store().held_chunks()
}

#[test]
fn a_record_split_over_two_chunks_reads_writes_and_syncs_each_resource_through_its_chunk() {
    let (host, (_, list, title_field)) = split();
    let store = host.store();
    assert_eq!(title(&host, "first", &title_field), "first");
    assert_eq!(title(&host, "second", &title_field), "second");
    assert_eq!(
        held(&host).iter().map(|(at, ..)| *at).collect::<Vec<_>>(),
        [0, 1]
    );

    // Each index row names the chunk that holds its record.
    let docs = store.index_contents(APP).unwrap().docs;
    assert_eq!(docs.get("first"), Some(&(list.clone(), 0)));
    assert_eq!(docs.get("second"), Some(&(list.clone(), 1)));

    // A record the manifest places in chunk 1 is made there, and read back from there.
    let new = NewRecord {
        ty: list,
        app: APP.to_owned(),
        fields: Vec::new(),
    };
    let before = held(&host);
    store.create_with_id(&new, "third").unwrap();
    let after = held(&host);
    assert_eq!(
        after[0], before[0],
        "the record's own document took no write"
    );
    assert_eq!(after[1].0, 1);
    assert!(
        after[1].1 + after[1].2 > before[1].1 + before[1].2,
        "chunk 1 took the write: {before:?} then {after:?}"
    );
    assert_eq!(store.record("third").unwrap().schema().name, "TodoList");
}

#[test]
fn a_change_to_a_resource_in_a_chunk_reaches_only_that_chunk() {
    let (host, (_, list, _)) = split();
    let store = host.store();
    let before = held(&host);
    // Two records the manifest places in chunk 1, made by the store.
    for id in ["third", "fourth"] {
        store
            .create_with_id(
                &NewRecord {
                    ty: list.clone(),
                    app: APP.to_owned(),
                    fields: Vec::new(),
                },
                id,
            )
            .unwrap();
    }
    let after = held(&host);
    assert_eq!(after.len(), 2, "the record and the chunk above it");
    assert_eq!(
        after[0], before[0],
        "counted per document: the record's chunk is unchanged"
    );
    assert!(
        after[1].2 >= before[1].2 + 2,
        "chunk 1 holds the writes: {before:?} then {after:?}"
    );
}
