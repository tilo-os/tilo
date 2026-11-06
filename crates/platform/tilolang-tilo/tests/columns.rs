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
    title: Text (title)
    done: Bool (done) = false
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
    title: Text (title)
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
    // both: 4,667 out of the inbox's 10,000, which leaves 5,333. The count the run's column
    // keeps is the most it can hold; read to its end, the listing says it is exactly that.
    let listing = inbox_listing(&ada, &meta, &mail);
    assert_eq!(listing.len(), 10_000);
    assert!(
        !listing.len_is_exact(),
        "the list says its length is up to that many"
    );
    let kept = (0..listing.len())
        .filter_map(|index| listing.get(index))
        .count();
    assert_eq!(kept, 5_333);
    assert!(listing.len_is_exact(), "every entry is checked now");
}

/// The view that lists the inbox the way the Mail window does: `threads.matching("is:inbox")`.
const INBOX_VIEW: &str = r#"app so.tilo.inbox "Inbox"

type Thread {
    title: Text (title)
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
    let clock = Rc::new(RefCell::new("2026-06-29T09:00:00Z".to_owned()));
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
    ada.host.runtime().set_clock(move || read.borrow().clone());
    let mut view = ada.view(INBOX_APP);
    assert_eq!(texts(view.tree()).len(), 999);
    // A query reads the running clock to the minute: the view waits for the next one.
    assert_eq!(
        view.wakes_at(),
        tilolang::calendar::instant("2026-06-29T09:01:00Z")
    );

    // The clock reaches the snooze's end: the view runs again, and the conversation moves in,
    // its row built (one item read), not the workspace read again.
    *clock.borrow_mut() = "2027-06-01T00:00:00Z".to_owned();
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
}

/// A list filtered by a set's member and more, of conversations the person's device has just
/// written: the inbox of a mailbox seeded a moment ago.
const JUST_WRITTEN: &str = r#"app so.tilo.inbox "Inbox"

type Thread {
    title: Text (title)
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
