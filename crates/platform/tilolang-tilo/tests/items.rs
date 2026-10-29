//! Items stored by their id: a card moved to another column is the same card, with its history
//! and every reference to it, and two replicas moving, editing and removing at the same time
//! converge where convergence.md ("Storage and sync") says.

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilolang_runtime::Principal;
use tilolang_runtime::meta::FieldKind;
use tilolang_runtime::store::{FieldSpec, FieldValue, Store as _};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_tilo::action::{DynAction, DynEdit};
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::record::{DynRecord, DynValue};

use crate::fixtures::{Replica, field_id};

/// A board whose columns hold their cards, and a tree of nodes that hold nodes.
const KANBAN: &str = r#"app so.tilo.kanban "Kanban"

type Card {
  title: Text merge text
}

type Column {
  title: Text merge text
  cards: Collection<Card>
}

type Node {
  name: Text merge text
  kids: Collection<Node>
}

type Board {
  columns: Collection<Column>
  nodes: Collection<Node>
}

action add_column(b: Board, title: Text) -> Column { b.columns.insert(Column(title: title)) }

action add_card(c: Column, title: Text) -> Card { c.cards.insert(Card(title: title)) }

action file(c: Column, card: Card) { c.cards.move(card, to: c.cards.len()) }

action place(c: Column, card: Card, at: Int) { c.cards.move(card, to: at) }

action retitle(card: Card, title: Text) { card.title = title }

action drop_column(b: Board, c: Column) { b.columns.remove(c) }

action add_node(b: Board, name: Text) -> Node { b.nodes.insert(Node(name: name)) }

action nest(into: Node, n: Node) { into.kids.move(n, to: into.kids.len()) }

root Board

default view main for Board

view main(b: Board) {
  page {
    for c in b.columns {
      text(c.title)
      for card in c.cards {
        text(card.title)
      }
    }
  }
}
"#;

const APP: &str = "so.tilo.kanban";

struct Room {
    ada: Replica,
    grace: Replica,
    meta: tilolang_runtime::Meta,
    root: String,
    lock: String,
}

fn room() -> Room {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(KANBAN, "")]);
    let package = packages[0].clone();
    let root = ada.host.root_of(APP).unwrap().doc;
    ada.commit();
    let grace = ada.fork("grace", &[(KANBAN, &package.ids)]);
    Room {
        ada,
        grace,
        meta: package.meta,
        root,
        lock: package.ids,
    }
}

impl Room {
    fn replica(&mut self, who: &str) -> &mut Replica {
        if who == "ada" {
            &mut self.ada
        } else {
            &mut self.grace
        }
    }

    /// `who` invokes `name` on the resource `subject` (of the type `ty`), as the door plans it,
    /// and commits it; returns what it recorded.
    fn act(&mut self, who: &str, (ty, subject): (&str, &str), name: &str, args: Json) -> DynAction {
        let action = self
            .meta
            .actions
            .iter()
            .find(|a| a.name == name)
            .unwrap()
            .id
            .clone();
        let args = args.as_object().cloned().unwrap_or_default();
        let kind = format!("tl.{APP}.{ty}");
        let replica = self.replica(who);
        let planned = replica
            .host
            .plan(&PlanRequest {
                app: APP,
                kind: &kind,
                action_id: &action,
                resource: Some(subject),
                args: &args,
                caller: None,
            })
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        replica.apply(&planned);
        planned
    }

    /// The id of the item `action` inserted.
    fn made(action: &DynAction) -> String {
        action
            .edits
            .iter()
            .find_map(|edit| match edit {
                DynEdit::Insert { item, .. } => Some(item.clone()),
                _ => None,
            })
            .unwrap()
    }

    /// An item's reference, as an argument names it.
    fn at(&self, id: &str) -> String {
        format!("{}/{id}", self.root)
    }

    fn column(&mut self, who: &str, title: &str) -> String {
        let root = self.root.clone();
        Self::made(&self.act(
            who,
            ("Board", &root),
            "add_column",
            json!({ "title": title }),
        ))
    }

    fn card(&mut self, who: &str, column: &str, title: &str) -> String {
        let column = self.at(column);
        Self::made(&self.act(
            who,
            ("Column", &column),
            "add_card",
            json!({ "title": title }),
        ))
    }

    fn file(&mut self, who: &str, column: &str, card: &str) {
        let (column, card) = (self.at(column), self.at(card));
        self.act(who, ("Column", &column), "file", json!({ "card": card }));
    }

    /// The items of the collection `field` (of the type `ty`) of `holder`, as `who`'s store lists
    /// them from its index, checked against the record read whole.
    fn listed(&self, who: &str, (ty, field): (&str, &str), holder: Option<&str>) -> Vec<String> {
        let replica = if who == "ada" { &self.ada } else { &self.grace };
        let field = field_id(&self.meta, ty, field);
        let holder_ref = match holder {
            Some(id) => ResourceRef::parse_id(&self.at(id), String::new()).unwrap(),
            None => ResourceRef::root(&self.root, String::new()),
        };
        let spec = FieldSpec {
            id: field.clone(),
            kind: FieldKind::Collection,
        };
        let listed = match replica.host.store().read(&holder_ref, &spec).unwrap() {
            FieldValue::Items(ids) => ids,
            other => panic!("{other:?}"),
        };
        let record = replica.host.store().record(&self.root).unwrap();
        let whole = match holder {
            None => items_of(&record, &field),
            Some(id) => find(&record, id)
                .map(|item| items_of(item, &field))
                .unwrap_or_default(),
        };
        assert_eq!(
            listed, whole,
            "{who}: the index lists what the record holds"
        );
        listed
    }

    fn cards(&self, who: &str, column: &str) -> Vec<String> {
        self.listed(who, ("Column", "cards"), Some(column))
    }

    fn title(&self, who: &str, card: &str) -> Result<String, String> {
        let replica = if who == "ada" { &self.ada } else { &self.grace };
        let ty = self.meta.types.iter().find(|t| t.name == "Card").unwrap();
        let at = ResourceRef::parse_id(&self.at(card), ty.id.clone()).unwrap();
        match replica.host.runtime().read(&at, "title") {
            Ok(Value::Text(text)) => Ok(text),
            Ok(other) => Err(format!("{other:?}")),
            Err(error) => Err(error.to_string()),
        }
    }

    fn sync(&mut self) {
        self.ada.sync_with(&mut self.grace);
        assert_eq!(
            self.ada.host.store().record(&self.root).unwrap(),
            self.grace.host.store().record(&self.root).unwrap(),
            "both replicas read the same board"
        );
        // Each index, kept change by change, holds what deriving it anew holds.
        for replica in [&self.ada, &self.grace] {
            let fresh =
                Replica::new("fresh", replica.doc.clone().fork(), &[(KANBAN, &self.lock)]).0;
            assert_eq!(
                replica.host.store().index_contents(APP).unwrap(),
                fresh.host.store().index_contents(APP).unwrap(),
                "{}'s index",
                replica.name
            );
        }
    }
}

fn items_of(record: &DynRecord, field: &str) -> Vec<String> {
    match record.get(field) {
        Some(DynValue::Collection(items)) => {
            items.iter().map(|(id, _)| id.as_str().to_owned()).collect()
        }
        other => panic!("{other:?}"),
    }
}

/// The item `id` anywhere in `record`.
fn find<'r>(record: &'r DynRecord, id: &str) -> Option<&'r DynRecord> {
    for field in &record.schema().fields {
        if let Some(DynValue::Collection(items)) = record.get(&field.id) {
            for (item_id, item) in items.iter() {
                if item_id.as_str() == id {
                    return Some(item);
                }
                if let Some(found) = find(item, id) {
                    return Some(found);
                }
            }
        }
    }
    None
}

#[test]
fn a_card_moved_to_another_column_is_the_same_card_with_its_history() {
    let mut r = room();
    let (todo, doing) = (r.column("ada", "To do"), r.column("ada", "Doing"));
    let card = r.card("ada", &todo, "Write the notes");
    let other = r.card("ada", &todo, "Ship");
    let card_at = r.at(&card);
    r.act(
        "ada",
        ("Card", &card_at),
        "retitle",
        json!({ "title": "Write the release notes" }),
    );
    r.file("ada", &doing, &card);
    assert_eq!(r.cards("ada", &todo), [other]);
    assert_eq!(r.cards("ada", &doing), std::slice::from_ref(&card));
    // The reference the card had before the move still names it, with what was written of it.
    assert_eq!(r.title("ada", &card).unwrap(), "Write the release notes");
    // History is the one card's: made, retitled, moved.
    let ty = r.meta.types.iter().find(|t| t.name == "Card").unwrap();
    let at = ResourceRef::parse_id(&card_at, ty.id.clone()).unwrap();
    let versions = r.ada.host.store().versions(&at).unwrap();
    let action = |name: &str| {
        r.meta
            .actions
            .iter()
            .find(|a| a.name == name)
            .unwrap()
            .id
            .clone()
    };
    let steps: Vec<&str> = versions.iter().map(|v| v.action.as_str()).collect();
    assert_eq!(
        steps,
        [
            action("add_card").as_str(),
            action("retitle").as_str(),
            action("file").as_str()
        ],
        "{versions:?}"
    );
    // So does a reference written as the path the card had when it was nested.
    let cards_field = field_id(&r.meta, "Column", "cards");
    let old = ResourceRef::parse_id(
        &format!("{}/{cards_field}:{card}", r.at(&todo)),
        ty.id.clone(),
    )
    .unwrap();
    assert_eq!(old, at);
}

struct Refusing;

impl tilolang_tilo::publish::Planner for Refusing {
    fn plan(&self, _: &PlanRequest<'_>) -> Result<DynAction, String> {
        Err("not in this test".into())
    }
}

/// A published tree as text: ids and labels, children in order.
fn shape(resource: &tilo_actions::ResourceSpec) -> String {
    let children: Vec<String> = resource.children.iter().map(shape).collect();
    format!(
        "{}|{}[{}]",
        resource.id.as_str(),
        resource.label,
        children.join(",")
    )
}

#[test]
fn what_is_published_follows_a_card_to_its_new_column() {
    use std::sync::Arc;
    use tilolang_tilo::publish::{ActionTypes, Planner, Publications, publish_tree};

    let mut r = room();
    let (todo, doing) = (r.column("ada", "To do"), r.column("ada", "Doing"));
    let card = r.card("ada", &todo, "card");
    r.card("ada", &todo, "other");
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&r.ada.host);
    published.settle(&r.ada.host, &planner);
    r.file("ada", &doing, &card);
    published.settle(&r.ada.host, &planner);
    let record = r.ada.host.store().record(&r.root).unwrap();
    let label = r.ada.host.label_of(&r.root);
    let anew = publish_tree(
        &ActionTypes::new(&r.meta, &planner),
        &record,
        "Board",
        &r.root,
        &label,
    );
    let kept = shape(&published.trees()[&r.root]);
    assert_eq!(kept, shape(&anew));
    assert!(kept.contains(&format!("{}|card", r.at(&card))), "{kept}");
}

#[test]
fn a_card_moves_within_its_column_and_into_another_at_a_place() {
    let mut r = room();
    let (todo, doing) = (r.column("ada", "To do"), r.column("ada", "Doing"));
    let first = r.card("ada", &todo, "first");
    let second = r.card("ada", &todo, "second");
    let third = r.card("ada", &doing, "third");
    let fourth = r.card("ada", &doing, "fourth");
    let todo_at = r.at(&todo);
    let doing_at = r.at(&doing);
    let second_at = r.at(&second);
    r.act(
        "ada",
        ("Column", &todo_at),
        "place",
        json!({ "card": second_at, "at": 0 }),
    );
    assert_eq!(r.cards("ada", &todo), [second.clone(), first.clone()]);
    let first_at = r.at(&first);
    r.act(
        "ada",
        ("Column", &doing_at),
        "place",
        json!({ "card": first_at, "at": 1 }),
    );
    assert_eq!(r.cards("ada", &todo), [second]);
    assert_eq!(r.cards("ada", &doing), [third, first, fourth]);
}

#[test]
fn concurrent_moves_of_a_card_leave_it_where_the_last_put_it() {
    let mut r = room();
    let (todo, doing, done) = (
        r.column("ada", "To do"),
        r.column("ada", "Doing"),
        r.column("ada", "Done"),
    );
    let card = r.card("ada", &todo, "card");
    r.sync();
    r.file("ada", &doing, &card);
    r.file("grace", &done, &card);
    r.sync();
    let (in_doing, in_done) = (r.cards("ada", &doing), r.cards("ada", &done));
    assert!(r.cards("ada", &todo).is_empty());
    assert_eq!(
        in_doing.len() + in_done.len(),
        1,
        "one card in one column: {in_doing:?} {in_done:?}"
    );
    assert_eq!(r.cards("grace", &doing), in_doing);
    assert_eq!(r.cards("grace", &done), in_done);
}

#[test]
fn an_edit_made_while_the_card_moved_is_kept() {
    let mut r = room();
    let (todo, doing) = (r.column("ada", "To do"), r.column("ada", "Doing"));
    let card = r.card("ada", &todo, "draft");
    r.sync();
    let card_at = r.at(&card);
    r.act(
        "ada",
        ("Card", &card_at),
        "retitle",
        json!({ "title": "final" }),
    );
    r.file("grace", &doing, &card);
    r.sync();
    for who in ["ada", "grace"] {
        assert_eq!(r.cards(who, &doing), std::slice::from_ref(&card));
        assert!(r.cards(who, &todo).is_empty());
        assert_eq!(r.title(who, &card).unwrap(), "final");
    }
}

#[test]
fn removing_a_column_removes_its_cards_and_not_one_moved_out_before() {
    let mut r = room();
    let (todo, doing) = (r.column("ada", "To do"), r.column("ada", "Doing"));
    let stays = r.card("ada", &todo, "stays");
    let leaves = r.card("ada", &todo, "leaves");
    r.file("ada", &doing, &leaves);
    let root = r.root.clone();
    let todo_at = r.at(&todo);
    r.act(
        "ada",
        ("Board", &root),
        "drop_column",
        json!({ "c": todo_at }),
    );
    assert_eq!(
        r.listed("ada", ("Board", "columns"), None),
        std::slice::from_ref(&doing)
    );
    assert_eq!(r.cards("ada", &doing), std::slice::from_ref(&leaves));
    assert!(
        r.title("ada", &stays).is_err(),
        "the card went with its column"
    );
    assert_eq!(r.title("ada", &leaves).unwrap(), "leaves");
    r.sync();
    assert!(r.title("grace", &stays).is_err());
    assert_eq!(r.cards("grace", &doing), [leaves]);
}

#[test]
fn a_card_moved_out_while_its_column_is_removed_stays_where_it_was_moved() {
    let mut r = room();
    let (todo, doing) = (r.column("ada", "To do"), r.column("ada", "Doing"));
    let rescued = r.card("ada", &todo, "rescued");
    let gone = r.card("ada", &todo, "gone");
    r.sync();
    let root = r.root.clone();
    let todo_at = r.at(&todo);
    r.act(
        "ada",
        ("Board", &root),
        "drop_column",
        json!({ "c": todo_at }),
    );
    r.file("grace", &doing, &rescued);
    r.sync();
    for who in ["ada", "grace"] {
        assert_eq!(
            r.listed(who, ("Board", "columns"), None),
            std::slice::from_ref(&doing)
        );
        assert_eq!(
            r.cards(who, &doing),
            std::slice::from_ref(&rescued),
            "{who}"
        );
        assert!(
            r.title(who, &gone).is_err(),
            "{who}: the card left in it went with it"
        );
    }
}

#[test]
fn a_card_moved_into_a_column_removed_meanwhile_goes_with_the_column() {
    let mut r = room();
    let (todo, doing) = (r.column("ada", "To do"), r.column("ada", "Doing"));
    let card = r.card("ada", &todo, "card");
    r.sync();
    let root = r.root.clone();
    let doing_at = r.at(&doing);
    r.act(
        "ada",
        ("Board", &root),
        "drop_column",
        json!({ "c": doing_at }),
    );
    r.file("grace", &doing, &card);
    r.sync();
    for who in ["ada", "grace"] {
        assert_eq!(
            r.listed(who, ("Board", "columns"), None),
            std::slice::from_ref(&todo)
        );
        assert!(r.cards(who, &todo).is_empty(), "{who}");
        assert!(r.title(who, &card).is_err(), "{who}");
    }
}

#[test]
fn concurrent_moves_into_each_other_hold_the_later_back() {
    let mut r = room();
    let root = r.root.clone();
    let made = |r: &mut Room, name: &str| {
        Room::made(&r.act("ada", ("Board", &root), "add_node", json!({ "name": name })))
    };
    let (one, two) = (made(&mut r, "one"), made(&mut r, "two"));
    r.sync();
    let (one_at, two_at) = (r.at(&one), r.at(&two));
    r.act(
        "ada",
        ("Node", &two_at),
        "nest",
        json!({ "n": one_at.clone() }),
    );
    r.act("grace", ("Node", &one_at), "nest", json!({ "n": two_at }));
    r.sync();
    let kids = |r: &Room, who: &str, id: &str| r.listed(who, ("Node", "kids"), Some(id));
    for who in ["ada", "grace"] {
        let top = r.listed(who, ("Board", "nodes"), None);
        assert_eq!(top.len(), 1, "{who}: one node holds the other: {top:?}");
        let held = if top[0] == one { &two } else { &one };
        assert_eq!(kids(&r, who, &top[0]), std::slice::from_ref(held), "{who}");
        assert!(kids(&r, who, held).is_empty(), "{who}");
    }
    // Moving the one on top into the other is refused: it would hold itself.
    let top = r.listed("ada", ("Board", "nodes"), None)[0].clone();
    let held = if top == one { two } else { one };
    let action = r
        .meta
        .actions
        .iter()
        .find(|a| a.name == "nest")
        .unwrap()
        .id
        .clone();
    let ty = r.meta.types.iter().find(|t| t.name == "Node").unwrap();
    let node = |id: &str| {
        Value::resource(&ResourceRef::parse_id(&format!("{root}/{id}"), ty.id.clone()).unwrap())
    };
    let refused = r.ada.host.runtime().invoke_action(
        &Principal::new("ada"),
        (action.as_str(), vec![node(&held), node(&top)]),
    );
    assert!(refused.is_err(), "{refused:?}");
}
