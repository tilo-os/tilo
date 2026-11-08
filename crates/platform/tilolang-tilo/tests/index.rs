//! The index the store reads lists, filters and search from: kept change by change, it holds
//! what deriving it from the record anew holds, whatever replicas wrote and in which order; an
//! index stamped with older heads catches up; a corrupt one is built anew and never read.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_runtime::Principal;
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::host::{Host, Opening};
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::record::DynValue;

use crate::fixtures::{APP as APP_COUNTER, COUNTER, Replica, TODO, field_id, type_id};

const APP: &str = "so.tilo.todo";
const KIND: &str = "tl.so.tilo.todo.TodoList";

/// A small deterministic generator: the same sequence every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n.max(1)).unwrap();
        usize::try_from(self.next().checked_rem(n).unwrap()).unwrap()
    }
}

fn plan(replica: &Replica, name: &str, args: Json, root: &str) -> DynAction {
    let meta = &replica.host.packages()[0].meta;
    let action = meta.actions.iter().find(|a| a.name == name).unwrap();
    replica
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: KIND,
            action_id: &action.id,
            resource: Some(root),
            args: args.as_object().unwrap(),
            caller: None,
            from: None,
        })
        .unwrap()
}

fn edit(replica: &mut Replica, root: &str, edit: Edit<tilolang_tilo::record::DynRecord>) {
    replica.apply(&DynAction {
        app: APP.to_owned(),
        kind: KIND.to_owned(),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: root.to_owned(),
            edit,
        }],
    });
}

fn items(replica: &Replica, root: &str) -> Vec<String> {
    let meta = &replica.host.packages()[0].meta;
    match replica
        .host
        .store()
        .record(root)
        .unwrap()
        .get(&field_id(meta, "TodoList", "items"))
    {
        Some(DynValue::Collection(items)) => {
            items.iter().map(|(id, _)| id.as_str().to_owned()).collect()
        }
        other => panic!("{other:?}"),
    }
}

/// One random change by `replica`: an item added, retitled, finished, moved, removed, or the
/// list retitled or tagged.
fn random_change(rng: &mut Rng, replica: &mut Replica, root: &str, step: usize) {
    let meta = replica.host.packages()[0].meta.clone();
    let items_field = field_id(&meta, "TodoList", "items");
    let ids = items(replica, root);
    let pick = |rng: &mut Rng| ids.get(rng.below(ids.len())).cloned();
    match rng.below(8) {
        0 | 1 => {
            let action = plan(
                replica,
                "add",
                json!({ "text": format!("task {step} word{}", step % 5) }),
                root,
            );
            replica.apply(&action);
        }
        2 => {
            if let Some(id) = pick(rng) {
                edit(
                    replica,
                    root,
                    Edit::new(
                        vec![
                            Segment::Item(Id::from_raw(&id)),
                            Segment::Field(field_id(&meta, "Item", "text")),
                        ],
                        EditOp::Replace {
                            text: format!("renamed {step}"),
                        },
                    ),
                );
            }
        }
        3 => {
            if let Some(id) = pick(rng) {
                let finish = meta.actions.iter().find(|a| a.name == "finish").unwrap();
                let list_ty = meta.types.iter().find(|t| t.name == "TodoList").unwrap();
                let item_ty = meta.types.iter().find(|t| t.name == "Item").unwrap();
                let item =
                    ResourceRef::root(root, list_ty.id.clone()).item(&id, item_ty.id.clone());
                replica
                    .host
                    .runtime()
                    .invoke_action(
                        &Principal::new(&replica.name),
                        (finish.id.as_str(), vec![Value::resource(&item)]),
                    )
                    .unwrap();
                replica.commit();
            }
        }
        4 => {
            if let Some(id) = pick(rng) {
                let action = plan(
                    replica,
                    "move_first",
                    json!({ "i": format!("{root}/{items_field}:{id}") }),
                    root,
                );
                replica.apply(&action);
            }
        }
        5 => {
            if let Some(id) = pick(rng) {
                replica.apply(&DynAction {
                    app: APP.to_owned(),
                    kind: "tl.so.tilo.todo.TodoList".to_owned(),
                    action_id: "$edit".to_owned(),
                    args: Json::Null,
                    authorship: Authorship::default(),
                    edits: vec![DynEdit::Place {
                        id: root.to_owned(),
                        item: id,
                        at: tilolang_runtime::place::Placement::Removed.encode(),
                        nested: Vec::new(),
                        ty: String::new(),
                        holders: vec![root.to_owned()],
                    }],
                });
            }
        }
        6 => {
            let action = plan(
                replica,
                "tag",
                json!({ "t": format!("tag{}", step % 3) }),
                root,
            );
            replica.apply(&action);
        }
        _ => edit(
            replica,
            root,
            Edit::new(
                vec![Segment::Field(field_id(&meta, "TodoList", "title"))],
                EditOp::Replace {
                    text: format!("list {step}"),
                },
            ),
        ),
    }
}

/// What deriving the index from `replica`'s record anew holds.
fn anew(replica: &Replica, lock: &str) -> tilolang_tilo::index::Contents {
    let mut doc = replica.doc.clone();
    let fresh = Replica::new("fresh", doc.fork(), &[(TODO, lock)]).0;
    fresh.host.store().index_contents(APP).unwrap()
}

fn kept(replica: &Replica) -> tilolang_tilo::index::Contents {
    replica.host.store().index_contents(APP).unwrap()
}

/// Ada's workspace with a list, and the package's ids lock.
fn workspace() -> (Replica, String, String) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let lock = packages[0].ids.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    (ada, root.doc, lock)
}

/// What a person sees of the list: the rows of its `items`, and the items the record holds.
fn listed(replica: &Replica, root: &str) -> Vec<String> {
    let meta = &replica.host.packages()[0].meta;
    let list = ResourceRef::root(root, type_id(meta, "TodoList"));
    let spec = tilolang_runtime::store::FieldSpec {
        id: field_id(meta, "TodoList", "items"),
        kind: tilolang_runtime::meta::FieldKind::Collection,
    };
    tilolang_runtime::store::Store::listing(replica.host.store(), &list, &spec)
        .unwrap()
        .ids()
}

/// The maps the record `root` of `replica` keeps its items in.
fn item_maps(replica: &Replica, root: &str) -> Vec<automerge::ObjId> {
    let held = tilolang_tilo::reads::resource_obj(&replica.doc, APP, root).unwrap();
    tilolang_tilo::containers::maps(&replica.doc, &held, tilolang_runtime::place::ITEMS_KEY)
}

/// How many maps the record `root` of `replica` keeps items in: one once the record has folded
/// the copies a replica made into the one it keeps.
fn maps_holding_items(replica: &Replica, root: &str) -> usize {
    use automerge::ReadDoc as _;
    item_maps(replica, root)
        .iter()
        .filter(|items| replica.doc.keys(items).next().is_some())
        .count()
}

/// Each replica adds the list's first item before the two meet: the record is made with the map
/// its items go in, so both items go into it and both are the list's.
#[test]
fn an_item_each_replica_added_before_they_met_is_the_lists_on_both() {
    let (mut ada, root, lock) = workspace();
    let mut grace = ada.fork("grace", &[(TODO, &lock)]);
    assert_eq!(
        item_maps(&ada, &root).len(),
        1,
        "the record is made with the map its items go in"
    );
    let ada_first = plan(&ada, "add", json!({ "text": "ada's" }), &root);
    ada.apply(&ada_first);
    let grace_first = plan(&grace, "add", json!({ "text": "grace's" }), &root);
    grace.apply(&grace_first);
    ada.sync_with(&mut grace);
    for (name, replica) in [("ada", &ada), ("grace", &grace)] {
        assert_eq!(
            items(replica, &root).len(),
            2,
            "{name}: both items are the list's"
        );
        assert_eq!(
            maps_holding_items(replica, &root),
            1,
            "{name}: one map of items"
        );
        assert_eq!(listed(replica, &root), items(replica, &root), "{name}");
        assert_eq!(kept(replica), anew(replica, &lock), "{name}: the index");
    }
}

/// A record written before the platform made its map of items with it, two replicas that each
/// added an item to it before they met, and a write on either side: both items read on both, and
/// the record folds the two maps into one, wherever the write was made.
#[test]
fn a_record_that_holds_two_maps_of_items_reads_both_and_keeps_one() {
    for write_by in ["ada", "grace"] {
        let (mut ada, root, lock) = workspace();
        // The record as an earlier version of the platform left it: no map of items at all.
        let held = tilolang_tilo::reads::resource_obj(&ada.doc, APP, &root).unwrap();
        let items_key = tilolang_runtime::place::ITEMS_KEY;
        {
            use automerge::ReadDoc as _;
            use automerge::transaction::Transactable as _;
            assert!(ada.doc.get(&held, items_key).unwrap().is_some());
            ada.doc.delete(&held, items_key).unwrap();
            ada.doc.commit();
        }
        let mut grace = ada.fork("grace", &[(TODO, &lock)]);
        let ada_first = plan(&ada, "add", json!({ "text": "ada's" }), &root);
        ada.apply(&ada_first);
        let grace_first = plan(&grace, "add", json!({ "text": "grace's" }), &root);
        grace.apply(&grace_first);
        ada.sync_with(&mut grace);
        assert_eq!(
            maps_holding_items(&ada, &root),
            2,
            "{write_by}: each replica made a map of items"
        );
        for (name, replica) in [("ada", &ada), ("grace", &grace)] {
            assert_eq!(
                items(replica, &root).len(),
                2,
                "{write_by}: {name} reads both maps' items"
            );
            assert_eq!(
                listed(replica, &root),
                items(replica, &root),
                "{write_by}: {name}: the rows are the items"
            );
        }

        // A write on one side folds the two maps into the one the record keeps, and both items
        // stay the list's on both.
        let writer = if write_by == "ada" {
            &mut ada
        } else {
            &mut grace
        };
        let second = plan(writer, "add", json!({ "text": "later" }), &root);
        writer.apply(&second);
        ada.sync_with(&mut grace);
        for (name, replica) in [("ada", &ada), ("grace", &grace)] {
            assert_eq!(
                maps_holding_items(replica, &root),
                1,
                "{write_by}: {name}: the record keeps one map of items"
            );
            assert_eq!(
                items(replica, &root).len(),
                3,
                "{write_by}: {name}: every item is the list's"
            );
            assert_eq!(
                listed(replica, &root),
                items(replica, &root),
                "{write_by}: {name}"
            );
            assert_eq!(
                kept(replica),
                anew(replica, &lock),
                "{write_by}: {name}: the index"
            );
        }
    }
}

#[test]
fn an_index_kept_change_by_change_holds_what_deriving_it_anew_holds() {
    for seed in [7_u64, 8, 1234, 4096, 98_765, 654_321] {
        let mut rng = Rng(seed);
        let (mut ada, root, lock) = workspace();
        let mut grace = ada.fork("grace", &[(TODO, &lock)]);
        for step in 0..60 {
            // Each writes on its own, and now and then they meet.
            if rng.below(2) == 0 {
                random_change(&mut rng, &mut ada, &root, step);
            } else {
                random_change(&mut rng, &mut grace, &root, step);
            }
            if rng.below(5) == 0 {
                ada.sync_with(&mut grace);
            }
            if step % 10 == 9 {
                assert_eq!(
                    kept(&ada),
                    anew(&ada, &lock),
                    "ada, seed {seed} step {step}"
                );
                assert_eq!(
                    kept(&grace),
                    anew(&grace, &lock),
                    "grace, seed {seed} step {step}"
                );
            }
        }
        ada.sync_with(&mut grace);
        assert_eq!(kept(&ada), kept(&grace), "seed {seed}: the replicas agree");
        assert_eq!(kept(&ada), anew(&ada, &lock), "seed {seed}");
    }
}

#[test]
fn what_the_store_writes_reads_back_before_the_record_holds_it() {
    let (mut ada, root, lock) = workspace();
    for step in 0..4 {
        let action = plan(
            &ada,
            "add",
            json!({ "text": format!("task {step}") }),
            &root,
        );
        ada.apply(&action);
    }
    let meta = ada.host.packages()[0].meta.clone();
    let items_field = field_id(&meta, "TodoList", "items");
    let list_ty = meta
        .types
        .iter()
        .find(|t| t.name == "TodoList")
        .unwrap()
        .id
        .clone();
    let item_ty = meta
        .types
        .iter()
        .find(|t| t.name == "Item")
        .unwrap()
        .id
        .clone();
    let list = ResourceRef::root(&root, list_ty);
    let listed = |replica: &Replica| -> Vec<String> {
        let spec = tilolang_runtime::store::FieldSpec {
            id: items_field.clone(),
            kind: tilolang_runtime::meta::FieldKind::Collection,
        };
        tilolang_runtime::store::Store::listing(replica.host.store(), &list, &spec)
            .unwrap()
            .ids()
    };
    // Moved here, not yet committed: the store's listing has it first already.
    let moved = items(&ada, &root).last().cloned().unwrap();
    let move_first = meta
        .actions
        .iter()
        .find(|a| a.name == "move_first")
        .unwrap();
    ada.host
        .runtime()
        .invoke_action(
            &Principal::new("ada"),
            (
                move_first.id.as_str(),
                vec![
                    Value::resource(&list),
                    Value::resource(&list.item(&moved, item_ty)),
                ],
            ),
        )
        .unwrap();
    assert_eq!(listed(&ada).first(), Some(&moved));
    ada.commit();
    assert_eq!(listed(&ada).first(), Some(&moved));
    assert_eq!(listed(&ada), items(&ada, &root));
    assert_eq!(kept(&ada), anew(&ada, &lock));
}

/// A directory of the test's own, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tilolang-index-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A host over `doc` keeping its indexes in `dir`, with the todo package loaded and its index
/// built or caught up.
fn kept_in(dir: &Path, doc: &AutoCommit, lock: &str) -> Host {
    let host = Host::open(
        Opening {
            record: Some(doc),
            chunks: None,
            index_dir: Some(dir.to_owned()),
            device: None,
        },
        Principal::new("ada"),
        || None,
    )
    .unwrap();
    host.load_builtin(TODO, lock).unwrap();
    host.wait_index();
    assert!(host.index_ready());
    host
}

#[test]
fn an_index_stamped_with_older_heads_catches_up_with_the_record() {
    let (mut ada, root, lock) = workspace();
    for step in 0..5 {
        let action = plan(
            &ada,
            "add",
            json!({ "text": format!("task {step}") }),
            &root,
        );
        ada.apply(&action);
    }
    let dir = TempDir::new("stale");
    let first = kept_in(dir.path(), &ada.doc, &lock);
    // The host's own index and the package's.
    assert_eq!(first.store().stats().builds, 2, "built the first time");
    drop(first);

    // The workspace moves on while no window keeps the index.
    let mut rng = Rng(42);
    for step in 0..20 {
        random_change(&mut rng, &mut ada, &root, step);
    }
    let again = kept_in(dir.path(), &ada.doc, &lock);
    let stats = again.store().stats();
    assert_eq!(stats.builds, 0, "not built again");
    // The host's own index and the package's.
    assert_eq!(stats.catch_ups, 2, "caught up from its heads");
    assert_eq!(
        again.store().index_contents(APP).unwrap(),
        anew(&ada, &lock)
    );
    drop(again);

    // Opened again over the same record: nothing to catch up, nothing read of the record.
    let third = kept_in(dir.path(), &ada.doc, &lock);
    let stats = third.store().stats();
    assert_eq!((stats.builds, stats.catch_ups), (0, 0));
    assert_eq!(third.store().read_stats().doc.entries, 0);
}

/// Every text `node` and the nodes inside it show.
fn texts_of(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
    out.extend(node.props.values().filter_map(|value| match value {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        texts_of(child, out);
    }
}

#[test]
fn a_store_without_the_record_opens_over_the_index_and_takes_the_record_later() {
    let (mut ada, root, lock) = workspace();
    for step in 0..5 {
        let action = plan(
            &ada,
            "add",
            json!({ "text": format!("task {step}") }),
            &root,
        );
        ada.apply(&action);
    }
    let dir = TempDir::new("detached");
    drop(kept_in(dir.path(), &ada.doc, &lock));
    // A peer's change the index does not hold yet.
    let action = plan(&ada, "add", json!({ "text": "late" }), &root);
    ada.apply(&action);

    let host = Host::open(
        Opening {
            record: None,
            chunks: None,
            index_dir: Some(dir.path().to_owned()),
            device: None,
        },
        Principal::new("ada"),
        || None,
    )
    .unwrap();
    host.load_builtin(TODO, &lock).unwrap();
    assert!(host.index_ready());
    assert!(!host.store().has_record());
    let mut view = host.open_main(APP).unwrap();
    assert_eq!(host.store().read_stats().doc.entries, 0);
    // The rows the index holds, before the record is had.
    let shown = |view: &tilolang_runtime::ViewInstance| {
        let mut texts = Vec::new();
        texts_of(view.tree(), &mut texts);
        texts
    };
    assert!(
        shown(&view).contains(&"task 4".to_owned()),
        "{:?}",
        shown(&view)
    );
    assert!(!shown(&view).contains(&"late".to_owned()));
    // The record arrives: the index catches up and the view shows what it holds.
    host.sync_from(&ada.doc);
    view.poll_patches().unwrap();
    assert!(
        shown(&view).contains(&"late".to_owned()),
        "{:?}",
        shown(&view)
    );
    assert_eq!(host.store().index_contents(APP).unwrap(), anew(&ada, &lock));
}

#[test]
fn a_window_opens_over_the_index_an_earlier_window_kept() {
    // The window that closed wrote its index to the disk as it ended: the next window of the
    // workspace opens over it, which is read, and nothing is derived from the record again.
    let (ada, root, lock) = workspace();
    let dir = TempDir::new("kept-open");
    let first = kept_in(dir.path(), &ada.doc, &lock);
    drop(first);
    let second = Host::open(
        Opening {
            record: Some(&ada.doc),
            chunks: None,
            index_dir: Some(dir.path().to_owned()),
            device: None,
        },
        Principal::new("ada"),
        || None,
    )
    .unwrap();
    second.load_builtin(TODO, &lock).unwrap();
    assert!(
        second.index_ready(),
        "the second window reads the kept index"
    );
    assert_eq!(second.store().stats().builds, 0, "nothing is built anew");
    assert_eq!(
        second.store().index_contents(APP).unwrap(),
        anew(&ada, &lock)
    );
    let _ = root;
}

#[test]
fn a_window_whose_index_is_not_kept_builds_it_off_the_frame() {
    // A window with no storage, or one whose index file another window holds, opens over an
    // index built in memory: the open does not wait for it, the frame after it is built shows
    // what it holds.
    let (ada, root, lock) = workspace();
    let host = Host::open(
        Opening {
            record: Some(&ada.doc),
            chunks: None,
            index_dir: None,
            device: None,
        },
        Principal::new("ada"),
        || None,
    )
    .unwrap();
    // What an instance does before the app's package loads.
    host.set_index_waker(Arc::new(|| {}));
    host.load_builtin(TODO, &lock).unwrap();
    assert!(!host.index_ready(), "the open does not wait for the index");
    assert!(
        host.store().index_contents(APP).is_none(),
        "nothing reads an index that is not built"
    );
    host.wait_index();
    assert!(host.index_ready(), "the window's frame takes it in");
    assert_eq!(host.store().index_contents(APP).unwrap(), anew(&ada, &lock));
    let _ = root;
}

#[test]
fn a_corrupt_index_is_built_anew_and_never_read() {
    let (mut ada, root, lock) = workspace();
    for text in ["milk", "eggs", "oat milk"] {
        let action = plan(&ada, "add", json!({ "text": text }), &root);
        ada.apply(&action);
    }
    let meta = ada.host.packages()[0].meta.clone();
    let text_field = field_id(&meta, "Item", "text");
    let item_ty = meta
        .types
        .iter()
        .find(|t| t.name == "Item")
        .unwrap()
        .id
        .clone();
    let list_ty = meta
        .types
        .iter()
        .find(|t| t.name == "TodoList")
        .unwrap()
        .id
        .clone();
    let oat = items(&ada, &root)[2].clone();
    let at = ResourceRef::root(&root, list_ty).item(&oat, item_ty);

    // A value whose bytes changed on disk.
    let dir = TempDir::new("corrupt");
    drop(kept_in(dir.path(), &ada.doc, &lock));
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().contains("so.tilo.todo"))
        .unwrap();
    let mut bytes = std::fs::read(&file).unwrap();
    let at_text = bytes
        .windows(8)
        .position(|window| window == b"oat milk")
        .expect("the text is in the file");
    bytes[at_text] = b'x';
    std::fs::write(&file, &bytes).unwrap();

    let host = kept_in(dir.path(), &ada.doc, &lock);
    let read = || host.runtime().read(&at, &text_field).unwrap();
    // Never what the damaged bytes say: the record answers while the index is not trusted.
    assert_eq!(read(), Value::text("oat milk"));
    host.wait_index();
    assert!(host.index_ready());
    assert_eq!(host.store().stats().builds, 1, "built anew");
    assert_eq!(read(), Value::text("oat milk"));
    assert_eq!(host.store().index_contents(APP).unwrap(), anew(&ada, &lock));

    // A file that is not an index at all.
    drop(host);
    std::fs::write(&file, b"not an index").unwrap();
    let host = kept_in(dir.path(), &ada.doc, &lock);
    assert_eq!(host.store().stats().builds, 1);
    assert_eq!(host.store().index_contents(APP).unwrap(), anew(&ada, &lock));
}

/// The titles of every todo-package hit a window publishes, sorted.
fn published(host: &Host) -> Vec<String> {
    let mut out: Vec<String> = tilolang_tilo::search::every_hit(host)
        .into_iter()
        .filter(|hit| {
            hit.resource
                .starts_with(&tilolang_tilo::publish::resource_id(APP, ""))
        })
        .map(|hit| hit.title)
        .collect();
    out.sort();
    out
}

#[test]
fn a_window_opened_over_its_index_publishes_every_hit_and_each_commit_at_once() {
    let (mut ada, root, lock) = workspace();
    for step in 0..5 {
        let action = plan(
            &ada,
            "add",
            json!({ "text": format!("task {step}") }),
            &root,
        );
        ada.apply(&action);
    }
    let dir = TempDir::new("published");
    drop(kept_in(dir.path(), &ada.doc, &lock));

    // A window opens over the index it kept: every hit is read from it, nothing built first.
    let host = Host::open(
        Opening {
            record: Some(&ada.doc),
            chunks: None,
            index_dir: Some(dir.path().to_owned()),
            device: None,
        },
        Principal::new("ada"),
        || None,
    )
    .unwrap();
    host.load_builtin(TODO, &lock).unwrap();
    assert!(host.index_ready());
    assert_eq!(
        published(&host),
        ["task 0", "task 1", "task 2", "task 3", "task 4"]
    );
    assert_eq!(
        host.store().read_stats().doc.entries,
        0,
        "the record not walked"
    );

    // A change committed to the workspace is published as soon as the window has it.
    let action = plan(&ada, "add", json!({ "text": "late arrival" }), &root);
    ada.apply(&action);
    host.sync_from(&ada.doc);
    assert!(published(&host).contains(&"late arrival".to_owned()));
    assert_eq!(host.store().index_contents(APP).unwrap(), anew(&ada, &lock));
}

/// A window's mark counts a page of the resources of its document, and says when there are
/// more: what a workspace of any size reads of them, never all of it, and of each resource
/// only its newest version.
#[test]
fn a_documents_newest_versions_are_read_a_page_at_a_time() {
    let (mut ada, root, _lock) = workspace();
    for step in 0..5 {
        let action = plan(
            &ada,
            "add",
            json!({ "text": format!("task {step}") }),
            &root,
        );
        ada.apply(&action);
    }
    ada.commit();
    let doc = ResourceRef::parse_id(&root, String::new()).unwrap().doc;
    let store = ada.host.store();

    let (page, more) = store.newest_page(&doc, 3).unwrap();
    assert_eq!(page.len(), 3, "a page of them, not the document");
    assert!(more, "the document holds more than the page");
    let (all, more) = store.newest_page(&doc, 100).unwrap();
    assert!(!more, "a page the document fits in holds it all");
    assert!(all.len() > 3, "the document holds a list and its items");
    for (resource, version) in &all {
        assert_eq!(&version.resource, resource, "a version of what it names");
        assert!(!version.change.is_empty(), "{resource} was changed");
    }
    // The newest version of an item is the one the page reads of it.
    let item = ResourceRef::parse_id(&all[1].0, String::new()).unwrap();
    assert_eq!(store.newest(&item).unwrap(), Some(all[1].1.clone()));
}

/// How many maps the document keeps at `path`, counting only those that hold something: a fold
/// empties the copies it merges, and leaves them where they are.
fn maps_holding(replica: &Replica, path: &[&str]) -> usize {
    use automerge::ReadDoc as _;
    tilolang_tilo::containers::reach(&replica.doc, &automerge::ROOT, path)
        .iter()
        .filter(|held| replica.doc.keys(held).next().is_some())
        .count()
}

/// The ids of the records of the app the workspace holds.
fn ids(replica: &Replica) -> Vec<String> {
    tilolang_tilo::action::resource_ids(&replica.doc, APP)
}

/// A record of the todo list `id`, made by the store as a window's creation makes one.
fn create(replica: &mut Replica, id: &str) {
    let list = type_id(
        &replica
            .host
            .packages()
            .iter()
            .find(|package| package.meta.app == APP)
            .expect("the app is open")
            .meta,
        "TodoList",
    );
    replica
        .host
        .store()
        .create_with_id(
            &tilolang_runtime::store::NewRecord {
                ty: list,
                app: APP.to_owned(),
                fields: Vec::new(),
            },
            id,
        )
        .unwrap();
    replica.commit();
}

/// Each replica founds the workspace with the first record of the app it makes, before the two
/// meet: each made the workspace's `tl` map and the app's map of its own. Both records are the
/// app's on both, and the index holds what deriving it anew holds; the next write keeps one map
/// of each, on both.
#[test]
fn the_first_record_each_replica_made_of_a_new_workspace_is_the_apps_on_both() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let lock = packages[0].ids.clone();
    let mut grace = Replica::new("grace", AutoCommit::new(), &[(TODO, &lock)]).0;
    // Each device opens the app and makes its first record on it, offline.
    ada.host.root_of(APP).unwrap();
    ada.commit();
    create(&mut ada, "ada-list");
    grace.host.root_of(APP).unwrap();
    grace.commit();
    create(&mut grace, "grace-list");
    for (name, replica) in [("ada", &ada), ("grace", &grace)] {
        assert_eq!(
            maps_holding(replica, &["tl"]),
            1,
            "{name}: a map of its own"
        );
        assert_eq!(
            maps_holding(replica, &["tl", APP]),
            1,
            "{name}: an app map of its own"
        );
    }
    ada.sync_with(&mut grace);
    for (name, replica) in [("ada", &ada), ("grace", &grace)] {
        assert_eq!(maps_holding(replica, &["tl"]), 2, "{name}: two maps met");
        for id in ["ada-list", "grace-list"] {
            assert!(
                tilolang_tilo::reads::resource_obj(&replica.doc, APP, id).is_some(),
                "{name}: {id} is one of the app's records"
            );
            assert!(
                replica.host.store().record(id).is_ok(),
                "{name}: {id} reads"
            );
        }
        assert_eq!(
            ids(replica).len(),
            3,
            "{name}: both lists and the app's root"
        );
        assert_eq!(kept(replica), anew(replica, &lock), "{name}: the index");
    }

    // A write on one side keeps one map of each, with both records in it, on both.
    create(&mut grace, "later");
    ada.sync_with(&mut grace);
    for (name, replica) in [("ada", &ada), ("grace", &grace)] {
        assert_eq!(maps_holding(replica, &["tl"]), 1, "{name}: one map");
        assert_eq!(
            maps_holding(replica, &["tl", APP]),
            1,
            "{name}: one app map"
        );
        let held = ids(replica);
        for id in ["ada-list", "grace-list", "later"] {
            assert!(held.iter().any(|held| held == id), "{name}: {id} stays");
            assert!(replica.host.store().record(id).is_ok(), "{name}: {id}");
        }
        assert_eq!(kept(replica), anew(replica, &lock), "{name}: the index");
    }
}

/// The same for the app's map alone: the workspace's `tl` map is one, and each replica makes
/// the app's first record of its own, so the two app maps met.
#[test]
fn the_first_record_each_replica_made_of_a_new_app_is_the_apps_on_both() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(COUNTER, ""), (TODO, "")]);
    let counter = packages[0].ids.clone();
    let lock = packages[1].ids.clone();
    // The workspace holds the counter's records; the todo app's map is made by the first record
    // of it, which each device makes on its own.
    ada.host.root_of(APP_COUNTER).unwrap();
    ada.commit();
    let mut grace = ada.fork("grace", &[(COUNTER, &counter), (TODO, &lock)]);
    create(&mut ada, "ada-list");
    create(&mut grace, "grace-list");
    assert_eq!(maps_holding(&ada, &["tl"]), 1, "one workspace map");
    assert_eq!(
        maps_holding(&grace, &["tl"]),
        1,
        "one workspace map on grace's side too"
    );
    ada.sync_with(&mut grace);
    for (name, replica) in [("ada", &ada), ("grace", &grace)] {
        assert_eq!(maps_holding(replica, &["tl"]), 1, "{name}: one map");
        assert_eq!(
            maps_holding(replica, &["tl", APP]),
            2,
            "{name}: each replica made the app's map"
        );
        for id in ["ada-list", "grace-list"] {
            assert!(
                tilolang_tilo::reads::resource_obj(&replica.doc, APP, id).is_some(),
                "{name}: {id} is one of the app's records"
            );
            assert!(
                replica.host.store().record(id).is_ok(),
                "{name}: {id} reads"
            );
        }
        assert_eq!(kept(replica), anew(replica, &lock), "{name}: the index");
    }

    create(&mut ada, "later");
    ada.sync_with(&mut grace);
    for (name, replica) in [("ada", &ada), ("grace", &grace)] {
        assert_eq!(
            maps_holding(replica, &["tl", APP]),
            1,
            "{name}: the app's maps are one"
        );
        let held = ids(replica);
        for id in ["ada-list", "grace-list", "later"] {
            assert!(held.iter().any(|held| held == id), "{name}: {id} stays");
            assert!(replica.host.store().record(id).is_ok(), "{name}: {id}");
        }
        assert_eq!(kept(replica), anew(replica, &lock), "{name}: the index");
    }
}
