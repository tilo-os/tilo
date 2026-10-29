//! The index's filter columns (search.md, "Who indexes"): equality, ranges, membership and patterns
//! answered from runs of a column, in a collection and across the workspace, kept change by
//! change as a build from the record keeps them, with facet counts read from the same columns.

use automerge::AutoCommit;
use tilolang_runtime::meta::FieldKind;
use tilolang_runtime::query::{Clock, find, resolve, store_pred};
use tilolang_runtime::store::{FieldSpec, Store as _};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Meta, Principal};

use crate::fixtures::{Replica, field_id};

const TASKS: &str = r#"app so.tilo.tasks "Tasks"

/// Something to do.
type Task {
    title: Text (title)
    done: Bool = false
    due: Option<Date> = .none
    status: Status
    effort: Int
    contact: std.Identifier = std.Identifier("tel:+10")
    tags: Set<Text>

    search {
        filter "Open" = not done
        filter done
        filter due
        filter status
        filter effort
        filter contact
        filter tags
    }
}

enum Status {
    waiting
    started
    blocked
}

/// A message in a mailbox.
type Message {
    subject: Text (title)
    from: std.Identifier = std.Identifier("tel:+10")
    to: Set<std.Identifier>
}

type Board {
    tasks: Collection<Task>
    messages: Collection<Message>
}

root Board

default view main for Board

action seed(b: Board) {
    let a = b.tasks.insert(Task(title: "a", effort: 3, due: Date("2026-10-27"), status: .waiting, contact: std.Identifier("jane@yc.com")))
    a.tags.add("home")
    b.tasks.insert(Task(title: "b", effort: 8, due: Date("2026-10-20"), status: .blocked, contact: std.Identifier("tel:+4930123")))
    let c = b.tasks.insert(Task(title: "c", effort: 5, status: .started, contact: std.Identifier("bo@acme.com")))
    c.tags.add("work")
    b.tasks.insert(Task(title: "d", effort: 1, done: true, due: Date("2026-11-02"), status: .waiting, contact: std.Identifier("ada@yc.com")))
    let m = b.messages.insert(Message(subject: "hello", from: std.Identifier("bo@acme.com")))
    m.to.add(std.Identifier("jane@yc.com"))
    b.messages.insert(Message(subject: "invoice", from: std.Identifier("billing@yc.com")))
}

action add(b: Board, title: Text, effort: Int) {
    b.tasks.insert(Task(title: title, effort: effort, status: .started))
}

action check(t: Task, done: Bool = not t.done) {
    t.done = done
}

action set_effort(t: Task, effort: Int) {
    t.effort = effort
}

action tag(t: Task, tag: Text) {
    t.tags.add(tag)
}

action untag(t: Task, tag: Text) {
    t.tags.remove(tag)
}

action remove(b: Board, t: Task) {
    b.tasks.remove(t)
}

action move_first(b: Board, t: Task) {
    if let first = b.tasks.first() {
        b.tasks.move(t, before: first)
    }
}

view main(b: Board) {
    page {
        for t in b.tasks {
            text(t.title)
        }
    }
}
"#;

const APP: &str = "so.tilo.tasks";
const NOW: &str = "2026-10-27T10:00:00Z";

struct Room {
    ada: Replica,
    meta: Meta,
    board: ResourceRef,
}

impl Room {
    fn new() -> Self {
        let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TASKS, "")]);
        let meta = packages[0].meta.clone();
        let board = ada.host.root_of(APP).unwrap();
        ada.commit();
        let mut seeded = Self { ada, meta, board };
        seeded.call("seed", vec![Value::resource(&seeded.board)]);
        seeded
    }

    fn call(&mut self, action: &str, args: Vec<Value>) {
        let id = self
            .meta
            .actions
            .iter()
            .find(|a| a.name == action)
            .unwrap()
            .id
            .clone();
        self.ada
            .host
            .runtime()
            .invoke_action(&Principal::new("ada"), (id.as_str(), args))
            .unwrap();
        self.ada.commit();
    }

    fn ty(&self, name: &str) -> &tilolang_runtime::meta::TypeMeta {
        self.meta.types.iter().find(|t| t.name == name).unwrap()
    }

    fn tasks(&self) -> Vec<ResourceRef> {
        let field = field_id(&self.meta, "Board", "tasks");
        let spec = FieldSpec {
            id: field,
            kind: FieldKind::Collection,
        };
        let task = self.ty("Task").id.clone();
        self.ada
            .host
            .store()
            .listing(&self.board, &spec)
            .unwrap()
            .ids()
            .into_iter()
            .map(|id| self.board.item(&id, task.clone()))
            .collect()
    }

    fn title(&self, task: &ResourceRef) -> String {
        match self
            .ada
            .host
            .runtime()
            .read(task, &field_id(&self.meta, "Task", "title"))
        {
            Ok(Value::Text(t)) => t,
            other => panic!("{other:?}"),
        }
    }

    /// The titles of the board's tasks the typed search `q` holds of, as the store lists them
    /// from its index.
    fn listed(&self, q: &str) -> Vec<String> {
        let task = self.ty("Task");
        let resolved = resolve(
            &find(q).unwrap(),
            task,
            &self.meta.enums,
            &Clock::at(NOW).unwrap(),
        )
        .unwrap();
        let pred = store_pred(&resolved.pred, task, APP).unwrap();
        let field = field_id(&self.meta, "Board", "tasks");
        let spec = FieldSpec {
            id: field,
            kind: FieldKind::Collection,
        };
        self.ada
            .host
            .store()
            .listing_query(
                &self.board,
                &spec,
                &task.id,
                Some(&pred),
                resolved.order.as_ref(),
            )
            .unwrap()
            .ids()
            .into_iter()
            .map(|id| self.title(&self.board.item(&id, task.id.clone())))
            .collect()
    }

    /// The same, every task read and checked one by one, in the collection's order and then
    /// sorted as the query asks.
    fn anew(&self, q: &str) -> Vec<String> {
        let task = self.ty("Task");
        let resolved = resolve(
            &find(q).unwrap(),
            task,
            &self.meta.enums,
            &Clock::at(NOW).unwrap(),
        )
        .unwrap();
        let pred = store_pred(&resolved.pred, task, APP).unwrap();
        let store = self.ada.host.store();
        let mut kept = Vec::new();
        for t in self.tasks() {
            let holds = pred
                .eval(&mut |read: &tilolang_runtime::store::FieldRead| {
                    read.lit(store.read(&t, &read.spec)?)
                })
                .unwrap();
            if holds {
                kept.push(t);
            }
        }
        if let Some(order) = &resolved.order {
            let spec = FieldSpec {
                id: order.field.clone(),
                kind: order.kind,
            };
            let mut keyed: Vec<_> = kept
                .into_iter()
                .map(|t| {
                    let v = tilolang_runtime::store::lit_of(
                        &spec.id,
                        store.read(&t, &spec).unwrap(),
                        &tilolang::pred::Lit::Absent,
                    )
                    .unwrap();
                    (v, t)
                })
                .collect();
            keyed.sort_by(|(a, _), (b, _)| tilolang_runtime::store::order_cmp(a, b));
            if order.descending {
                keyed.reverse();
            }
            kept = keyed.into_iter().map(|(_, t)| t).collect();
        }
        kept.iter().map(|t| self.title(t)).collect()
    }

    fn evaluated(&self) -> u64 {
        self.ada.host.store().read_stats().evaluated
    }

    /// The index as kept, and as a store reading the room anew builds it.
    fn kept_and_anew(
        &mut self,
    ) -> (
        tilolang_tilo::index::Contents,
        tilolang_tilo::index::Contents,
    ) {
        let kept = self.ada.host.store().index_contents(APP).unwrap();
        let fresh = Replica::new("ada", self.ada.doc.fork(), &[(TASKS, "")]).0;
        let anew = fresh.host.store().index_contents(APP).unwrap();
        (kept, anew)
    }
}

/// An action of a change, and what it is given in the room as it is.
type Step = Box<dyn Fn(&Room) -> Vec<Value>>;

const QUERIES: &[&str] = &[
    "",
    "is:open",
    "is:done",
    "done:false",
    "due:today",
    "due:2026-10",
    "due:2026-10-21..",
    "due:..today",
    "due:-7d",
    "due:none",
    "due:any",
    "status:waiting",
    "status:waiting,blocked",
    "effort:>3",
    "effort:3..5",
    "contact:*@yc.com",
    "contact:jane@yc.com",
    "contact:tel:+49*",
    "*@yc.com",
    "tags:home",
    "tags:none",
    "tags:any",
    "is:open effort:>3",
    "sort:effort",
    "sort:-effort",
    "is:open sort:-effort",
    "effort:>=3 sort:effort",
    "sort:-due",
];

#[test]
fn filter_columns_answer_what_reading_every_item_finds_and_follow_every_change() {
    let mut room = Room::new();
    for q in QUERIES {
        assert_eq!(room.listed(q), room.anew(q), "`{q}`");
    }
    assert_eq!(room.listed("status:waiting"), ["a", "d"]);
    assert_eq!(room.listed("is:open sort:-effort"), ["b", "c", "a"]);
    // A named filter, a value, a range in its own order and a membership are runs of a
    // column: no task is checked one by one.
    let before = room.evaluated();
    for q in [
        "is:open",
        "status:blocked",
        "effort:>3 sort:effort",
        "tags:home",
        "due:none",
    ] {
        room.listed(q);
    }
    assert_eq!(room.evaluated(), before, "answered from the columns");

    // Every kind of change, each followed by the columns as a build from the record finds.
    let board = room.board.clone();
    let steps: Vec<(&str, Step)> = vec![
        (
            "check",
            Box::new(|r: &Room| vec![Value::resource(&r.tasks()[0])]),
        ),
        (
            "set_effort",
            Box::new(|r: &Room| vec![Value::resource(&r.tasks()[2]), Value::Int(9)]),
        ),
        (
            "tag",
            Box::new(|r: &Room| vec![Value::resource(&r.tasks()[1]), Value::text("home")]),
        ),
        (
            "untag",
            Box::new(|r: &Room| vec![Value::resource(&r.tasks()[0]), Value::text("home")]),
        ),
        (
            "add",
            Box::new(move |_: &Room| {
                vec![Value::resource(&board), Value::text("e"), Value::Int(4)]
            }),
        ),
        (
            "move_first",
            Box::new(|r: &Room| vec![Value::resource(&r.board), Value::resource(&r.tasks()[4])]),
        ),
        (
            "remove",
            Box::new(|r: &Room| vec![Value::resource(&r.board), Value::resource(&r.tasks()[2])]),
        ),
        (
            "check",
            Box::new(|r: &Room| vec![Value::resource(&r.tasks()[0])]),
        ),
    ];
    for (action, args) in steps {
        let args = args(&room);
        room.call(action, args);
        for q in QUERIES {
            assert_eq!(room.listed(q), room.anew(q), "`{q}` after {action}");
        }
        let (kept, anew) = room.kept_and_anew();
        assert_eq!(kept, anew, "the columns after {action}");
    }
}

#[test]
fn a_query_of_every_resource_of_a_type_reads_its_columns_across_the_workspace() {
    let room = Room::new();
    let store = room.ada.host.store();
    let clock = Clock::at(NOW).unwrap();
    let select = |ty: &str, q: &str| -> Vec<String> {
        let meta = room.ty(ty);
        let resolved = resolve(&find(q).unwrap(), meta, &room.meta.enums, &clock).unwrap();
        let pred = store_pred(&resolved.pred, meta, APP).unwrap();
        let title = field_id(
            &room.meta,
            ty,
            if ty == "Task" { "title" } else { "subject" },
        );
        store
            .select(&meta.id, &pred, resolved.order.as_ref())
            .unwrap()
            .iter()
            .map(|r| match room.ada.host.runtime().read(r, &title) {
                Ok(Value::Text(t)) => t,
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(select("Task", "is:open sort:effort"), ["a", "c", "b"]);
    assert_eq!(select("Task", "status:waiting sort:-effort"), ["a", "d"]);
    // An identifier alone matches every identifier field of every type: a suffix is a run of
    // the column written backwards.
    let mut yc = select("Task", "*@yc.com");
    yc.extend(select("Message", "*@yc.com"));
    yc.sort();
    assert_eq!(yc, ["a", "d", "hello", "invoice"]);
    assert_eq!(select("Message", "from:*@yc.com"), ["invoice"]);
    assert_eq!(select("Message", "to:jane@yc.com"), ["hello"]);
    assert_eq!(select("Task", "tel:+49*"), ["b"]);
}

#[test]
fn facet_counts_are_read_from_the_columns() {
    let room = Room::new();
    let store = room.ada.host.store();
    let task = room.ty("Task");
    let field = |name: &str| field_id(&room.meta, "Task", name);
    let case = |name: &str| {
        room.meta
            .enums
            .iter()
            .flat_map(|e| e.variants.iter())
            .find(|v| v.name == name)
            .unwrap()
            .id
            .clone()
    };
    let _ = task;
    let status = store
        .facets(
            &room.board,
            &field_id(&room.meta, "Board", "tasks"),
            &field("status"),
        )
        .unwrap();
    assert_eq!(
        status,
        [
            (case("waiting"), 2),
            (case("started"), 1),
            (case("blocked"), 1)
        ]
    );
    let open = store
        .facets(
            &room.board,
            &field_id(&room.meta, "Board", "tasks"),
            "=open",
        )
        .unwrap();
    assert_eq!(open, [("1".to_owned(), 3), ("0".to_owned(), 1)]);
}
