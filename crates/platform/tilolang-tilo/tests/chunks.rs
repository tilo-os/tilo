//! A record split over chunks: the store holds each chunk as a document of its own, reads and
//! writes every resource in the chunk the record's manifest places it in, and derives each index
//! row from the chunk that holds it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use automerge::AutoCommit;
use serde_json::Value as Json;
use serde_json::json;
use tilo_actions::{ActionContext, ActionInvoker};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::{Edit, EditOp, Model as _, Segment};
use tilolang_runtime::Principal;
use tilolang_runtime::Value;
use tilolang_runtime::meta::Meta;
use tilolang_runtime::place::{Place, Placement as Placed};
use tilolang_runtime::store::NewRecord;
use tilolang_runtime::value::ResourceRef;
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::chunk::{self, ChunkNo, Chunks};
use tilolang_tilo::host::{Host, Opening};
use tilolang_tilo::items::{self, Items};
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::reads;
use tilolang_tilo::record::{DynRecord, DynValue};
use tilolang_tilo::schema::{TypeSchema, lookup};

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
    let action = DynAction {
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
    };
    apply_recorded(doc, &action);
    doc.commit();
}

/// Apply `action` to `doc` as the workspace commits one: the change names the action it
/// recorded, so a replica that receives it reads which action it was
/// ([`tilo_actions::ActionContext`]).
fn apply_recorded(doc: &mut AutoCommit, action: &DynAction) {
    let encoded = action.encode().expect("the action encodes");
    let context = ActionContext::for_resource(
        action.resources().first().copied().unwrap_or("room"),
        &action.kind,
        &action.kind,
    )
    .with_app(&action.app, "tilolang")
    .with_invoker(ActionInvoker::actor("ada"));
    // The document's own actor writes it: a fork of it is a replica of its own, and two
    // replicas' changes never share a sequence number.
    tilo_actions::apply_local_with_context(doc, action, encoded, None, context)
        .expect("the action applies");
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
    let mut chunks = Chunks::of(record);
    chunks.insert(1, second);
    assert!(host.sync(&chunks), "chunk 1 is new to the store");
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

/// A board with three collections of its own: a card may be in any of them.
pub(crate) const BOARD: &str = r#"app so.tilo.board "Board"

type Card {
  text: Text merge text
}

type Board {
  left: Collection<Card>
  right: Collection<Card>
  other: Collection<Card>
  ticks: Int merge counter
}

action add(l: Board, text: Text) -> Card { l.left.insert(Card(text: text)) }

action retitle(c: Card, text: Text) { c.text = text }

action move_right(l: Board, c: Card) { l.right.move(c, to: l.right.len()) }

action move_other(l: Board, c: Card) { l.other.move(c, to: l.other.len()) }

action boom(l: Board) {
  l.left.insert(Card(text: "gone"))
  l.ticks += 1 / (l.ticks - l.ticks)
}

root Board

default view main for Board

view main(b: Board) {
  page {
    for c in b.left {
      text(c.text)
    }
  }
}
"#;

pub(crate) const BOARD_APP: &str = "so.tilo.board";

/// The documents of a workspace, one per chunk: a replica applies each edit of an action to the
/// chunk it is addressed to, as the door does, and commits each document its own change.
///
/// It holds them as the door does ([`Chunks`]), so handing them to the store hands the same
/// documents the record keeps: a fill writes one chunk per action and copies none of them.
#[derive(Clone)]
pub(crate) struct Docs(Chunks);

impl Docs {
    pub(crate) fn of(record: AutoCommit) -> Self {
        Self(Chunks::of(record))
    }

    pub(crate) fn doc(&mut self, chunk: ChunkNo) -> &mut AutoCommit {
        self.0.open(chunk)
    }

    /// Apply `action` as its edits are addressed: an edit that names no chunk is written where
    /// the record's manifest places its resource.
    pub(crate) fn apply(&mut self, action: &DynAction) {
        let mut edits: BTreeMap<ChunkNo, Vec<DynEdit>> = BTreeMap::new();
        let placement = self
            .0
            .first()
            .map(chunk::Placement::read)
            .unwrap_or_default();
        for edit in &action.edits {
            let chunk = edit
                .chunk()
                .unwrap_or_else(|| placement.chunk(edit.resource()));
            edits
                .entry(chunk)
                .or_default()
                .push(edit.clone().in_chunk(chunk));
        }
        // Each document takes the edits addressed to it as one change, as the workspace
        // commits the action once.
        for (chunk, edits) in edits {
            let one = DynAction {
                app: action.app.clone(),
                kind: action.kind.clone(),
                action_id: action.action_id.clone(),
                args: action.args.clone(),
                authorship: action.authorship.clone(),
                edits,
            };
            apply_recorded(self.doc(chunk), &one);
        }
        let held: Vec<ChunkNo> = self.0.numbers().collect();
        for chunk in held {
            self.0.open(chunk).commit();
        }
    }

    /// Send every document to the store, as the workspace hands them over.
    pub(crate) fn sync(&self, host: &Host) {
        host.sync(&self.0);
    }

    /// The documents forked: a replica of the workspace whose own changes are its own.
    fn fork(&mut self) -> Self {
        let numbers: Vec<ChunkNo> = self.0.numbers().collect();
        let mut forked = Chunks::default();
        for number in numbers {
            let forked_doc = self.0.open(number).fork();
            forked.insert(number, forked_doc);
        }
        Self(forked)
    }

    /// The documents as a set of chunks, to read a record's items across them.
    pub(crate) fn chunks(&self) -> Chunks {
        self.0.clone()
    }
}

/// Merge `other` into `into` as sync does, document by document.
fn merge(into: &mut Docs, other: &Docs) {
    let mut every: BTreeMap<ChunkNo, AutoCommit> = BTreeMap::new();
    for (number, doc) in into.0.docs().chain(other.0.docs()) {
        every
            .entry(number)
            .or_default()
            .merge_incoming(&mut doc.clone())
            .expect("the documents merge");
    }
    for doc in every.values_mut() {
        doc.commit();
    }
    into.0 = Chunks::default();
    for (number, doc) in every {
        into.0.insert(number, doc);
    }
    into.0.reread_placement();
}

/// The board of [`BOARD`], its root made in the record's own chunk, and the store over it.
pub(crate) fn board() -> (Host, Docs, Meta, String, String) {
    // Compiled once for its ids and metadata; the workspace itself is built document by
    // document, so that a replica forking it starts from a root the record holds.
    let (replica, packages) = Replica::new("ada", AutoCommit::new(), &[(BOARD, "")]);
    drop(replica);
    let package = packages[0].clone();
    let meta = package.meta.clone();
    let ty = meta
        .types
        .iter()
        .find(|declared| declared.name == "Board")
        .unwrap()
        .id
        .clone();
    let root = format!("root-{ty}");
    let mut record = AutoCommit::new();
    let action = DynAction {
        app: BOARD_APP.to_owned(),
        kind: ty.clone(),
        action_id: "create".to_owned(),
        args: json!({}),
        authorship: Authorship::default(),
        edits: vec![DynEdit::Create {
            id: root.clone(),
            record: DynRecord::empty(lookup(&ty).unwrap()).unwrap().to_json(),
        }],
    };
    apply_recorded(&mut record, &action);
    record.commit();
    assert!(
        reads::resource_obj(&record, BOARD_APP, &root).is_some(),
        "the record holds the root"
    );
    let host = Host::new(&record, Principal::new("ada"), || None).unwrap();
    assert!(
        host.store().record(&root).is_ok(),
        "the store reads the root"
    );
    host.load_builtin(BOARD, &package.ids).unwrap();
    (host, Docs::of(record), meta, root, package.ids)
}

/// What the store holds of each chunk: its changes and its open writes, by number.
fn counts(host: &Host) -> BTreeMap<ChunkNo, (usize, usize)> {
    held(host)
        .into_iter()
        .map(|(n, a, b)| (n, (a, b)))
        .collect()
}

/// The action `name` of the board, invoked on `subject` of the type `ty`, as the door plans it.
pub(crate) fn act(
    host: &Host,
    meta: &Meta,
    (ty, subject): (&str, &str),
    name: &str,
    args: Json,
) -> DynAction {
    let action = meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("no action {name}"))
        .id
        .clone();
    let args = args.as_object().cloned().unwrap_or_default();
    let kind = format!("tl.{BOARD_APP}.{ty}");
    host.plan(&PlanRequest {
        app: BOARD_APP,
        kind: &kind,
        action_id: &action,
        resource: Some(subject),
        args: &args,
        caller: None,
        from: None,
    })
    .unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// The id of the item an action inserted.
pub(crate) fn made(action: &DynAction) -> String {
    action
        .edits
        .iter()
        .find_map(|edit| match edit.bare() {
            DynEdit::Insert { item, .. } => Some(item.clone()),
            _ => None,
        })
        .expect("the action inserted an item")
}

/// The ids the collection `field` of the board holds, in order, as the store reads it whole.
pub(crate) fn left(host: &Host, root: &str, field: &str) -> Vec<String> {
    match host.store().record(root).unwrap().get(field) {
        Some(DynValue::Collection(items)) => {
            items.iter().map(|(id, _)| id.as_str().to_owned()).collect()
        }
        other => panic!("no collection: {other:?}"),
    }
}

/// How many items the chunk `number` holds of the record `root`.
pub(crate) fn held_in(chunks: &Chunks, number: ChunkNo, root: &str, schema: &TypeSchema) -> usize {
    let Some(doc) = chunks.mirror(number) else {
        return 0;
    };
    let Some(obj) = reads::resource_obj(doc, BOARD_APP, root) else {
        return 0;
    };
    Items::scan(doc, &obj, schema, number).found.len()
}

/// The copy of the item `id` the chunk `number` holds.
fn copy_in(
    chunks: &Chunks,
    number: ChunkNo,
    root: &str,
    schema: &TypeSchema,
    id: &str,
) -> Option<items::Found> {
    let doc = chunks.mirror(number)?;
    let obj = reads::resource_obj(doc, BOARD_APP, root)?;
    Items::scan(doc, &obj, schema, number).found.remove(id)
}

/// Fill the board's left collection with `n` cards, in order: the ids it made.
pub(crate) fn fill(host: &Host, docs: &mut Docs, meta: &Meta, root: &str, n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            let action = act(
                host,
                meta,
                ("Board", root),
                "add",
                json!({ "text": format!("t{i}") }),
            );
            let id = made(&action);
            docs.apply(&action);
            // As the door does: each insert is committed before the next is planned, so the
            // next one appends after it.
            docs.sync(host);
            id
        })
        .collect()
}

#[test]
fn five_thousand_cards_live_in_three_chunks_in_order() {
    let (host, mut docs, meta, root, _) = board();
    let inserted = fill(&host, &mut docs, &meta, &root, 5000);
    docs.sync(&host);

    let size = usize::try_from(chunk::ITEMS).unwrap();
    assert_eq!(docs.chunks().numbers().collect::<Vec<_>>(), [0, 1, 2]);
    let chunks = docs.chunks();
    let schema = lookup(&meta.types.iter().find(|t| t.name == "Board").unwrap().id).unwrap();
    assert_eq!(
        [0, 1, 2].map(|n| held_in(&chunks, n, &root, schema)),
        [size, size, 5000 - 2 * size],
        "each run fills its chunk before the next opens"
    );

    // In order, as inserted, and read the same by the store.
    let field = field_id(&meta, "Board", "left");
    let order_read = Items::across(&chunks, BOARD_APP, &root, schema).resolve(schema);
    assert_eq!(order_read.items(None, &field), inserted);
    assert_eq!(left(&host, &root, &field), inserted);

    // Lookups find a card through the chunk its index row names, without reading the others.
    let rows = host.store().index_contents(BOARD_APP).unwrap();
    let row = |i: usize, field: &str| {
        rows.fields
            .get(&(format!("{root}/{}", inserted[i]), field.to_owned()))
            .cloned()
    };
    let chunk_of = |i: usize| row(i, tilolang_tilo::index::CHUNK);
    assert_eq!(chunk_of(0), Some("0".to_owned()));
    assert_eq!(chunk_of(size - 1), Some("0".to_owned()));
    assert_eq!(chunk_of(size), Some("1".to_owned()));
    assert_eq!(chunk_of(2 * size), Some("2".to_owned()));
    let card_ty = meta
        .types
        .iter()
        .find(|t| t.name == "Card")
        .unwrap()
        .id
        .clone();
    assert_eq!(row(2 * size, tilolang_tilo::index::TYPE), Some(card_ty));
}

#[test]
fn a_change_to_a_card_in_the_last_chunk_writes_only_that_chunk() {
    let (host, mut docs, meta, root, _) = board();
    let inserted = fill(&host, &mut docs, &meta, &root, 5000);
    docs.sync(&host);
    let before = counts(&host);

    let card = format!("{root}/{}", inserted[4899]);
    let action = act(
        &host,
        &meta,
        ("Card", &card),
        "retitle",
        json!({ "text": "changed" }),
    );
    assert_eq!(
        action
            .edits
            .iter()
            .filter_map(DynEdit::chunk)
            .collect::<Vec<_>>(),
        [2],
        "the edit is addressed to the card's chunk alone"
    );
    docs.apply(&action);
    docs.sync(&host);

    let after = counts(&host);
    assert_eq!(after[&0], before[&0], "the record's own chunk is untouched");
    assert_eq!(after[&1], before[&1], "the first run's chunk is untouched");
    assert!(
        after[&2].0 + after[&2].1 > before[&2].0 + before[&2].1,
        "the last chunk holds the write: {before:?} then {after:?}"
    );
    let card_schema = lookup(&meta.types.iter().find(|t| t.name == "Card").unwrap().id).unwrap();
    let card_record = copy_in(&docs.chunks(), 2, &root, card_schema, &inserted[4899])
        .expect("the card is read in the last chunk");
    assert_eq!(card_record.chunk, 2);
}

/// An insert the store plans for a chunk above the record's own leaves nothing open in that
/// chunk once the record's change reaches it: it settles as one written into the record itself.
#[test]
fn an_insert_into_a_chunk_settles_when_the_record_takes_it_in() {
    let (_store, mut docs, meta, root, ids) = board();
    let left_field = field_id(&meta, "Board", "left");
    // A run above the record, so the store plans the card into chunk 1.
    {
        let record = docs.doc(chunk::FIRST);
        chunk::run(record, &root, &left_field, 1, "", 0).unwrap();
        record.commit();
    }
    let (replica, _) = Replica::new("ada", docs.doc(chunk::FIRST).fork(), &[(BOARD, &ids)]);
    let host = replica.host;
    let before = counts(&host);

    // The door invokes the action: the store writes the card into chunk 1's own document.
    let add = meta
        .actions
        .iter()
        .find(|action| action.name == "add")
        .unwrap()
        .id
        .clone();
    let board = host.root_of(BOARD_APP).unwrap();
    host.runtime()
        .invoke_action(
            &Principal::new("ada"),
            (
                add.as_str(),
                vec![Value::resource(&board), Value::text("c")],
            ),
        )
        .unwrap();
    let open = counts(&host);
    assert!(
        open.get(&1).is_some_and(|(_, pending)| *pending > 0),
        "chunk 1 holds the open write: {before:?} then {open:?}"
    );

    // The door applies what the store queued to the record, and the record hands it over.
    let committed = host.take_committed();
    assert_eq!(committed.len(), 1, "one action was queued");
    let action = committed.into_iter().next().unwrap().action;
    let card = made(&action);
    docs.apply(&action);
    docs.sync(&host);

    let settled = counts(&host);
    assert_eq!(
        settled.get(&1),
        Some(&(1, 0)),
        "chunk 1 holds the record's change and nothing open: {before:?} then {settled:?}"
    );
    assert_eq!(
        settled[&0].1, 0,
        "nothing stays open in the record: {settled:?}"
    );
    assert_eq!(left(&host, &root, &left_field), [card]);
}

#[test]
fn a_card_moved_on_two_replicas_at_once_lands_in_one_collection() {
    let (host, mut docs, meta, root, ids) = board();
    let (right, other) = (
        field_id(&meta, "Board", "right"),
        field_id(&meta, "Board", "other"),
    );
    // The two target collections have a run in a chunk each, above the record.
    {
        let record = docs.doc(chunk::FIRST);
        chunk::run(record, &root, &right, 1, "", 0).unwrap();
        chunk::run(record, &root, &other, 2, "", 0).unwrap();
        record.commit();
    }
    docs.sync(&host);
    let first = act(
        &host,
        &meta,
        ("Board", &root),
        "add",
        json!({ "text": "c" }),
    );
    let card = made(&first);
    docs.apply(&first);
    docs.sync(&host);
    let applied = act(
        &host,
        &meta,
        ("Board", &root),
        "add",
        json!({ "text": "d" }),
    );
    docs.apply(&applied);
    docs.sync(&host);

    // Two replicas, each moving the card to a collection of its own.
    let mut ada = docs.fork();
    let mut grace = docs.fork();
    let (mut ada_host, _) = Replica::new("ada", ada.doc(chunk::FIRST).fork(), &[(BOARD, &ids)]);
    let (mut grace_host, _) =
        Replica::new("grace", grace.doc(chunk::FIRST).fork(), &[(BOARD, &ids)]);
    let at = format!("{root}/{card}");
    for (docs, host, name, chunk) in [
        (&mut ada, &mut ada_host, "move_right", 1),
        (&mut grace, &mut grace_host, "move_other", 2),
    ] {
        let action = act(
            &host.host,
            &meta,
            ("Board", &root),
            name,
            json!({ "c": at }),
        );
        let mut chunks: Vec<ChunkNo> = action.edits.iter().filter_map(DynEdit::chunk).collect();
        chunks.sort_unstable();
        chunks.dedup();
        assert_eq!(
            chunks,
            [0, chunk],
            "{name}: out of the record, into the run's chunk"
        );
        docs.apply(&action);
        docs.sync(&host.host);
    }

    merge(&mut ada, &grace);
    ada.sync(&ada_host.host);
    ada.sync(&grace_host.host);

    let chunks = ada.chunks();
    let schema = lookup(&meta.types.iter().find(|t| t.name == "Board").unwrap().id).unwrap();
    let (moved, kept) = (
        copy_in(&chunks, 1, &root, schema, &card).expect("a copy in the first target's chunk"),
        copy_in(&chunks, 2, &root, schema, &card).expect("a copy in the second's"),
    );
    assert!(
        copy_in(&chunks, 0, &root, schema, &card).is_none(),
        "the record's chunk holds no copy"
    );

    // The copy whose placing change is later in the record's order is the one read; the other is
    // not in any collection.
    let written = |found: &items::Found| found.held.as_ref().map(|held| held.written.clone());
    let winner = if written(&moved) > written(&kept) {
        1
    } else {
        2
    };
    let resolved = Items::across(&chunks, BOARD_APP, &root, schema).resolve(schema);
    let (in_right, in_other) = (resolved.items(None, &right), resolved.items(None, &other));
    assert_eq!(in_right.len() + in_other.len(), 1, "{resolved:?}");
    assert_eq!(in_right.len(), usize::from(winner == 1), "{resolved:?}");
    assert_eq!(in_other.len(), usize::from(winner == 2), "{resolved:?}");

    // Both stores read the same board, and the card is in one collection only.
    assert_eq!(
        ada_host.host.store().record(&root).unwrap(),
        grace_host.host.store().record(&root).unwrap()
    );
    let record = ada_host.host.store().record(&root).unwrap();
    let some = |field: &str| match record.get(field) {
        Some(DynValue::Collection(items)) => items.iter().any(|(id, _)| id.as_str() == card),
        _ => false,
    };
    assert!(some(&right) ^ some(&other), "one copy, in one collection");
}

/// The cards `n` of the board, written in one action per run as an import would: the board a
/// test measures a card's cost on, made without paying that cost `n` times.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "index arithmetic over the cards a test made"
)]
fn fill_bulk(docs: &mut Docs, meta: &Meta, root: &str, n: usize) -> Vec<String> {
    let field = field_id(meta, "Board", "left");
    let ty = meta
        .types
        .iter()
        .find(|declared| declared.name == "Card")
        .unwrap()
        .id
        .clone();
    let schema = lookup(&ty).unwrap();
    let per = usize::try_from(chunk::ITEMS).unwrap();
    let ids: Vec<String> = (0..n).map(|index| format!("c{index}")).collect();
    for (round, run) in ids.chunks(per).enumerate() {
        let number = chunk::FIRST + u64::try_from(round).unwrap();
        let edits: Vec<DynEdit> = run
            .iter()
            .enumerate()
            .map(|(at, id)| {
                DynEdit::Insert {
                    id: root.to_owned(),
                    item: id.clone(),
                    record: DynRecord::empty(schema).unwrap().to_json(),
                    at: Placed::At {
                        place: Place {
                            parent: None,
                            field: field.clone(),
                            key: format!("{:010}", round * per + at),
                        },
                        was: None,
                    }
                    .encode(),
                }
                .in_chunk(number)
            })
            .collect();
        docs.apply(&DynAction {
            app: BOARD_APP.to_owned(),
            kind: ty.clone(),
            action_id: "add".to_owned(),
            args: json!({}),
            authorship: Authorship::default(),
            edits,
        });
        let last = format!("{:010}", round * per + run.len() - 1);
        let len = u64::try_from(run.len()).unwrap();
        let record = docs.doc(chunk::FIRST);
        chunk::run(record, root, &field, number, &last, len).unwrap();
        record.commit();
    }
    ids
}

/// The chunk a collection's first run is in, and of the cards the store reads before the chunk
/// above it arrives: the record's own.
fn first_run() -> usize {
    usize::try_from(chunk::ITEMS).unwrap()
}

/// The cards of the board `root` as the store lists them, read whole.
fn cards_in(host: &Host, meta: &Meta, root: &str) -> Vec<String> {
    left(host, root, &field_id(meta, "Board", "left"))
}

/// A chunk that holds cards of the board and that the source cannot give: it is not on this
/// device yet, or the peer holding it is away. The store asks for it once and waits for the sync
/// that brings it, a window reads the cards it has meanwhile and says its count is not the
/// board's, and the frame the chunk arrives in reads it, so the index is whole.
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
#[test]
fn a_chunk_the_source_cannot_give_is_asked_for_once_and_read_when_it_arrives() {
    let (earlier, mut docs, meta, root, ids) = board();
    drop(earlier);
    let cards = fill_bulk(&mut docs, &meta, &root, 3_000);
    let per_run = first_run();
    assert!(
        cards.len() > per_run,
        "the cards fill the record's own run and one above it"
    );

    // This replica's source holds the record alone: the chunk the rest of the cards are in has
    // not reached this device, and asking for it answers nothing.
    let record = docs.doc(chunk::FIRST).clone();
    let replica: Rc<RefCell<Chunks>> = Rc::new(RefCell::new(Chunks::of(record.clone())));
    let host = Host::open(
        Opening {
            record: Some(&record),
            chunks: None,
            index_dir: None,
            device: None,
        },
        Principal::new("ada"),
        || None,
    )
    .unwrap();
    host.sync_chunks(&replica.borrow());
    let asked: Rc<RefCell<Vec<ChunkNo>>> = Rc::default();
    {
        let (source, asked) = (Rc::clone(&replica), Rc::clone(&asked));
        host.set_chunk_loader(Box::new(move |number| {
            asked.borrow_mut().push(number);
            source.borrow().mirror(number).cloned()
        }));
    }
    host.load_builtin(BOARD, &ids).unwrap();
    host.wait_index();

    // The window reads the cards the store has, and its count is not the board's yet.
    assert_eq!(cards_in(&host, &meta, &root), cards[..per_run]);
    assert!(host.index_answers(), "the store answers rows");
    assert!(
        !host.store().index_whole(),
        "the chunk above holds cards the store has not read"
    );
    // It was asked for once, and nothing is owed: a frame rests while the chunk is away.
    assert_eq!(*asked.borrow(), [1], "the chunk is asked for once");
    assert!(
        !host.index_pending(),
        "the store waits for the chunk rather than reading it again"
    );
    for _ in 0..3 {
        host.sync_chunks(&replica.borrow());
        host.poll_index();
    }
    assert_eq!(
        *asked.borrow(),
        [1],
        "frames that bring no chunk ask for it no more"
    );
    assert_eq!(held(&host).len(), 1, "the store starts no chunk empty");

    // A card a person renames while the chunk is away keeps the name: it is written where the
    // card is, and read back there.
    let type_id = |name: &str| {
        meta.types
            .iter()
            .find(|declared| declared.name == name)
            .unwrap()
            .id
            .clone()
    };
    let at = ResourceRef::root(&root, type_id("Board")).item(&cards[7], type_id("Card"));
    let card = at.id_string();
    let rename = act(
        &host,
        &meta,
        ("Card", &card),
        "retitle",
        json!({ "text": "renamed" }),
    );
    docs.apply(&rename);
    let record = docs.doc(chunk::FIRST).clone();
    host.sync_from(&record);
    let text_field = field_id(&meta, "Card", "text");
    let text = |host: &Host| match host.store().record_at(&at).unwrap().get(&text_field) {
        Some(DynValue::Text(text)) => text.clone(),
        other => panic!("no text: {other:?}"),
    };
    assert_eq!(text(&host), "renamed");
    assert!(
        !host.index_pending(),
        "the card's change is taken in while the chunk is still away"
    );

    // An edit that belongs in the chunk that is away is refused, never written where the store
    // holds nothing of it: a card made there would be lost when the chunk reached this device.
    let add = meta
        .actions
        .iter()
        .find(|action| action.name == "add")
        .unwrap()
        .id
        .clone();
    let board = host.root_of(BOARD_APP).unwrap();
    let refused = host.runtime().invoke_action(
        &Principal::new("ada"),
        (
            add.as_str(),
            vec![Value::resource(&board), Value::text("away")],
        ),
    );
    assert!(
        refused.is_err(),
        "the store refuses an edit it would write into a chunk it does not hold"
    );
    assert_eq!(held(&host).len(), 1, "and starts no chunk empty");
    assert_eq!(
        cards_in(&host, &meta, &root),
        cards[..per_run],
        "the refused edit left the board as it was"
    );

    // The chunk arrives through the sync: it is read once, and the board is whole.
    let second = docs.doc(1).clone();
    replica.borrow_mut().insert(1, second);
    host.sync_chunks(&replica.borrow());
    assert_eq!(
        *asked.borrow(),
        [1, 1],
        "one ask while it was away, and one the source answers"
    );
    assert!(host.store().index_whole(), "the index covers the board");
    assert!(host.index_ready(), "and is read from, not built");
    assert_eq!(cards_in(&host, &meta, &root), cards, "every card in order");
    assert_eq!(
        text(&host),
        "renamed",
        "the card keeps the name it was given while the chunk was away"
    );
}

/// What one card added to a board of `cards` costs, counted per insert over `samples` inserts:
/// what the store did, what it read, and the work automerge did taking the change into the
/// mirror. Counts, so neither the machine nor its load decides them.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "counts a test read, divided by its samples"
)]
fn insert_costs(
    host: &Host,
    docs: &mut Docs,
    meta: &Meta,
    root: &str,
    cards: usize,
    samples: usize,
) -> Vec<(&'static str, u64)> {
    fill_bulk(docs, meta, root, cards);
    docs.sync(host);
    let stats = host.store().stats();
    let reads = host.store().read_stats();
    let work = automerge::work();
    for i in cards..cards.saturating_add(samples) {
        let action = act(
            host,
            meta,
            ("Board", root),
            "add",
            json!({ "text": format!("s{i}") }),
        );
        docs.apply(&action);
        docs.sync(host);
    }
    let now = host.store().stats();
    let reads_now = host.store().read_stats();
    let work_now = automerge::work();
    let per = |all: u64| all / u64::try_from(samples).unwrap_or(1).max(1);
    let delta = |before: u64, after: u64| per(after.saturating_sub(before));
    vec![
        (
            "syncs",
            delta(stats.incremental_syncs, now.incremental_syncs),
        ),
        ("full syncs", delta(stats.full_syncs, now.full_syncs)),
        ("index writes", delta(stats.index_writes, now.index_writes)),
        ("catch-ups", delta(stats.catch_ups, now.catch_ups)),
        ("builds", delta(stats.builds, now.builds)),
        ("located", delta(reads.doc.located, reads_now.doc.located)),
        ("fields", delta(reads.doc.fields, reads_now.doc.fields)),
        (
            "listings",
            delta(reads.doc.listings, reads_now.doc.listings),
        ),
        ("entries", delta(reads.doc.entries, reads_now.doc.entries)),
        ("dirs", delta(reads.index.dirs, reads_now.index.dirs)),
        ("pages", delta(reads.index.pages, reads_now.index.pages)),
        (
            "index fields",
            delta(reads.index.fields, reads_now.index.fields),
        ),
        ("evaluated", delta(reads.evaluated, reads_now.evaluated)),
        ("versions", delta(reads.versions, reads_now.versions)),
        (
            "version changes",
            delta(reads.version_changes, reads_now.version_changes),
        ),
        ("walked ops", delta(work.walked_ops, work_now.walked_ops)),
        (
            "searched slabs",
            delta(work.searched_slabs, work_now.searched_slabs),
        ),
        (
            "visited changes",
            delta(work.visited_changes, work_now.visited_changes),
        ),
        (
            "renumbered objects",
            delta(work.renumbered_objects, work_now.renumbered_objects),
        ),
    ]
}

/// A card added to a board that already holds four thousand costs what one added to a board of a
/// thousand costs: the store derives the card and writes the one index the change reached, reads
/// nothing that grows with the board, and automerge applies the change without walking the chunk
/// it lands in or renumbering the objects it holds.
#[test]
fn a_card_added_to_a_big_board_costs_what_one_added_to_a_small_board_costs() {
    let samples = 25;
    let (small_host, mut small_docs, small_meta, small_root, _) = board();
    let small = insert_costs(
        &small_host,
        &mut small_docs,
        &small_meta,
        &small_root,
        1_000,
        samples,
    );
    let (large_host, mut large_docs, large_meta, large_root, _) = board();
    let large = insert_costs(
        &large_host,
        &mut large_docs,
        &large_meta,
        &large_root,
        4_000,
        samples,
    );
    // The documents an insert writes are the chunk its card lands in, and the record, whose
    // manifest names the run it went into: one document while a board fits the record's own
    // chunk, two once its cards live in a chunk above it. The shape, not work that grows.
    let count = |costs: &[(&'static str, u64)], name: &str| {
        costs
            .iter()
            .find(|(held, _)| *held == name)
            .map_or_else(|| panic!("no count named {name}"), |(_, count)| *count)
    };
    assert_eq!(count(&small, "syncs"), 1, "one document at 1000 cards");
    assert_eq!(
        count(&large, "syncs"),
        2,
        "the card's chunk and the record at 4000"
    );
    // A card's change reached the board's package, so it writes that package's index: a store
    // that writes every loaded package's index for every change would pay a commit of nothing
    // here, and one per package a workspace holds.
    assert_eq!(count(&small, "index writes"), 1, "one index per card");
    assert_eq!(count(&large, "index writes"), 1, "one index per card");
    // A change by an actor that sorts before others renumbers the objects those others made in
    // each document it reaches: a handful the record's own writer made, never one per card.
    assert!(
        count(&large, "renumbered objects") <= 2 * count(&small, "renumbered objects").max(8),
        "an insert renumbers objects that grow with the board: {small:?} against {large:?}"
    );
    let mut misses: Vec<String> = Vec::new();
    for ((name, at_small), (_, at_large)) in small.iter().zip(&large) {
        if !matches!(*name, "syncs" | "renumbered objects") && at_small != at_large {
            misses.push(format!(
                "{name}: {at_small} per insert at 1000, {at_large} at 4000"
            ));
        }
    }
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// A window whose record the framework keeps as one document — no chunk above the record's own is
/// held apart — keeps a card added past a full run: the door writes it into that document, and the
/// board reads it there, in its place after the others, in the window and once opened again.
#[test]
fn a_card_added_past_a_full_run_of_a_record_kept_as_one_document_is_kept() {
    let (earlier, mut docs, meta, root, ids) = board();
    drop(earlier);
    let mut cards = fill_bulk(&mut docs, &meta, &root, first_run());
    let mut record = docs.doc(chunk::FIRST).clone();
    let open = |record: &AutoCommit| {
        let host = Host::open(
            Opening {
                record: Some(record),
                chunks: None,
                index_dir: None,
                device: None,
            },
            Principal::new("ada"),
            || None,
        )
        .unwrap();
        host.load_builtin(BOARD, &ids).unwrap();
        host.wait_index();
        host
    };
    let host = open(&record);
    assert_eq!(cards_in(&host, &meta, &root), cards);

    let add = act(
        &host,
        &meta,
        ("Board", &root),
        "add",
        json!({ "text": "one more" }),
    );
    assert!(
        add.edits
            .iter()
            .any(|edit| edit.chunk() == Some(chunk::FIRST + 1)),
        "the card goes past the full run"
    );
    cards.push(made(&add));
    apply_recorded(&mut record, &add);
    host.sync_from(&record);
    host.wait_index();
    assert_eq!(
        cards_in(&host, &meta, &root),
        cards,
        "the window reads the card"
    );
    drop(host);
    assert_eq!(
        cards_in(&open(&record), &meta, &root),
        cards,
        "and so does a window opened on the record again"
    );
}
