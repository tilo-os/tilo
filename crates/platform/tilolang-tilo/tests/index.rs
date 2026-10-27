//! The index the store reads lists, filters and search from: kept change by change, it holds
//! what deriving it from the record anew holds, whatever replicas wrote and in which order; an
//! index stamped with older heads catches up; a corrupt one is built anew and never read.

use std::path::{Path, PathBuf};

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Decision, Principal};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::host::{Host, Opening};
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::record::DynValue;

use crate::fixtures::{Replica, TODO, field_id};

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
                            Segment::Field(items_field),
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
                let item = ResourceRef::root(root, list_ty.id.clone()).item(
                    &items_field,
                    &id,
                    item_ty.id.clone(),
                );
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
                edit(
                    replica,
                    root,
                    Edit::new(
                        vec![Segment::Field(items_field)],
                        EditOp::RemoveItem {
                            id: Id::from_raw(&id),
                        },
                    ),
                );
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

/// Ada's room with a list, and the package's ids lock.
fn room() -> (Replica, String, String) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let lock = packages[0].ids.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    (ada, root.doc, lock)
}

#[test]
fn an_index_kept_change_by_change_holds_what_deriving_it_anew_holds() {
    for seed in [7_u64, 1234, 98_765] {
        let mut rng = Rng(seed);
        let (mut ada, root, lock) = room();
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
    let (mut ada, root, lock) = room();
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
                    Value::resource(&list.item(&items_field, &moved, item_ty)),
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
            index_dir: Some(dir.to_owned()),
            device: None,
        },
        Principal::new("ada"),
        |_, _| Decision::Allow,
    )
    .unwrap();
    host.load_builtin(TODO, lock).unwrap();
    host.wait_index();
    assert!(host.index_ready());
    host
}

#[test]
fn an_index_stamped_with_older_heads_catches_up_with_the_record() {
    let (mut ada, root, lock) = room();
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

    // The room moves on while no window keeps the index.
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
    let (mut ada, root, lock) = room();
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
            index_dir: Some(dir.path().to_owned()),
            device: None,
        },
        Principal::new("ada"),
        |_, _| Decision::Allow,
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
fn a_corrupt_index_is_built_anew_and_never_read() {
    let (mut ada, root, lock) = room();
    for text in ["milk", "eggs", "oat milk"] {
        let action = plan(&ada, "add", json!({ "text": text }), &root);
        ada.apply(&action);
    }
    let meta = ada.host.packages()[0].meta.clone();
    let items_field = field_id(&meta, "TodoList", "items");
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
    let at = ResourceRef::root(&root, list_ty).item(&items_field, &oat, item_ty);

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
