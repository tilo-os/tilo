//! The index's filter columns (search.md, "Who indexes"): equality, ranges, membership and patterns
//! answered from runs of a column, in a collection and across the workspace, kept change by
//! change as a build from the record keeps them, with facet counts read from the same columns.

use std::rc::Rc;

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
    title: Text
    done: Bool = false
    due: Option<Date> = .none
    status: Status
    effort: Int
    contact: std.Identifier = std.Identifier("tel:+10")
    assignee: Option<Principal> = .none
    tags: Set<Text>

    search {
        filter "Open" = not done
        filter done
        filter due
        filter status
        filter effort
        filter contact
        filter assignee
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
    subject: Text
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

action assign(t: Task, who: Principal) {
    t.assignee = .some(who)
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

struct Workspace {
    ada: Replica,
    meta: Meta,
    board: ResourceRef,
}

impl Workspace {
    fn new() -> Self {
        Self::open(None)
    }

    /// As [`Self::new`], the app's actions reading the clock `at` from the seed on: what a test
    /// sorting by when a field changed needs, so its versions stand one after another.
    fn at(at: &str) -> Self {
        Self::open(Some(at))
    }

    fn open(at: Option<&str>) -> Self {
        let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TASKS, "")]);
        if let Some(at) = at.map(str::to_owned) {
            ada.host.set_clock(move || at.clone());
        }
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
    fn listing(&self, q: &str) -> Rc<tilolang_runtime::store::Listing> {
        let task = self.ty("Task");
        let resolved = resolve(
            &find(q).unwrap(),
            task,
            &self.meta.enums,
            &Clock::at(NOW).unwrap(),
            None,
        )
        .unwrap();
        let pred = store_pred(&resolved.pred, task, APP).unwrap();
        self.query(Some(&pred), resolved.order.as_ref())
    }

    /// The board's tasks the store lists for `pred` and `order`.
    fn query(
        &self,
        pred: Option<&tilolang_runtime::store::Pred>,
        order: Option<&tilolang_runtime::query::Order>,
    ) -> Rc<tilolang_runtime::store::Listing> {
        let task = self.ty("Task");
        let field = field_id(&self.meta, "Board", "tasks");
        let spec = FieldSpec {
            id: field,
            kind: FieldKind::Collection,
        };
        self.ada
            .host
            .store()
            .listing_query(&self.board, &spec, &task.id, pred, order)
            .unwrap()
    }

    /// The titles of the board's tasks the typed search `q` holds of, as the store lists them
    /// from its index.
    fn listed(&self, q: &str) -> Vec<String> {
        let task = self.ty("Task");
        self.listing(q)
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
            None,
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

    /// The index as kept, and as a store reading the workspace anew builds it.
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

/// An action of a change, and what it is given in the workspace as it is.
type Step = Box<dyn Fn(&Workspace) -> Vec<Value>>;

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
    let mut workspace = Workspace::new();
    for q in QUERIES {
        assert_eq!(workspace.listed(q), workspace.anew(q), "`{q}`");
    }
    assert_eq!(workspace.listed("status:waiting"), ["a", "d"]);
    assert_eq!(workspace.listed("is:open sort:-effort"), ["b", "c", "a"]);
    // A named filter, a value, a range in its own order and a membership are runs of a
    // column: no task is checked one by one.
    let before = workspace.evaluated();
    for q in [
        "is:open",
        "status:blocked",
        "effort:>3 sort:effort",
        "tags:home",
        "due:none",
    ] {
        workspace.listed(q);
    }
    assert_eq!(workspace.evaluated(), before, "answered from the columns");

    // Every kind of change, each followed by the columns as a build from the record finds.
    let board = workspace.board.clone();
    let steps: Vec<(&str, Step)> = vec![
        (
            "check",
            Box::new(|r: &Workspace| vec![Value::resource(&r.tasks()[0])]),
        ),
        (
            "set_effort",
            Box::new(|r: &Workspace| vec![Value::resource(&r.tasks()[2]), Value::Int(9)]),
        ),
        (
            "tag",
            Box::new(|r: &Workspace| vec![Value::resource(&r.tasks()[1]), Value::text("home")]),
        ),
        (
            "untag",
            Box::new(|r: &Workspace| vec![Value::resource(&r.tasks()[0]), Value::text("home")]),
        ),
        (
            "add",
            Box::new(move |_: &Workspace| {
                vec![Value::resource(&board), Value::text("e"), Value::Int(4)]
            }),
        ),
        (
            "move_first",
            Box::new(|r: &Workspace| {
                vec![Value::resource(&r.board), Value::resource(&r.tasks()[4])]
            }),
        ),
        (
            "remove",
            Box::new(|r: &Workspace| {
                vec![Value::resource(&r.board), Value::resource(&r.tasks()[2])]
            }),
        ),
        (
            "check",
            Box::new(|r: &Workspace| vec![Value::resource(&r.tasks()[0])]),
        ),
    ];
    for (action, args) in steps {
        let args = args(&workspace);
        workspace.call(action, args);
        for q in QUERIES {
            assert_eq!(
                workspace.listed(q),
                workspace.anew(q),
                "`{q}` after {action}"
            );
        }
        let (kept, anew) = workspace.kept_and_anew();
        assert_eq!(kept, anew, "the columns after {action}");
    }
}

/// A filtered listing keeps what it checked across a change: an item put in, one made to be of
/// the listing, one made not to be, and one taken out each list as the record read anew lists
/// them.
#[test]
fn a_listing_kept_across_a_change_lists_as_one_read_anew() {
    let mut workspace = Workspace::new();
    let q = "is:open sort:-effort";
    assert_eq!(workspace.listed(q), workspace.anew(q), "`{q}`");
    let board = workspace.board.clone();
    let steps: Vec<(&str, &str, Step)> = vec![
        (
            "put in",
            "add",
            Box::new(move |_: &Workspace| {
                vec![Value::resource(&board), Value::text("e"), Value::Int(4)]
            }),
        ),
        (
            "made to be of it",
            "check",
            Box::new(|r: &Workspace| vec![Value::resource(&r.tasks()[2])]),
        ),
        (
            "made not to be of it",
            "check",
            Box::new(|r: &Workspace| vec![Value::resource(&r.tasks()[0])]),
        ),
        (
            "taken out",
            "remove",
            Box::new(|r: &Workspace| {
                vec![Value::resource(&r.board), Value::resource(&r.tasks()[1])]
            }),
        ),
    ];
    for (case, action, args) in steps {
        workspace.call(action, args(&workspace));
        assert_eq!(
            workspace.listed(q),
            workspace.anew(q),
            "`{q}` after an item was {case}"
        );
    }
}

/// A listing ordered by when a field of an item changed stands what changed last first, read
/// from the index's stamps of those changes and no item of the workspace.
#[test]
fn a_listing_ordered_by_when_a_field_changed_reads_the_indexs_stamps() {
    // The tasks are seeded at 09:00, so their `done` was last changed by the version that made
    // them; the two people's checks come after, one at 10:00 and one at 10:05.
    let mut workspace = Workspace::at("2026-10-27T09:00:00Z");
    workspace
        .ada
        .host
        .set_clock(|| "2026-10-27T10:00:00Z".to_owned());
    workspace.call("check", vec![Value::resource(&workspace.tasks()[0])]);
    workspace
        .ada
        .host
        .set_clock(|| "2026-10-27T10:05:00Z".to_owned());
    workspace.call("check", vec![Value::resource(&workspace.tasks()[2])]);
    let task = workspace.ty("Task");
    let resolved = resolve(
        &find("is:done").unwrap(),
        task,
        &workspace.meta.enums,
        &Clock::at(NOW).unwrap(),
        None,
    )
    .unwrap();
    let pred = store_pred(&resolved.pred, task, APP).unwrap();
    let order = tilolang_runtime::query::Order {
        field: format!("$changed.{}", field_id(&workspace.meta, "Task", "done")),
        kind: FieldKind::Register,
        descending: true,
        absent_last: false,
        then: None,
    };
    let before = workspace.ada.host.store().read_stats();
    let ids = workspace.query(Some(&pred), Some(&order)).ids();
    let after = workspace.ada.host.store().read_stats();
    assert_eq!(
        after.doc.fields, before.doc.fields,
        "the order reads the index's stamps, not the items"
    );
    let titles: Vec<String> = ids
        .into_iter()
        .map(|id| workspace.title(&workspace.board.item(&id, task.id.clone())))
        .collect();
    assert_eq!(
        titles,
        ["c", "a", "d"],
        "the task checked last stands first"
    );
}

/// A field naming a person is a column like any other: a filter on it, or the one a named
/// filter asks for, is a run of it, so counting how many tasks a person holds reads no task.
#[test]
fn a_person_a_filter_names_is_a_run_of_its_column() {
    let mut workspace = Workspace::new();
    let first = workspace.tasks()[0].clone();
    workspace.call(
        "assign",
        vec![
            Value::resource(&first),
            Value::Principal(Principal::new("ada")),
        ],
    );
    assert_eq!(workspace.listed("assignee:ada"), ["a"]);
    assert_eq!(
        workspace.listed("assignee:ada"),
        workspace.anew("assignee:ada")
    );
    let before = workspace.evaluated();
    assert_eq!(workspace.listed("assignee:ada"), ["a"]);
    assert_eq!(workspace.evaluated(), before, "answered from the column");
    // How many it holds is the run's length, which the column knows: what a place's count reads.
    let listing = workspace.listing("assignee:ada");
    assert!(listing.len_is_exact());
    assert_eq!(listing.count(), 1);
    // Unassigned is the value every task without one holds.
    assert_eq!(workspace.listed("assignee:none"), ["b", "c", "d"]);
    assert_eq!(
        workspace.listed("assignee:none"),
        workspace.anew("assignee:none")
    );
    // The columns follow the change, as a build from the record finds them.
    workspace.call(
        "assign",
        vec![
            Value::resource(&workspace.tasks()[1].clone()),
            Value::Principal(Principal::new("ada")),
        ],
    );
    assert_eq!(workspace.listed("assignee:ada"), ["a", "b"]);
    let (kept, anew) = workspace.kept_and_anew();
    assert_eq!(kept, anew, "the columns after assigning");
}

#[test]
fn a_query_of_every_resource_of_a_type_reads_its_columns_across_the_workspace() {
    let workspace = Workspace::new();
    let store = workspace.ada.host.store();
    let clock = Clock::at(NOW).unwrap();
    let select = |ty: &str, q: &str| -> Vec<String> {
        let meta = workspace.ty(ty);
        let resolved =
            resolve(&find(q).unwrap(), meta, &workspace.meta.enums, &clock, None).unwrap();
        let pred = store_pred(&resolved.pred, meta, APP).unwrap();
        let title = field_id(
            &workspace.meta,
            ty,
            if ty == "Task" { "title" } else { "subject" },
        );
        store
            .select(&meta.id, &pred, resolved.order.as_ref())
            .unwrap()
            .iter()
            .map(|r| match workspace.ada.host.runtime().read(r, &title) {
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
    let workspace = Workspace::new();
    let store = workspace.ada.host.store();
    let task = workspace.ty("Task");
    let field = |name: &str| field_id(&workspace.meta, "Task", name);
    let case = |name: &str| {
        workspace
            .meta
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
            &workspace.board,
            &field_id(&workspace.meta, "Board", "tasks"),
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
            &workspace.board,
            &field_id(&workspace.meta, "Board", "tasks"),
            "=open",
        )
        .unwrap();
    assert_eq!(open, [("1".to_owned(), 3), ("0".to_owned(), 1)]);
}

/// A type whose inbox filter is one membership run and two other conjuncts, one of them the
/// clock: a snooze that has ended, and a mute that holds.
const SNOOZED: &str = r#"app so.tilo.inbox "Inbox"

type Thread {
    title: Text
    boxes: Set<Text>
    snoozed_until: Time = Time("1970-01-01T00:00:00Z")
    muted: Bool = false

    search {
        filter "Inbox" = boxes.contains("inbox") and snoozed_until <= now and not muted
    }
}

type Mail {
    threads: Collection<Thread>
}

root Mail

default view main for Mail

view main(m: Mail) {
    page {
        for t in m.threads {
            text(t.title)
        }
    }
}
"#;
const INBOX_APP: &str = "so.tilo.inbox";

/// A workspace whose `threads` collection holds `n` conversations, every fifth snoozed far ahead:
/// what the paging test reads.
fn inbox_workspace(n: usize) -> (Replica, Meta, ResourceRef) {
    inbox_workspace_of(SNOOZED, n, |i, item, meta| {
        use serde_json::json;
        use tilolang_tilo::record::DynValue;
        if let Some(DynValue::Register(snoozed)) =
            item.get_mut(&field_id(meta, "Thread", "snoozed_until"))
            && i % 5 == 0
        {
            snoozed.clone_from(&json!("2030-01-01T00:00:00Z"));
        }
        if let Some(DynValue::Register(muted)) = item.get_mut(&field_id(meta, "Thread", "muted")) {
            muted.clone_from(&json!(i % 3 == 0));
        }
    })
}

/// A workspace of `source`'s package whose `threads` collection holds `n` conversations, all in the
/// inbox, each set up by `shape`. Made by one change that creates the root whole, as the bench
/// seeds a workspace, so the size costs one write.
fn inbox_workspace_of(
    source: &str,
    n: usize,
    shape: impl Fn(usize, &mut tilolang_tilo::DynRecord, &Meta),
) -> (Replica, Meta, ResourceRef) {
    use serde_json::json;
    use tilo_resource::{Collection, Id, Model as _};
    use tilolang_tilo::action::{DynAction, DynEdit};
    use tilolang_tilo::record::{DynRecord, DynValue};

    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(source, "")]);
    // The index writes what the filters that read the clock hold at the workspace's clock: a
    // query that answers from it is asked at the same time.
    ada.host.set_clock(|| NOW.to_owned());
    let meta = packages[0].meta.clone();
    let mail = ada.host.root_of(INBOX_APP).unwrap();
    ada.commit();
    let mail_ty = meta.types.iter().find(|t| t.name == "Mail").unwrap();
    let thread_ty = meta.types.iter().find(|t| t.name == "Thread").unwrap();
    let schema = tilolang_tilo::schema::lookup(&thread_ty.id).unwrap();
    let mut threads = Collection::default();
    for i in 0..n {
        let mut item = DynRecord::empty(schema).unwrap();
        if let Some(DynValue::Text(title)) = item.get_mut(&field_id(&meta, "Thread", "title")) {
            *title = format!("Thread {i:05}");
        }
        if let Some(DynValue::Set(boxes)) = item.get_mut(&field_id(&meta, "Thread", "boxes")) {
            boxes.insert(json!("inbox"));
        }
        shape(i, &mut item, &meta);
        threads.push(Id::from_raw(format!("thread-{i:05}")), item);
    }
    let mut root = DynRecord::empty(tilolang_tilo::schema::lookup(&mail_ty.id).unwrap()).unwrap();
    if let Some(DynValue::Collection(slot)) = root.get_mut(&field_id(&meta, "Mail", "threads")) {
        *slot = threads;
    }
    let create = DynAction {
        app: INBOX_APP.to_owned(),
        kind: "tl.so.tilo.inbox.seed".to_owned(),
        action_id: "$create".to_owned(),
        args: serde_json::Value::Null,
        authorship: tilolang_tilo::Authorship::default(),
        edits: vec![DynEdit::Create {
            id: mail.id_string(),
            record: root.to_json(),
        }],
    };
    ada.apply(&create);
    ada.commit();
    (ada, meta, mail)
}
/// The listing of the inbox filter over `mail`.
fn inbox_listing(
    ada: &Replica,
    meta: &Meta,
    mail: &ResourceRef,
) -> Rc<tilolang_runtime::store::Listing> {
    let thread = meta.types.iter().find(|t| t.name == "Thread").unwrap();
    let resolved = resolve(
        &find("is:inbox").unwrap(),
        thread,
        &meta.enums,
        &Clock::at(NOW).unwrap(),
        None,
    )
    .unwrap();
    let pred = store_pred(&resolved.pred, thread, INBOX_APP).unwrap();
    let spec = FieldSpec {
        id: field_id(meta, "Mail", "threads"),
        kind: FieldKind::Collection,
    };
    ada.host
        .store()
        .listing_query(
            mail,
            &spec,
            &thread.id,
            Some(&pred),
            resolved.order.as_ref(),
        )
        .unwrap()
}

/// The inbox filter's first page of `workspace`, and how many conversations it checked one by one
/// to fill it.
fn inbox_page(ada: &Replica, meta: &Meta, mail: &ResourceRef) -> (Vec<String>, u64) {
    let thread = meta.types.iter().find(|t| t.name == "Thread").unwrap();
    let before = ada.host.store().read_stats().evaluated;
    let listing = inbox_listing(ada, meta, mail);
    // What the first page reads: the rows a window shows first.
    let title = field_id(meta, "Thread", "title");
    let items = (0..40)
        .map_while(|index| listing.get(index))
        .map(|id| {
            let item = mail.item(&id, thread.id.clone());
            match ada.host.runtime().read(&item, &title) {
                Ok(Value::Text(t)) => t,
                other => panic!("{other:?}"),
            }
        })
        .collect();
    let checked = ada
        .host
        .store()
        .read_stats()
        .evaluated
        .saturating_sub(before);
    (items, checked)
}

#[test]
fn a_filter_of_a_run_and_a_clock_conjunct_pages_at_the_first_page_and_lists_what_holds() {
    // The workspace of 10k conversations: the inbox run, and the two conjuncts checked as its
    // pages fill, cost the page read, not the workspace.
    let (ada, meta, mail) = inbox_workspace(10_000);
    let (items, checked) = inbox_page(&ada, &meta, &mail);
    assert_eq!(items.len(), 40, "a first page of the 10k");
    assert!(
        checked <= 60,
        "filling the first page checked {checked} of 10,000"
    );
    // Every third conversation is muted (3,334) and every fifth snoozed (2,000), 667 of them
    // both: 4,667 out of the inbox's 10,000, which leaves 5,333. The filter is a column like
    // any other, written at the clock it is asked at, so the listing is sized by its count:
    // it is that many, and a badge reads that without reading the workspace.
    let listing = inbox_listing(&ada, &meta, &mail);
    assert_eq!(listing.len(), 5_333);
    assert!(listing.len_is_exact(), "the list says how many it holds");
    assert_eq!(listing.count(), 5_333);
}

#[test]
fn a_clock_filtered_count_is_exact_at_every_size() {
    // The same inbox at 1k and at 10k, its count read from the filter's column: the
    // conversations neither muted (every third) nor snoozed (every fifth).
    for n in [1_000usize, 10_000] {
        let held = (0..n).filter(|i| i % 3 != 0 && i % 5 != 0).count();
        let (ada, meta, mail) = inbox_workspace(n);
        let listing = inbox_listing(&ada, &meta, &mail);
        assert_eq!(listing.len(), held, "{n} conversations");
        assert!(listing.len_is_exact(), "{n} conversations");
        assert_eq!(listing.count(), held, "{n} conversations");
    }
}

/// The view that lists the inbox the way the Mail window does: `threads.matching("is:inbox")`.
const INBOX_VIEW: &str = r#"app so.tilo.inbox "Inbox"

type Thread {
    title: Text
    boxes: Set<Text>
    snoozed_until: Time = Time("1970-01-01T00:00:00Z")
    muted: Bool = false

    search {
        filter "Inbox" = boxes.contains("inbox") and snoozed_until <= now and not muted
    }
}

type Mail {
    threads: Collection<Thread>
}

root Mail

default view main for Mail

view main(m: Mail) {
    page {
        for t in m.threads.matching("is:inbox") {
            text(t.title)
        }
    }
}
"#;

/// The titles a view shows.
fn texts(tree: &tilolang_runtime::UiNode) -> Vec<String> {
    fn walk(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.Text"
            && let Some(Value::Text(text)) = node.props.get("value")
        {
            out.push(text.clone());
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

#[test]
fn a_snooze_ending_moves_its_thread_into_the_inbox_without_reading_the_workspace_again() {
    use std::cell::RefCell;
    use std::rc::Rc;

    // A workspace of 1,000 conversations in the inbox, the first snoozed until 2027: the view
    // starts with the other 999.
    let clock = Rc::new(RefCell::new(NOW.to_owned()));
    let read = Rc::clone(&clock);
    let (ada, _meta, _mail) = inbox_workspace_of(INBOX_VIEW, 1_000, |i, item, meta| {
        use serde_json::json;
        use tilolang_tilo::record::DynValue;
        // The first conversation is snoozed until 2027, and comes back once it is there.
        if i == 0
            && let Some(DynValue::Register(snoozed)) =
                item.get_mut(&field_id(meta, "Thread", "snoozed_until"))
        {
            snoozed.clone_from(&json!("2027-01-01T00:00:00Z"));
        }
    });
    ada.host.set_clock(move || read.borrow().clone());
    let mut view = ada.view(INBOX_APP);
    assert_eq!(texts(view.tree()).len(), 999);
    // An idle view waits for the instant the snooze ends, not for the next minute: nothing reads
    // it in between, and it wakes exactly there.
    let ends = tilolang::calendar::instant("2027-01-01T00:00:00Z").unwrap();
    assert_eq!(view.wakes_at(), Some(ends));
    *clock.borrow_mut() = "2026-12-31T23:59:00Z".to_owned();
    assert!(!view.is_stale(), "the view woke before the snooze ended");

    // The clock reaches the snooze's end: the view runs again, and the conversation moves in,
    // its row built (one item read), not the workspace read again.
    *clock.borrow_mut() = "2027-01-01T00:00:00Z".to_owned();
    assert!(view.is_stale(), "the snooze's end opened the inbox");
    let before = ada.host.store().read_stats().evaluated;
    view.poll_patches().unwrap();
    assert_eq!(texts(view.tree()).len(), 1_000);
    // The listing's first page was filled again, and the item that came in checked: the
    // entries read are the page's, not the workspace's.
    let filled = ada
        .host
        .store()
        .read_stats()
        .evaluated
        .saturating_sub(before);
    assert!(filled <= 2, "the change checked {filled} items");
    // No value the filter compares lies after it: the view has nothing to wait for, and no
    // minute of the clock wakes it again.
    assert_eq!(
        view.wakes_at(),
        None,
        "the inbox waits for no time it reads"
    );
}

/// The inbox's app ([`INBOX_VIEW`]) with what a person does to a conversation: snooze it until a
/// moment.
fn snoozing() -> String {
    format!(
        "{INBOX_VIEW}\naction snooze(t: Thread, until: Time) {{\n    t.snoozed_until = until\n}}\n"
    )
}

// USER REQUIREMENT: a conversation snoozed just now comes back at its moment, with no window
// open: the window waits for that moment, and the store's own write holds it before the record
// brings it back.
#[test]
fn a_snooze_an_action_just_set_is_the_moment_the_view_waits_for() {
    let (mut ada, meta, mail) = inbox_workspace_of(&snoozing(), 3, |_, _, _| {});
    let thread = meta.types.iter().find(|t| t.name == "Thread").unwrap();
    assert_eq!(
        texts(ada.view(INBOX_APP).tree()).len(),
        3,
        "three conversations are in the inbox"
    );
    let first = inbox_listing(&ada, &meta, &mail)
        .ids()
        .into_iter()
        .next()
        .map(|id| mail.item(&id, thread.id.clone()))
        .expect("the inbox lists a conversation");
    let moment = "2027-01-01T00:00:00Z";
    let action = meta.actions.iter().find(|a| a.name == "snooze").unwrap();
    ada.host
        .runtime()
        .invoke_action(
            &Principal::new("ada"),
            (
                action.id.as_str(),
                vec![Value::resource(&first), Value::text(moment)],
            ),
        )
        .unwrap();
    // The write is the store's: the record has not brought it back yet, and the view that reads
    // the inbox waits for the moment the conversation comes back.
    let view = ada.view(INBOX_APP);
    assert_eq!(
        texts(view.tree()).len(),
        2,
        "the snoozed conversation is out of the inbox"
    );
    assert_eq!(
        view.wakes_at(),
        tilolang::calendar::instant(moment),
        "the snooze the action set is the moment the view waits for"
    );
    ada.commit();
    assert_eq!(
        view.wakes_at(),
        tilolang::calendar::instant(moment),
        "and it is once the record holds it"
    );
}

/// The inbox filter's answers for a clock that runs behind the process's own, as a test's pinned
/// clock and a device whose clock was corrected backwards do.
#[test]
fn a_read_at_an_instant_behind_the_one_the_index_wrote_at_answers_for_that_instant() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let clock = Rc::new(RefCell::new(NOW.to_owned()));
    let read = Rc::clone(&clock);
    // One of the two conversations is snoozed until a moment the fake instant is before and the
    // process's own clock is after: out of the inbox at the fake one, in it at the later one.
    let snoozed = "2026-11-01T00:00:00Z";
    let (mut ada, meta, mail) = inbox_workspace_of(SNOOZED, 2, |i, item, meta| {
        use serde_json::json;
        use tilolang_tilo::record::DynValue;
        if i == 0
            && let Some(DynValue::Register(until)) =
                item.get_mut(&field_id(meta, "Thread", "snoozed_until"))
        {
            until.clone_from(&json!(snoozed));
        }
    });
    ada.host.set_clock(move || read.borrow().clone());
    assert_eq!(
        inbox_listing(&ada, &meta, &mail).count(),
        1,
        "at the fake instant the snoozed conversation is out of the inbox"
    );

    // Read at the process's own clock, which is past the snooze's moment: the conversation is in
    // the inbox, and the index records the instant it wrote the columns at.
    let own = tilolang_tilo::host::system_time();
    assert!(
        tilolang::calendar::instant(&own) > tilolang::calendar::instant(snoozed),
        "the process's clock runs past the moment the fake instant snoozes until: {own}"
    );
    *clock.borrow_mut() = own;
    assert_eq!(
        inbox_listing(&ada, &meta, &mail).count(),
        2,
        "at the process's clock the snooze's moment has passed"
    );

    // Read again at an instant behind the one the index wrote at: what the filter holds is what
    // that instant asks for, not what the later one did.
    *clock.borrow_mut() = NOW.to_owned();
    assert_eq!(
        inbox_listing(&ada, &meta, &mail).count(),
        1,
        "a read behind the instant the index wrote at answers for its own"
    );

    // A conversation written at the fake instant, after the index wrote at the later one, is
    // written at the instant the write read: the read between the two answers for its own.
    let thread_id = meta
        .types
        .iter()
        .find(|t| t.name == "Thread")
        .unwrap()
        .id
        .clone();
    let mut late =
        tilolang_tilo::DynRecord::empty(tilolang_tilo::schema::lookup(&thread_id).unwrap())
            .unwrap();
    if let Some(tilolang_tilo::record::DynValue::Register(until)) =
        late.get_mut(&field_id(&meta, "Thread", "snoozed_until"))
    {
        until.clone_from(&serde_json::json!("2026-11-05T00:00:00Z"));
    }
    if let Some(tilolang_tilo::record::DynValue::Set(boxes)) =
        late.get_mut(&field_id(&meta, "Thread", "boxes"))
    {
        boxes.insert(serde_json::json!("inbox"));
    }
    ada.apply(&tilolang_tilo::action::DynAction {
        app: INBOX_APP.to_owned(),
        kind: "tl.so.tilo.inbox.thread".to_owned(),
        action_id: "$add".to_owned(),
        args: serde_json::Value::Null,
        authorship: tilolang_tilo::Authorship::default(),
        edits: vec![tilolang_tilo::action::DynEdit::Insert {
            id: mail.id_string(),
            item: "thread-late".to_owned(),
            record: tilo_resource::Model::to_json(&late),
            at: tilolang_tilo::runtime::place::Placement::At {
                place: tilolang_tilo::runtime::place::Place {
                    parent: None,
                    field: field_id(&meta, "Mail", "threads"),
                    key: "0000000002".to_owned(),
                },
                was: None,
            }
            .encode(),
        }],
    });
    ada.commit();
    *clock.borrow_mut() = "2026-11-06T00:00:00Z".to_owned();
    assert_eq!(
        inbox_listing(&ada, &meta, &mail).count(),
        3,
        "a write at the fake instant holds of its filter from there, not from the later one"
    );
}

/// A list filtered by a set's member and more, of conversations the person's device has just
/// written: the inbox of a mailbox seeded a moment ago.
const JUST_WRITTEN: &str = r#"app so.tilo.inbox "Inbox"

type Thread {
    title: Text
    unread: Bool
    boxes: Set<Text>

    search {
        filter "Inbox" = boxes.contains("inbox") and unread
    }
}

type Mail {
    threads: Collection<Thread>
}

root Mail

default view main for Mail

view main(m: Mail) {
    page {
        for t in m.threads.matching("is:inbox") {
            text(t.title)
        }
    }
}

scenario "a morning's mail" {
    let m = root Mail
    let launch = m.threads.insert(Thread(title: "Launch plan", unread: true))
    launch.boxes.add("inbox")
    let lease = m.threads.insert(Thread(title: "Lease renewal", unread: false))
    lease.boxes.add("inbox")
    let invoice = m.threads.insert(Thread(title: "Invoice", unread: true))
    invoice.boxes.add("archive")
}
"#;

/// The conversations a person's device has just written are in the list a filter of a set's
/// member and another conjunct shows, before the index holds them as well as after: the run
/// of the member is as long as the writes leave it, not as the index counted it, which left
/// a just-seeded inbox empty.
#[test]
fn a_filtered_list_shows_what_was_just_written_before_and_after_it_is_indexed() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(JUST_WRITTEN, "")]);
    let scenario = packages[0].meta.scenarios[0].action.clone();
    ada.host.root_of(INBOX_APP).unwrap();
    ada.commit();
    ada.host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.as_str(), vec![]))
        .unwrap();
    assert_eq!(
        texts(ada.view(INBOX_APP).tree()),
        ["Launch plan"],
        "written and not yet indexed"
    );
    ada.commit();
    assert_eq!(
        texts(ada.view(INBOX_APP).tree()),
        ["Launch plan"],
        "indexed"
    );
}
