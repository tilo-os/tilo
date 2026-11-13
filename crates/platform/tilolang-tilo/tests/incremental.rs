//! Search and publication follow the workspace by what changed: one retyped item is reindexed and
//! republished alone, and what they hold always equals indexing and publishing everything anew.

use std::rc::Rc;
use std::sync::Arc;

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilo_actions::{ResourceSpec, Target};
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_runtime::SourceWords;
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::publish::{
    ActionTypes, PlanRequest, Planner, Publications, publish_tree, resource_id,
};
use tilolang_tilo::record::DynValue;
use tilolang_tilo::search::{every_hit, hits};

use crate::fixtures::{COUNTER, Replica, TODO, field_id};

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
            from: None,
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
fn workspace() -> (Replica, tilolang_runtime::Meta, String) {
    let (ada, meta, root, _) = workspace_with_ids();
    (ada, meta, root)
}

/// As [`workspace`], with the package's ids lock, for a peer to load the same package.
fn workspace_with_ids() -> (Replica, tilolang_runtime::Meta, String, String) {
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
    let mut out: Vec<String> = hits(meta, &record, root, replica.host.store())
        .into_iter()
        .map(|hit| format!("{} {:?}", hit.resource, hit.texts))
        .collect();
    out.sort();
    out
}

/// Everything search finds, read from the index.
fn indexed(replica: &Replica) -> Vec<String> {
    let mut out: Vec<String> = every_hit(&replica.host)
        .into_iter()
        .filter(|hit| hit.resource.starts_with(&resource_id(APP, "")))
        .map(|hit| format!("{} {:?}", hit.resource, hit.texts))
        .collect();
    out.sort();
    out
}

#[test]
fn search_reads_the_index_the_lists_read() {
    let (mut ada, meta, root) = workspace();
    assert_eq!(indexed(&ada), all_hits(&ada, &meta, &root));

    let ids = item_ids(&ada, &meta, &root);
    retype(&mut ada, &meta, &root, &ids[1], "oat milk");
    assert_eq!(indexed(&ada), all_hits(&ada, &meta, &root));

    add(&mut ada, &meta, &root, "jam");
    assert_eq!(indexed(&ada), all_hits(&ada, &meta, &root));

    // Emptied, an item has nothing to be found by.
    retype(&mut ada, &meta, &root, &ids[0], "");
    assert_eq!(indexed(&ada), all_hits(&ada, &meta, &root));
}

#[test]
fn search_follows_a_peer_and_matches_a_full_index() {
    let (mut ada, meta, root, lock) = workspace_with_ids();
    let mut grace = ada.fork("grace", &[(TODO, &lock)]);
    let ids = item_ids(&ada, &meta, &root);
    retype(&mut grace, &meta, &root, &ids[2], "rye bread");
    add(&mut grace, &meta, &root, "tea");
    ada.sync_with(&mut grace);
    assert_eq!(indexed(&ada), all_hits(&ada, &meta, &root));

    // A peer removes one item and moves another.
    let ids = item_ids(&ada, &meta, &root);
    remove(&mut grace, &meta, &root, &ids[1]);
    let moved = plan_move_first(&grace, &meta, &root, &ids[3]);
    grace.apply(&moved);
    ada.sync_with(&mut grace);
    assert_eq!(indexed(&ada), all_hits(&ada, &meta, &root));
}

#[test]
fn a_window_publishes_what_it_reaches_and_the_whole_workspace_when_asked() {
    let (ada, meta, root) = workspace();
    let ids = item_ids(&ada, &meta, &root);
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    // This publication is a window's: it holds the rows on screen, not the workspace.
    published.window();
    published.update(&ada.host, &planner);
    // A window the turn is not drawn for publishes nothing: the workspace is not read for a
    // surface nothing shows.
    assert!(
        published.trees().is_empty(),
        "a window that showed nothing published {} records",
        published.trees().len()
    );
    // What the window shows of one item: its record, published down to it, and nothing of the
    // other two the record holds.
    let shown = [resource_id(APP, &format!("{root}/{}", ids[1]))];
    let trees = published.reached(&ada.host, &planner, &shown);
    let reached = trees.first().expect("the item's record is published");
    assert_eq!(reached.id.as_str(), resource_id(APP, &root));
    assert_eq!(
        reached
            .children
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        ["eggs"],
        "one item shown publishes one item"
    );
    assert_eq!(
        published.visited(),
        1,
        "what the window shows of three items reads one record"
    );
    // Nothing moved and the window shows what it showed: a frame reads nothing again.
    let again = published.reached(&ada.host, &planner, &shown);
    assert!(
        Rc::ptr_eq(&again[0], &trees[0]),
        "a frame that showed the same read the record again"
    );
    // A caller that asks about the whole workspace reads all of it, and publishes nothing of
    // it to the window.
    let whole = published.everything(&ada.host, &planner);
    assert_eq!(
        whole
            .iter()
            .flat_map(|record| record.children.iter())
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        ["milk", "eggs", "bread"],
        "the whole workspace is read when it is asked for"
    );
    let again = published.reached(&ada.host, &planner, &shown);
    assert_eq!(
        again[0]
            .children
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        ["eggs"],
        "what the window shows is what it still holds"
    );
}

struct Refusing;
impl Planner for Refusing {
    fn plan(&self, _: &PlanRequest<'_>) -> Result<DynAction, String> {
        Err("not in this test".into())
    }
}

/// A published tree as text: ids, kinds, labels and action names, children in order.
fn shape(resource: &ResourceSpec) -> String {
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
        &ActionTypes::new(meta, &planner, &SourceWords),
        &record,
        "TodoList",
        root,
        &label,
    ))
}

#[test]
fn an_item_offers_the_direct_edits_its_type_offers() {
    let (ada, _meta, root) = workspace();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    let tree = &published.trees()[&root];
    let edits = |resource: &ResourceSpec| -> Vec<String> {
        resource
            .actions
            .iter()
            .filter(|a| matches!(a.id.as_str(), "edit-text" | "edit-flag"))
            .map(|a| a.id.clone())
            .collect()
    };
    assert_eq!(edits(tree), ["edit-text", "edit-flag"], "a root's edits");
    let (first, second) = (&tree.children[0], &tree.children[1]);
    assert_eq!(
        edits(first),
        ["edit-text", "edit-flag"],
        "an item offers the direct edits its type's fields support, as a root does"
    );
    // The specs are shared per type: items of one type bind the same edit types to their own
    // ids, rather than each carrying specs of its own.
    for id in ["edit-text", "edit-flag"] {
        let held = |resource: &ResourceSpec| {
            resource
                .actions
                .iter()
                .find(|a| a.id == id)
                .unwrap()
                .action_type()
                .clone()
        };
        assert!(
            Arc::ptr_eq(&held(first), &held(second)),
            "{id} is one shared type, bound to each item, not rebuilt per item"
        );
    }
    // Each item's edit is its own: the target is the item's path, as an action names it.
    let target = |resource: &ResourceSpec| {
        resource
            .actions
            .iter()
            .find(|a| a.id == "edit-text")
            .unwrap()
            .target()
            .get::<String>()
            .cloned()
    };
    assert_eq!(
        target(first).as_deref(),
        Some(first.id.as_str().trim_start_matches("tl:so.tilo.todo:"))
    );
    assert_ne!(target(first), target(second), "each binds its own item");
}

#[test]
fn publication_rebuilds_the_changed_item_and_matches_publishing_anew() {
    let (mut ada, meta, root) = workspace();
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
            from: None,
        })
        .unwrap();
    ada.apply(&moved);
    published.update(&ada.host, &planner);
    assert_eq!(
        shape(&published.trees()[&root]),
        published_anew(&ada, &meta, &root)
    );

    remove(&mut ada, &meta, &root, &ids[1]);
    published.update(&ada.host, &planner);
    assert_eq!(
        shape(&published.trees()[&root]),
        published_anew(&ada, &meta, &root)
    );
}

/// A turn publishes the records it changed: the counter's record, which nothing typed into
/// the to-do list reaches, is neither read nor published again.
#[test]
fn a_turn_publishes_only_the_record_it_changed() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(COUNTER, ""), (TODO, "")]);
    let todo = packages[1].meta.clone();
    let root = ada.host.root_of(APP).unwrap().doc;
    let counter = ada.host.root_of(crate::fixtures::APP).unwrap().doc;
    ada.commit();
    add(&mut ada, &todo, &root, "milk");
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    assert_eq!(
        published.trees().len(),
        2,
        "both records of the workspace are published"
    );
    let (held, looked_at, sweeps) = (
        Rc::clone(&published.trees()[&counter]),
        published.visited(),
        published.sweeps(),
    );

    add(&mut ada, &todo, &root, "eggs");
    published.settle(&ada.host, &planner);
    assert_eq!(
        published.visited().saturating_sub(looked_at),
        1,
        "adding an item looked at more than its record"
    );
    assert_eq!(
        published.sweeps(),
        sweeps,
        "adding an item read every record of the workspace in full"
    );
    assert!(
        Rc::ptr_eq(&held, &published.trees()[&counter]),
        "a change to the to-do list published the counter's record again"
    );
}

/// Search and publication starting in one frame read one copy of the mirror; a change makes
/// the next reader take a new one.
#[test]
fn readers_share_a_snapshot_until_the_mirror_changes() {
    let (mut ada, meta, root) = workspace();
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

/// A record is published under the name its declaration says it has, and a change to a field
/// that names it (a `title`) relabels it, whatever the field's generated id sorts as: an order
/// over ids would name the record by another text on a build whose ids differ, so naming is
/// read in the order the source declares (the `title` first, a `body`'s first line next, the
/// rest as declared), never in the order the ids happen to compare.
#[test]
fn a_title_names_a_record_whatever_its_field_ids_are() {
    let (mut ada, meta, root) = workspace();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    let check = |ada: &Replica, published: &Publications, step: &str| {
        assert_eq!(
            shape(&published.trees()[&root]),
            published_anew(ada, &meta, &root),
            "after {step}"
        );
    };
    let title = |published: &Publications| published.trees()[&root].label.clone();
    check(&ada, &published, "publishing the record");

    // The title is a text of the record's own, so it names the record even though the record
    // also holds items with texts (its list's `milk`, `eggs`, `bread`).
    let (title_field, list_field) = (
        field_id(&meta, "TodoList", "title"),
        field_id(&meta, "TodoList", "items"),
    );
    assert_ne!(
        title_field, list_field,
        "the title and the list are fields of their own"
    );
    // Retype the title: the record is named by it now.
    ada.apply(&DynAction {
        app: APP.to_owned(),
        kind: "tl.so.tilo.todo.TodoList".to_owned(),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: root.clone(),
            edit: Edit::new(
                vec![Segment::Field(title_field.clone())],
                EditOp::Replace {
                    text: "Shopping".to_owned(),
                },
            ),
        }],
    });
    published.update(&ada.host, &planner);
    check(&ada, &published, "titling the list");
    assert_eq!(
        title(&published),
        "Shopping",
        "the title names the record, not its first item"
    );

    // Clear it: the record holds no title now, and its first item names it, as reading it
    // anew finds — never the id order, which would pick another text.
    ada.apply(&DynAction {
        app: APP.to_owned(),
        kind: "tl.so.tilo.todo.TodoList".to_owned(),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: root.clone(),
            edit: Edit::new(
                vec![Segment::Field(title_field)],
                EditOp::Replace {
                    text: String::new(),
                },
            ),
        }],
    });
    published.update(&ada.host, &planner);
    check(&ada, &published, "clearing the title");
    assert_eq!(
        title(&published),
        "milk",
        "with no title, the first item names the list"
    );
}

/// What names a record is a text of its own (a `title` field, or its `body`'s first line),
/// and only a record that holds none is named by its items.
/// A republish reads it again only where a change can move it, and gets it right each time.
#[test]
fn the_label_follows_whatever_names_the_record() {
    let (mut ada, meta, root) = workspace();
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
            from: None,
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

/// The published id and label of every resource in `resource`.
fn labels(resource: &ResourceSpec, out: &mut Vec<(String, String)>) {
    out.push((resource.id.as_str().to_owned(), resource.label.clone()));
    for child in &resource.children {
        labels(child, out);
    }
}

/// An item retyped moves its label, and the record's when it names it, and nothing else: a
/// reader holding an earlier revision learns those labels alone, until an item is added.
#[test]
fn a_retyped_item_moves_only_labels_and_a_reader_learns_them_alone() {
    let (mut ada, meta, root) = workspace();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    let start = published.revision();
    assert_eq!(published.labels_since(start), Some(Vec::new()));
    let now = |published: &Publications| {
        let mut out = Vec::new();
        labels(&published.trees()[&root], &mut out);
        out
    };
    let before = now(&published);
    // What moved from `before` to the tree now: what a reader holding `before` must learn.
    let moved = |published: &Publications, before: &[(String, String)]| {
        let mut out: Vec<(String, String)> = now(published)
            .into_iter()
            .filter(|label| !before.contains(label))
            .collect();
        out.sort();
        out
    };
    let learned = |published: &Publications, since: u64| {
        let mut out = published.labels_since(since).expect("only labels moved");
        out.sort();
        out
    };

    let ids = item_ids(&ada, &meta, &root);
    retype(&mut ada, &meta, &root, &ids[0], "almond milk");
    published.update(&ada.host, &planner);
    let first = moved(&published, &before);
    assert_eq!(first.len(), 2, "the item and the list it names: {first:?}");
    assert_eq!(learned(&published, start), first);

    let middle = published.revision();
    let after_first = now(&published);
    retype(&mut ada, &meta, &root, &ids[2], "rye bread");
    published.update(&ada.host, &planner);
    assert_eq!(
        learned(&published, middle),
        moved(&published, &after_first),
        "since the first retyping, only the second"
    );
    assert_eq!(learned(&published, start), moved(&published, &before));

    add(&mut ada, &meta, &root, "jam");
    published.update(&ada.host, &planner);
    assert_eq!(
        published.labels_since(start),
        None,
        "an item added is more than labels"
    );
    assert_eq!(
        published.labels_since(published.revision()),
        Some(Vec::new())
    );
}

/// Every published resource's id and whether it is its app's root.
fn roots(resource: &ResourceSpec, out: &mut Vec<(String, bool)>) {
    if let Some(named) = &resource.resource_type {
        out.push((resource.id.as_str().to_owned(), named.root));
    }
    for child in &resource.children {
        roots(child, out);
    }
}

/// A resource says what it is as it is published: its type's noun, its app's name, and
/// whether it is the app's root, the one a search naming the app means.
#[test]
fn a_published_resource_says_what_it_is() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let meta = packages[0].meta.clone();
    let root = ada.host.root_of(APP).unwrap().doc;
    ada.commit();
    for text in ["milk", "eggs"] {
        add(&mut ada, &meta, &root, text);
    }
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    let mut out = Vec::new();
    roots(&published.trees()[&root], &mut out);
    let ids = item_ids(&ada, &meta, &root);
    let item = |id: &str| resource_id(APP, &format!("{root}/{id}"));
    assert_eq!(
        out,
        [
            (resource_id(APP, &root), true),
            (item(&ids[0]), false),
            (item(&ids[1]), false),
        ]
    );
    let named = published.trees()[&root].resource_type.clone().unwrap();
    assert_eq!(
        (named.noun, named.app),
        (
            tilo_actions::Noun::new("todo list", "todo lists"),
            tilo_actions::Noun::new("Todo", "todos")
        )
    );
}

#[test]
fn planning_an_action_changes_nothing_anyone_follows() {
    let (ada, meta, root) = workspace();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let before = indexed(&ada);
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
            from: None,
        })
        .unwrap();
    assert_eq!(indexed(&ada), before);
    assert!(published.update(&ada.host, &planner).is_empty());
    assert!(!view.is_stale(), "no view re-runs for a trial run");
    assert!(view.poll_patches().unwrap().is_empty());
}

#[test]
fn a_host_without_frames_publishes_the_whole_workspace_and_takes_a_change_in_after_it() {
    let (mut ada, meta, root) = workspace();
    let ids = item_ids(&ada, &meta, &root);
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    // A window publishes nothing up front: a record is read when a window reaches it.
    let mut windowed = Publications::new(&ada.host);
    windowed.window();
    assert!(windowed.update(&ada.host, &planner).is_empty());
    assert!(!windowed.is_building());
    // What the window shows is read on the frame that shows it, and only that.
    let shown = [resource_id(APP, &format!("{root}/{}", ids[1]))];
    assert_eq!(windowed.reached(&ada.host, &planner, &shown).len(), 1);
    assert_eq!(
        windowed.visited(),
        1,
        "what the window shows of three items reads one record"
    );
    // A host without frames publishes the whole workspace, read on another thread, and a
    // change made while it reads is taken in after what it read.
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    assert_eq!(
        shape(&published.trees()[&root]),
        published_anew(&ada, &meta, &root),
        "every record is published for a caller that reads the whole workspace"
    );
    retype(&mut ada, &meta, &root, &ids[1], "oat milk");
    published.settle(&ada.host, &planner);
    assert!(!published.is_building());
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
    let (ada, _meta, root) = workspace();
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

/// Conversations of notes: a collection within a collection's items.
const THREADS: &str = r#"app so.tilo.threads "Threads"

type Note {
  text: Text merge text
}

type Thread {
  title: Text merge text
  notes: Collection<Note>
  draft: Text merge text
}

type Inbox {
  threads: Collection<Thread>
}

action start(i: Inbox, title: Text) { i.threads.insert(Thread(title: title)) }

action post(t: Thread, text: Text) { t.notes.insert(Note(text: text)) }

action clear(n: Note) { n.text = "" }

root Inbox

default view main for Inbox

view main(i: Inbox) {
  page {
    for t in i.threads {
      text(t.title)
    }
  }
}
"#;

const THREADS_APP: &str = "so.tilo.threads";

fn invoke(
    replica: &mut Replica,
    meta: &tilolang_runtime::Meta,
    (ty, action): (&str, &str),
    resource: &str,
    args: &Json,
) {
    let action = meta.actions.iter().find(|a| a.name == action).unwrap();
    let planned = replica
        .host
        .plan(&PlanRequest {
            app: THREADS_APP,
            kind: &format!("tl.{THREADS_APP}.{ty}"),
            action_id: &action.id,
            resource: Some(resource),
            args: args.as_object().unwrap(),
            caller: None,
            from: None,
        })
        .unwrap();
    replica.apply(&planned);
}

fn edit_text(replica: &mut Replica, root: &str, path: Vec<Segment>, text: &str) {
    replica.apply(&DynAction {
        app: THREADS_APP.to_owned(),
        kind: format!("tl.{THREADS_APP}.Inbox"),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: root.to_owned(),
            edit: Edit::new(
                path,
                EditOp::Replace {
                    text: text.to_owned(),
                },
            ),
        }],
    });
}

/// Every published resource with a deferred action, by id, and the target that action is bound
/// to: a resource republished is bound to a target of its own, one kept keeps it. (The action's
/// builder is its type's, shared by every item of the type.)
fn targets(resource: &ResourceSpec, out: &mut std::collections::BTreeMap<String, Target>) {
    if let Some(action) = resource
        .actions
        .iter()
        .find(|a| matches!(a.input(), tilo_actions::ActionInput::Deferred { .. }))
    {
        out.insert(resource.id.as_str().to_owned(), action.target().clone());
    }
    for child in &resource.children {
        targets(child, out);
    }
}

/// A change within a conversation is republished where it is: typing its draft reads its
/// label again, retyping a note relabels that note, posting one adds it, and the notes no
/// change reached are the ones published before.
#[test]
fn a_change_within_an_item_republishes_only_the_resources_it_reached() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(THREADS, "")]);
    let meta = packages[0].meta.clone();
    let root = ada.host.root_of(THREADS_APP).unwrap().doc;
    ada.commit();
    let threads = field_id(&meta, "Inbox", "threads");
    let notes = field_id(&meta, "Thread", "notes");
    invoke(
        &mut ada,
        &meta,
        ("Inbox", "start"),
        &root,
        &json!({ "title": "inbox" }),
    );
    let thread_id = match ada.host.store().record(&root).unwrap().get(&threads) {
        Some(DynValue::Collection(items)) => items.iter().next().unwrap().0.as_str().to_owned(),
        other => panic!("{other:?}"),
    };
    let thread = format!("{root}/{thread_id}");
    for text in ["one", "two", "three"] {
        invoke(
            &mut ada,
            &meta,
            ("Thread", "post"),
            &thread,
            &json!({ "text": text }),
        );
    }
    let note_ids: Vec<String> = match ada.host.store().record(&root).unwrap().get(&threads) {
        Some(DynValue::Collection(items)) => match items.iter().next().unwrap().1.get(&notes) {
            Some(DynValue::Collection(notes)) => {
                notes.iter().map(|(id, _)| id.as_str().to_owned()).collect()
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    let anew = |ada: &Replica| {
        let planner: Arc<dyn Planner> = Arc::new(Refusing);
        let record = ada.host.store().record(&root).unwrap();
        let label = ada.host.label_of(&root);
        shape(&publish_tree(
            &ActionTypes::new(&meta, &planner, &SourceWords),
            &record,
            "Inbox",
            &root,
            &label,
        ))
    };
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    assert_eq!(shape(&published.trees()[&root]), anew(&ada));
    let before = {
        let mut out = std::collections::BTreeMap::new();
        targets(&published.trees()[&root], &mut out);
        out
    };
    assert_eq!(before.len(), 3, "the three notes: {:?}", before.keys());
    let note = |id: &str| resource_id(THREADS_APP, &format!("{root}/{id}"));
    // Typing the draft: the conversation names itself by it once its title is empty.
    let thread_path = |field: &str| {
        vec![
            Segment::Item(Id::from_raw(&thread_id)),
            Segment::Field(field_id(&meta, "Thread", field)),
        ]
    };
    edit_text(&mut ada, &root, thread_path("draft"), "hel");
    // Whether the draft names the conversation depends on where its field sorts among the
    // conversation's texts: the record is reported exactly when its tree changed, and is the
    // very same tree when it did not.
    let was = Rc::clone(&published.trees()[&root]);
    let touched = published.update(&ada.host, &planner);
    let now = &published.trees()[&root];
    if shape(now) == shape(&was) {
        assert!(touched.is_empty(), "{touched:?} reported, nothing changed");
        assert!(Rc::ptr_eq(now, &was), "an unchanged tree is the very one");
    } else {
        assert_eq!(touched, vec![root.clone()]);
    }
    check_kept(&ada, &published, &before, "typing the draft", &anew);

    let note_path = vec![
        Segment::Item(Id::from_raw(&note_ids[1])),
        Segment::Field(field_id(&meta, "Note", "text")),
    ];
    edit_text(&mut ada, &root, note_path, "zwei");
    published.update(&ada.host, &planner);
    check_kept(&ada, &published, &before, "retyping a note", &anew);
    assert_eq!(
        find(&published.trees()[&root], &note(&note_ids[1]))
            .unwrap()
            .label,
        "zwei"
    );

    invoke(
        &mut ada,
        &meta,
        ("Thread", "post"),
        &thread,
        &json!({ "text": "four" }),
    );
    published.update(&ada.host, &planner);
    check_kept(&ada, &published, &before, "posting a note", &anew);
}

fn find<'r>(resource: &'r ResourceSpec, id: &str) -> Option<&'r ResourceSpec> {
    if resource.id.as_str() == id {
        return Some(resource);
    }
    resource.children.iter().find_map(|child| find(child, id))
}

/// The tree published equals publishing anew, and every resource with a target in `before`
/// still has the one it had.
fn check_kept(
    ada: &Replica,
    published: &Publications,
    before: &std::collections::BTreeMap<String, Target>,
    step: &str,
    anew: &dyn Fn(&Replica) -> String,
) {
    let tree = published.trees().values().next().unwrap();
    assert_eq!(shape(tree), anew(ada), "after {step}");
    let mut now = std::collections::BTreeMap::new();
    targets(tree, &mut now);
    for (id, target) in before {
        assert!(
            now.get(id).is_some_and(|now| now.same(target)),
            "{id} was republished by {step}"
        );
    }
}

/// The planned action moving `item` to the front of the list.
fn plan_move_first(
    replica: &Replica,
    meta: &tilolang_runtime::Meta,
    root: &str,
    item: &str,
) -> DynAction {
    let move_first = meta
        .actions
        .iter()
        .find(|a| a.name == "move_first")
        .unwrap();
    let items = field_id(meta, "TodoList", "items");
    let args = json!({ "i": format!("{root}/{items}:{item}") })
        .as_object()
        .cloned()
        .unwrap();
    replica
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: "tl.so.tilo.todo.TodoList",
            action_id: &move_first.id,
            resource: Some(root),
            args: &args,
            caller: None,
            from: None,
        })
        .unwrap()
}

/// Remove `item` from the list, as an edit of `replica`'s.
fn remove(replica: &mut Replica, meta: &tilolang_runtime::Meta, root: &str, item: &str) {
    let _ = meta;
    replica.apply(&DynAction {
        app: APP.to_owned(),
        kind: "tl.so.tilo.todo.TodoList".to_owned(),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Place {
            id: root.to_owned(),
            item: item.to_owned(),
            at: tilolang_runtime::place::Placement::Removed.encode(),
            nested: Vec::new(),
            ty: String::new(),
            holders: vec![root.to_owned()],
        }],
    });
}

/// A collection's listing is kept across items added, moved and removed, by this replica and by
/// a peer: the store moves each item a change touched and says how ([`Splice`]), and what it
/// holds is what listing the collection anew reads.
#[test]
fn the_store_keeps_a_listing_by_the_items_a_change_touched() {
    use tilolang_runtime::meta::FieldKind;
    use tilolang_runtime::store::{FieldKey, FieldSpec, Splice, Store as _};
    use tilolang_runtime::value::{ResourceRef, Value};

    let (mut ada, meta, root, lock) = workspace_with_ids();
    let mut grace = ada.fork("grace", &[(TODO, &lock)]);
    let items = field_id(&meta, "TodoList", "items");
    let list_ty = meta
        .types
        .iter()
        .find(|t| t.name == "TodoList")
        .unwrap()
        .id
        .clone();
    let list = ResourceRef::root(&root, list_ty);
    let spec = FieldSpec {
        id: items.clone(),
        kind: FieldKind::Collection,
    };
    let key = FieldKey::new(&list, items);
    let store = ada.host.store().clone();
    let listed = || store.listing(&list, &spec).unwrap().ids();
    assert_eq!(listed(), item_ids(&ada, &meta, &root));
    let watch = store.watch();

    // A peer's item arrives at the end.
    add(&mut grace, &meta, &root, "tea");
    ada.sync_with(&mut grace);
    let ids = item_ids(&ada, &meta, &root);
    let tea = ids[3].clone();
    let arrived = [Splice::Insert {
        at: 3,
        item: tea.clone(),
    }];
    assert_eq!(watch.take().listing(&key), Some(&arrived[..]));
    assert_eq!(listed(), ids);

    // Moved to the front by its replica: out of its place, into the first.
    let moved = plan_move_first(&ada, &meta, &root, &tea);
    assert!(watch.take().is_empty(), "planning tells nobody");
    ada.apply(&moved);
    let steps = [
        Splice::Remove {
            at: 3,
            item: tea.clone(),
        },
        Splice::Insert { at: 0, item: tea },
    ];
    assert_eq!(watch.take().listing(&key), Some(&steps[..]));
    assert_eq!(listed(), item_ids(&ada, &meta, &root));

    // Added by the runtime here: told when the action commits, with the step its item took
    // into the listing, and not again when the record holds the action's change, which moves
    // nothing those who read were told.
    let add_action = meta.actions.iter().find(|a| a.name == "add").unwrap();
    ada.host
        .runtime()
        .invoke_action(
            &tilolang_runtime::Principal::new("ada"),
            (
                add_action.id.as_str(),
                vec![Value::resource(&list), Value::text("jam")],
            ),
        )
        .unwrap();
    let told = watch.take();
    let ids = item_ids(&ada, &meta, &root);
    let added = [Splice::Insert {
        at: 4,
        item: ids[4].clone(),
    }];
    assert_eq!(told.listing(&key), Some(&added[..]), "{told:?}");
    assert_eq!(listed(), ids);
    ada.commit();
    let told = watch.take();
    assert_eq!(told.listing(&key), None, "nothing moved: {told:?}");
    assert!(
        !told.docs.values().flatten().any(|named| *named == key),
        "the collection is not named again: {told:?}"
    );
    assert_eq!(item_ids(&ada, &meta, &root), ids);
    assert_eq!(listed(), ids);

    // Removed by a peer.
    grace.sync_with(&mut ada);
    remove(&mut grace, &meta, &root, &ids[1]);
    ada.sync_with(&mut grace);
    let removed = [Splice::Remove {
        at: 1,
        item: ids[1].clone(),
    }];
    assert_eq!(watch.take().listing(&key), Some(&removed[..]));
    assert_eq!(listed(), item_ids(&ada, &meta, &root));

    // A store that lists the collection anew reads the same.
    let fresh = Replica::new("ada", ada.doc.fork(), &[(TODO, &lock)]).0;
    assert_eq!(
        fresh.host.store().listing(&list, &spec).unwrap().ids(),
        listed()
    );
}

/// A filtered listing (the items a predicate holds of) is kept across items finished, added,
/// moved and removed, here and by a peer: each change has the store evaluate the items it
/// touched, not every item, and what it holds is what evaluating every item anew finds.
#[test]
fn the_store_keeps_a_filtered_listing_by_the_items_a_change_touched() {
    use tilolang_runtime::meta::FieldKind;
    use tilolang_runtime::store::{FieldSpec, Store as _};
    use tilolang_runtime::value::{ResourceRef, Value};

    let (mut ada, meta, root, lock) = workspace_with_ids();
    let mut grace = ada.fork("grace", &[(TODO, &lock)]);
    let items = field_id(&meta, "TodoList", "items");
    let done = field_id(&meta, "Item", "done");
    let type_id = |name: &str| {
        meta.types
            .iter()
            .find(|t| t.name == name)
            .unwrap()
            .id
            .clone()
    };
    let list = ResourceRef::root(&root, type_id("TodoList"));
    let item_ty = type_id("Item");
    let spec = FieldSpec {
        id: items,
        kind: FieldKind::Collection,
    };
    let open = not_done(done);
    let store = ada.host.store().clone();
    let filtered = || {
        store
            .listing_where(&list, &spec, &item_ty, &open)
            .unwrap()
            .ids()
    };
    // Every item evaluated anew, read by read.
    let anew = |replica: &Replica| -> Vec<String> {
        item_ids(replica, &meta, &root)
            .into_iter()
            .filter(|id| {
                let item = list.item(id, item_ty.clone());
                replica.host.runtime().read(&item, "done").ok() != Some(Value::Bool(true))
            })
            .collect()
    };
    let evaluated = || store.read_stats().evaluated;

    // Answered from the index's column of `done`: no item is evaluated.
    assert_eq!(filtered(), anew(&ada));
    assert_eq!(evaluated(), 0, "answered from the column");
    assert_eq!(filtered(), anew(&ada));
    assert_eq!(evaluated(), 0, "answered from the column");

    // Finished here: it leaves, and only it is evaluated again.
    let ids = item_ids(&ada, &meta, &root);
    let finish = meta.actions.iter().find(|a| a.name == "finish").unwrap();
    ada.host
        .runtime()
        .invoke_action(
            &tilolang_runtime::Principal::new("ada"),
            (
                finish.id.as_str(),
                vec![Value::resource(&list.item(&ids[1], item_ty.clone()))],
            ),
        )
        .unwrap();
    ada.commit();
    let before = evaluated();
    assert_eq!(filtered(), anew(&ada));
    assert_eq!(filtered().len(), 2);
    assert!(evaluated() - before <= 1, "the finished item alone");

    // A peer's item arrives at the end, and one moves to the front.
    add(&mut grace, &meta, &root, "tea");
    ada.sync_with(&mut grace);
    let before = evaluated();
    assert_eq!(filtered(), anew(&ada));
    assert!(evaluated() - before <= 1, "the arrived item alone");
    let tea = item_ids(&ada, &meta, &root)[3].clone();
    let moved = plan_move_first(&ada, &meta, &root, &tea);
    ada.apply(&moved);
    let before = evaluated();
    assert_eq!(filtered(), anew(&ada));
    assert_eq!(filtered()[0], tea);
    assert!(evaluated() - before <= 1, "the moved item alone");

    // Removed by a peer.
    grace.sync_with(&mut ada);
    let first_open = filtered()[1].clone();
    remove(&mut grace, &meta, &root, &first_open);
    ada.sync_with(&mut grace);
    assert_eq!(filtered(), anew(&ada));
    assert!(!filtered().contains(&first_open));

    // A store that lists the collection anew finds the same.
    let fresh = Replica::new("ada", ada.doc.fork(), &[(TODO, &lock)]).0;
    assert_eq!(
        fresh
            .host
            .store()
            .listing_where(&list, &spec, &item_ty, &open)
            .unwrap()
            .ids(),
        filtered()
    );
}

/// `not item.done`, as the store answers it: the register `done`, false where unwritten.
fn not_done(done: String) -> tilolang_runtime::store::Pred {
    use tilolang::pred::{CmpOp, Lit, Operand};
    use tilolang_runtime::meta::FieldKind;
    use tilolang_runtime::store::{FieldRead, FieldSpec, Pred};

    Pred::Not(Box::new(Pred::Cmp(
        CmpOp::Eq,
        Operand::Field(FieldRead {
            spec: FieldSpec {
                id: done,
                kind: FieldKind::Register,
            },
            absent: Lit::Bool(false),
        }),
        Operand::Lit(Lit::Bool(true)),
    )))
}

const NOTES_APP: &str = "so.tilo.notes";

const NOTES: &str = r#"app so.tilo.notes "Notes"

type Note {
  title: Text merge text
  body: Text merge text
}

type Notebook {
  title: Text merge text
  notes: Collection<Note>
}

action add(b: Notebook, title: Text, body: Text) { b.notes.insert(Note(title: title, body: body)) }

root Notebook

default view main for Notebook

view main(b: Notebook) {
  page {
    input(bind: b.title, placeholder: "Title")
    for n in b.notes {
      text(n.title)
    }
  }
}
"#;

fn add_note(
    replica: &mut Replica,
    meta: &tilolang_runtime::Meta,
    root: &str,
    title: &str,
    body: &str,
) {
    let action = meta.actions.iter().find(|a| a.name == "add").unwrap();
    let args = json!({ "title": title, "body": body })
        .as_object()
        .cloned()
        .unwrap();
    let planned = replica
        .host
        .plan(&PlanRequest {
            app: NOTES_APP,
            kind: "tl.so.tilo.notes.Notebook",
            action_id: &action.id,
            resource: Some(root),
            args: &args,
            caller: None,
            from: None,
        })
        .unwrap();
    replica.apply(&planned);
}

/// What a window publishes of a resource covers every text it holds, in field order, the first
/// as what it reads as; an emptied text drops out.
#[test]
fn every_text_of_a_resource_is_published() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(NOTES, "")]);
    let meta = packages[0].meta.clone();
    let root = ada.host.root_of(NOTES_APP).unwrap();
    ada.commit();
    add_note(&mut ada, &meta, &root.doc, "apples", "and plums");
    add_note(&mut ada, &meta, &root.doc, "", "only a body");
    let mut notes: Vec<(String, Vec<String>)> = every_hit(&ada.host)
        .into_iter()
        .filter(|hit| hit.type_name == "Note")
        .map(|hit| (hit.title, hit.texts))
        .collect();
    notes.sort();
    assert_eq!(
        notes,
        [
            (
                "apples".to_owned(),
                vec!["apples".to_owned(), "and plums".to_owned()]
            ),
            ("only a body".to_owned(), vec!["only a body".to_owned()]),
        ]
    );
}
