//! Search and publication follow the room by what changed: one retyped item is reindexed and
//! republished alone, and what they hold always equals indexing and publishing everything anew.

use std::sync::Arc;

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilo_actions::Resource;
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::publish::{
    ActionTypes, PlanRequest, Planner, Publications, publish_tree, resource_id,
};
use tilolang_tilo::record::DynValue;
use tilolang_tilo::search::{SearchIndex, hits};

use crate::fixtures::{Replica, TODO, field_id};

const APP: &str = "so.tilo.todo";

fn add(replica: &mut Replica, meta: &tilolang_runtime::Meta, root: &str, text: &str) {
    let action = meta.actions.iter().find(|a| a.name == "add").unwrap();
    let args = json!({ "text": text }).as_object().cloned().unwrap();
    let planned = replica
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: "tl.so.tilo.todo.TodoList",
            action_id: &action.id,
            resource: Some(root),
            args: &args,
            caller: None,
        })
        .unwrap();
    replica.apply(&planned);
}

fn retype(
    replica: &mut Replica,
    meta: &tilolang_runtime::Meta,
    root: &str,
    item: &str,
    text: &str,
) {
    let edit = Edit::new(
        vec![
            Segment::Field(field_id(meta, "TodoList", "items")),
            Segment::Item(Id::from_raw(item)),
            Segment::Field(field_id(meta, "Item", "text")),
        ],
        EditOp::Replace {
            text: text.to_owned(),
        },
    );
    replica.apply(&DynAction {
        app: APP.to_owned(),
        kind: "tl.so.tilo.todo.TodoList".to_owned(),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: root.to_owned(),
            edit,
        }],
    });
}

fn item_ids(replica: &Replica, meta: &tilolang_runtime::Meta, root: &str) -> Vec<String> {
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

/// Ada's list with three items.
fn room() -> (Replica, tilolang_runtime::Meta, String) {
    let (ada, meta, root, _) = room_with_ids();
    (ada, meta, root)
}

/// As [`room`], with the package's ids lock, for a peer to load the same package.
fn room_with_ids() -> (Replica, tilolang_runtime::Meta, String, String) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let meta = packages[0].meta.clone();
    let ids = packages[0].ids.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    for text in ["milk", "eggs", "bread"] {
        add(&mut ada, &meta, &root.doc, text);
    }
    (ada, meta, root.doc, ids)
}

/// Everything search would find, indexed anew.
fn all_hits(replica: &Replica, meta: &tilolang_runtime::Meta, root: &str) -> Vec<String> {
    let record = replica.host.store().record(root).unwrap();
    let mut out: Vec<String> = hits(meta, &record, root)
        .into_iter()
        .map(|hit| format!("{} {:?}", hit.resource, hit.texts))
        .collect();
    out.sort();
    out
}

fn indexed(index: &SearchIndex) -> Vec<String> {
    let mut out: Vec<String> = index
        .hits()
        .map(|hit| format!("{} {:?}", hit.resource, hit.texts))
        .collect();
    out.sort();
    out
}

#[test]
fn search_reindexes_only_the_retyped_item() {
    let (mut ada, meta, root) = room();
    let mut index = SearchIndex::new(&ada.host);
    let first = index.settle(&ada.host);
    assert_eq!(first.upserted.len(), 3, "{first:?}");
    assert!(index.update(&ada.host).is_empty(), "nothing changed");

    let ids = item_ids(&ada, &meta, &root);
    retype(&mut ada, &meta, &root, &ids[1], "oat milk");
    let delta = index.update(&ada.host);
    assert_eq!(delta.removed, Vec::<String>::new());
    assert_eq!(
        delta
            .upserted
            .iter()
            .map(|hit| hit.resource.clone())
            .collect::<Vec<_>>(),
        vec![resource_id(
            APP,
            &format!("{root}/{}:{}", field_id(&meta, "TodoList", "items"), ids[1])
        )],
    );
    assert_eq!(delta.upserted[0].title, "oat milk");

    add(&mut ada, &meta, &root, "jam");
    let delta = index.update(&ada.host);
    assert_eq!(delta.upserted.len(), 1, "{delta:?}");
    assert_eq!(delta.upserted[0].title, "jam");

    // Emptied, an item has nothing to be found by.
    retype(&mut ada, &meta, &root, &ids[0], "");
    let delta = index.update(&ada.host);
    assert_eq!(delta.removed.len(), 1, "{delta:?}");
    assert_eq!(indexed(&index), all_hits(&ada, &meta, &root));
}

#[test]
fn search_follows_a_peer_and_matches_a_full_index() {
    let (mut ada, meta, root, lock) = room_with_ids();
    let mut grace = ada.fork("grace", &[(TODO, &lock)]);
    let mut index = SearchIndex::new(&ada.host);
    index.settle(&ada.host);
    let ids = item_ids(&ada, &meta, &root);
    retype(&mut grace, &meta, &root, &ids[2], "rye bread");
    add(&mut grace, &meta, &root, "tea");
    ada.sync_with(&mut grace);
    let delta = index.update(&ada.host);
    assert_eq!(delta.upserted.len(), 2, "{delta:?}");
    assert_eq!(indexed(&index), all_hits(&ada, &meta, &root));
}

struct Refusing;

impl Planner for Refusing {
    fn plan(&self, _: &PlanRequest<'_>) -> Result<DynAction, String> {
        Err("not in this test".into())
    }
}

/// A published tree as text: ids, kinds, labels and action names, children in order.
fn shape(resource: &Resource) -> String {
    let actions: Vec<String> = resource.actions.iter().map(|a| a.id.clone()).collect();
    let children: Vec<String> = resource.children.iter().map(shape).collect();
    format!(
        "{}|{}|{}|{actions:?}[{}]",
        resource.id.as_str(),
        resource.kind,
        resource.label,
        children.join(",")
    )
}

fn published_anew(replica: &Replica, meta: &tilolang_runtime::Meta, root: &str) -> String {
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let record = replica.host.store().record(root).unwrap();
    let label = replica.host.label_of(root);
    shape(&publish_tree(
        &ActionTypes::new(meta, &planner),
        &record,
        "TodoList",
        root,
        &label,
    ))
}

#[test]
fn publication_rebuilds_the_changed_item_and_matches_publishing_anew() {
    let (mut ada, meta, root) = room();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    assert_eq!(published.settle(&ada.host, &planner), vec![root.clone()]);
    assert!(
        published.update(&ada.host, &planner).is_empty(),
        "nothing changed"
    );
    assert_eq!(
        shape(&published.trees()[&root]),
        published_anew(&ada, &meta, &root)
    );

    let ids = item_ids(&ada, &meta, &root);
    retype(&mut ada, &meta, &root, &ids[0], "almond milk");
    assert_eq!(published.update(&ada.host, &planner), vec![root.clone()]);
    assert_eq!(
        shape(&published.trees()[&root]),
        published_anew(&ada, &meta, &root)
    );

    add(&mut ada, &meta, &root, "jam");
    let first = item_ids(&ada, &meta, &root)[3].clone();
    let move_first = meta
        .actions
        .iter()
        .find(|a| a.name == "move_first")
        .unwrap();
    let items = field_id(&meta, "TodoList", "items");
    let args = json!({ "i": format!("{root}/{items}:{first}") })
        .as_object()
        .cloned()
        .unwrap();
    let moved = ada
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: "tl.so.tilo.todo.TodoList",
            action_id: &move_first.id,
            resource: Some(&root),
            args: &args,
            caller: None,
        })
        .unwrap();
    ada.apply(&moved);
    published.update(&ada.host, &planner);
    assert_eq!(
        shape(&published.trees()[&root]),
        published_anew(&ada, &meta, &root)
    );
}

/// Search and publication starting in one frame read one copy of the mirror; a change makes
/// the next reader take a new one.
#[test]
fn readers_share_a_snapshot_until_the_mirror_changes() {
    let (mut ada, meta, root) = room();
    let first = ada.host.store().snapshot();
    assert!(Arc::ptr_eq(&first, &ada.host.store().snapshot()));
    let ids = item_ids(&ada, &meta, &root);
    retype(&mut ada, &meta, &root, &ids[0], "oat milk");
    let after = ada.host.store().snapshot();
    assert!(
        !Arc::ptr_eq(&first, &after),
        "a snapshot of the mirror before the change"
    );
    let list_type = meta
        .types
        .iter()
        .find(|t| t.name == "TodoList")
        .unwrap()
        .id
        .clone();
    let list = tilolang_runtime::value::ResourceRef::root(&root, list_type);
    assert_eq!(
        after.first_text(&list),
        ada.host.store().first_text(&list),
        "the new snapshot holds the change"
    );
}

/// What names a record is its first text, in field id order and a collection's items in order.
/// A republish reads it again only where a change can move it, and gets it right each time.
#[test]
fn the_label_follows_whatever_names_the_record() {
    let (mut ada, meta, root) = room();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    let label = |published: &Publications| published.trees()[&root].label.clone();
    assert_eq!(label(&published), "milk");

    let ids = item_ids(&ada, &meta, &root);
    let check = |ada: &Replica, published: &Publications, step: &str| {
        assert_eq!(
            shape(&published.trees()[&root]),
            published_anew(ada, &meta, &root),
            "after {step}"
        );
    };
    retype(&mut ada, &meta, &root, &ids[0], "almond milk");
    published.update(&ada.host, &planner);
    check(&ada, &published, "retyping the item that names the list");
    assert_eq!(label(&published), "almond milk");

    retype(&mut ada, &meta, &root, &ids[0], "");
    published.update(&ada.host, &planner);
    check(&ada, &published, "emptying it");
    assert_eq!(label(&published), "eggs", "the next item names the list");

    retype(&mut ada, &meta, &root, &ids[2], "rye bread");
    published.update(&ada.host, &planner);
    check(
        &ada,
        &published,
        "retyping an item after the one that names it",
    );

    let tag = meta.actions.iter().find(|a| a.name == "tag").unwrap();
    let args = json!({ "t": "groceries" }).as_object().cloned().unwrap();
    let tagged = ada
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: "tl.so.tilo.todo.TodoList",
            action_id: &tag.id,
            resource: Some(&root),
            args: &args,
            caller: None,
        })
        .unwrap();
    ada.apply(&tagged);
    published.update(&ada.host, &planner);
    check(&ada, &published, "tagging the list");

    let title = Edit::new(
        vec![Segment::Field(field_id(&meta, "TodoList", "title"))],
        EditOp::Replace {
            text: "Shopping".to_owned(),
        },
    );
    ada.apply(&DynAction {
        app: APP.to_owned(),
        kind: "tl.so.tilo.todo.TodoList".to_owned(),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: root.clone(),
            edit: title,
        }],
    });
    published.update(&ada.host, &planner);
    check(&ada, &published, "titling the list");
}

#[test]
fn planning_an_action_changes_nothing_anyone_follows() {
    let (ada, meta, root) = room();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut index = SearchIndex::new(&ada.host);
    index.settle(&ada.host);
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    let mut view = ada.view(APP);
    view.poll_patches().unwrap();
    // Running a published action plans it first: a trial run, taken back.
    let action = meta.actions.iter().find(|a| a.name == "add").unwrap();
    let args = json!({ "text": "tea" }).as_object().cloned().unwrap();
    ada.host
        .plan(&PlanRequest {
            app: APP,
            kind: "tl.so.tilo.todo.TodoList",
            action_id: &action.id,
            resource: Some(&root),
            args: &args,
            caller: None,
        })
        .unwrap();
    assert!(index.update(&ada.host).is_empty());
    assert!(published.update(&ada.host, &planner).is_empty());
    assert!(!view.is_stale(), "no view re-runs for a trial run");
    assert!(view.poll_patches().unwrap().is_empty());
}

#[test]
fn the_first_index_and_publication_do_not_hold_up_the_frame() {
    let (mut ada, meta, root) = room();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut index = SearchIndex::new(&ada.host);
    let mut published = Publications::new(&ada.host);
    // The frame that starts them returns at once; the room is read on another thread.
    assert!(index.update(&ada.host).is_empty());
    assert!(index.is_reading());
    assert!(published.update(&ada.host, &planner).is_empty());
    assert!(published.is_building());
    // A change while they read is taken in after what they read.
    let ids = item_ids(&ada, &meta, &root);
    retype(&mut ada, &meta, &root, &ids[1], "oat milk");
    index.settle(&ada.host);
    published.settle(&ada.host, &planner);
    assert!(!index.is_reading() && !published.is_building());
    assert_eq!(indexed(&index), all_hits(&ada, &meta, &root));
    assert_eq!(
        shape(&published.trees()[&root]),
        published_anew(&ada, &meta, &root)
    );
}

/// A planner that counts what it is asked to plan.
#[derive(Default)]
struct Counting(std::sync::atomic::AtomicUsize);

impl Planner for Counting {
    fn plan(&self, _: &PlanRequest<'_>) -> Result<DynAction, String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err("counted".into())
    }
}

#[test]
fn publishing_plans_no_action_until_one_runs() {
    let (ada, _meta, root) = room();
    let counting = Arc::new(Counting::default());
    let planner: Arc<dyn Planner> = counting.clone();
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    let plans = || counting.0.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        plans(),
        0,
        "publishing every item planned none of their actions"
    );
    // An action without parameters is built when it runs.
    let tree = &published.trees()[&root];
    let item = &tree.children[0];
    let action = item
        .actions
        .iter()
        .find(|a| matches!(a.input(), tilo_actions::ActionInput::Deferred { .. }))
        .expect("the item's `finish`, deferred");
    assert!(action.build_deferred().is_some());
    assert_eq!(plans(), 1);
}
