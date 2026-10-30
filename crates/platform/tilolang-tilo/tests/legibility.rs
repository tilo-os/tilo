//! Seeing what a task did (task-legibility.md §2, §4): an agent's run creates, changes and
//! removes records in Notes and Tasks; every version names the run that made it and what it
//! did, removals included; the run's changes are listed across both apps; and what the person
//! has not seen is what the run did after their seen mark, until they review the run, edit
//! what it changed, or mark it seen.

use automerge::AutoCommit;
use tilolang_runtime::seen::{SeenMark, SeenWrite};
use tilolang_runtime::store::{Version, VersionKind};
use tilolang_runtime::{InvokeOutcome, Principal, ResourceRef, Store as _, Value};

use crate::fixtures::Replica;

const NOTES: &str = include_str!("../../../../apps/tl/notes/app.tl");
const NOTES_IDS: &str = include_str!("../../../../apps/tl/notes/app.ids");
const TASKS: &str = include_str!("../../../../apps/tl/tasks/app.tl");
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const NOTES_APP: &str = "so.tilo.lang.notes";
const TASKS_APP: &str = "so.tilo.lang.tasks";

const AGENT: &str = "agent:helper";
const RUN: &str = "issue-7@8000000000000001";
const LATER_RUN: &str = "issue-7@8000000000000002";

fn call(r: &Replica, who: &str, app: &str, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(app).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new(who), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

fn target(v: &Value) -> ResourceRef {
    match v {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

fn field(r: &Replica, app: &str, ty: &str, name: &str) -> String {
    crate::fixtures::field_id(&r.host.package(app).unwrap().meta, ty, name)
}

fn kinds(versions: &[Version]) -> Vec<VersionKind> {
    versions.iter().map(|v| v.kind).collect()
}

fn runs(versions: &[Version]) -> Vec<Option<&str>> {
    versions
        .iter()
        .map(|v| v.task.as_ref().map(|t| t.id.as_str()))
        .collect()
}

/// Ada's room after an agent's run tidied her notes and tasks.
struct Tidied {
    ada: Replica,
    /// The note the run made, the one it renamed and the one it deleted.
    made: ResourceRef,
    groceries: ResourceRef,
    old: ResourceRef,
    /// The task it added, the one it checked off and the one it removed.
    dentist: ResourceRef,
    landlord: ResourceRef,
    stale: ResourceRef,
}

impl Tidied {
    fn all(&self) -> [&ResourceRef; 6] {
        [
            &self.made,
            &self.groceries,
            &self.old,
            &self.dentist,
            &self.landlord,
            &self.stale,
        ]
    }
}

fn a_run_in_notes_and_tasks() -> Tidied {
    let (mut ada, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[(NOTES, NOTES_IDS), (TASKS, TASKS_IDS)],
    );
    ada.host
        .runtime()
        .set_clock(|| "2026-10-30T09:00:00Z".to_owned());
    let notes = Value::resource(&ada.host.root_of(NOTES_APP).unwrap());
    let tasks = Value::resource(&ada.host.root_of(TASKS_APP).unwrap());
    ada.commit();

    // What Ada had before the run: two notes and two tasks of her own.
    let groceries = call(&ada, "ada", NOTES_APP, "new_note", vec![notes.clone()]);
    ada.commit();
    call(
        &ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![groceries.clone(), Value::text("Groceries")],
    );
    let old = call(&ada, "ada", NOTES_APP, "new_note", vec![notes.clone()]);
    let landlord = call(
        &ada,
        "ada",
        TASKS_APP,
        "add",
        vec![tasks.clone(), Value::text("Call the landlord")],
    );
    let stale = call(
        &ada,
        "ada",
        TASKS_APP,
        "add",
        vec![tasks.clone(), Value::text("Stale")],
    );
    ada.commit();
    let (groceries, old, landlord, stale) = (
        target(&groceries),
        target(&old),
        target(&landlord),
        target(&stale),
    );
    // Nothing the person made herself is marked.
    for at in [&groceries, &old, &landlord, &stale] {
        assert!(ada.host.runtime().unseen("ada", at).unwrap().is_empty());
    }

    // The agent's run: a note made, one renamed and one deleted; a task added, one checked off
    // and one removed.
    ada.host
        .runtime()
        .set_clock(|| "2026-10-30T09:10:00Z".to_owned());
    let made = call(&ada, AGENT, NOTES_APP, "new_note", vec![notes.clone()]);
    call(
        &ada,
        AGENT,
        NOTES_APP,
        "retitle",
        vec![Value::resource(&groceries), Value::text("Shopping")],
    );
    call(
        &ada,
        AGENT,
        NOTES_APP,
        "delete",
        vec![notes, Value::resource(&old)],
    );
    let dentist = call(
        &ada,
        AGENT,
        TASKS_APP,
        "add",
        vec![tasks.clone(), Value::text("Book the dentist")],
    );
    call(
        &ada,
        AGENT,
        TASKS_APP,
        "check",
        vec![Value::resource(&landlord), Value::Bool(true)],
    );
    call(
        &ada,
        AGENT,
        TASKS_APP,
        "remove",
        vec![tasks, Value::resource(&stale)],
    );
    ada.commit_in_run(AGENT, RUN, "Tidy my notes and tasks");
    Tidied {
        made: target(&made),
        dentist: target(&dentist),
        ada,
        groceries,
        old,
        landlord,
        stale,
    }
}

#[test]
fn a_runs_versions_in_two_apps_name_the_run_and_what_it_did_removals_included() {
    let Tidied {
        ada,
        made,
        groceries,
        old,
        dentist,
        landlord,
        stale,
    } = a_run_in_notes_and_tasks();
    let store = ada.host.store();

    // Each version names the run that made it and what it did.
    let renamed = store.versions(&groceries).unwrap();
    assert_eq!(
        kinds(&renamed),
        [
            VersionKind::Created,
            VersionKind::Changed,
            VersionKind::Changed
        ]
    );
    assert_eq!(runs(&renamed), [None, None, Some(RUN)]);
    let last = renamed.last().unwrap();
    assert_eq!(last.who, AGENT);
    assert_eq!(last.fields, [field(&ada, NOTES_APP, "Note", "title")]);
    assert_eq!(last.task.as_ref().unwrap().title, "Tidy my notes and tasks");
    assert_eq!(
        kinds(&store.versions(&made).unwrap()),
        [VersionKind::Created]
    );
    assert_eq!(
        kinds(&store.versions(&dentist).unwrap()),
        [VersionKind::Created]
    );
    let checked = store.versions(&landlord).unwrap();
    assert_eq!(
        checked.last().unwrap().fields,
        [field(&ada, TASKS_APP, "Task", "done")]
    );
    // A removed record stays answerable: who removed it, in which run.
    for removed in [&old, &stale] {
        let versions = store.versions(removed).unwrap();
        let last = versions.last().unwrap();
        assert_eq!(last.kind, VersionKind::Removed, "{removed:?}: {versions:?}");
        assert_eq!(last.who, AGENT);
        assert_eq!(last.task.as_ref().map(|t| t.id.as_str()), Some(RUN));
    }

    // The run's changes, across both apps, oldest first: each resource it touched.
    let changes = store.run_changes(RUN).unwrap();
    let mut touched: Vec<String> = changes.iter().map(|v| v.resource.clone()).collect();
    touched.sort();
    touched.dedup();
    for at in [&made, &groceries, &old, &dentist, &landlord, &stale] {
        assert!(touched.contains(&at.id_string()), "{at:?} in {touched:?}");
    }
    assert!(changes.iter().all(|v| v.who == AGENT));
    assert!(changes.windows(2).all(|w| w[0].at <= w[1].at));
}

#[test]
fn what_a_run_did_stays_unseen_until_the_person_reviews_edits_or_marks_it() {
    let tidied = a_run_in_notes_and_tasks();
    let all = tidied.all().map(Clone::clone);
    let Tidied {
        mut ada,
        made,
        groceries,
        landlord,
        ..
    } = tidied;

    // Everything the run did is unseen by Ada.
    let runtime = ada.host.runtime();
    for at in &all {
        let unseen = runtime.unseen("ada", at).unwrap();
        assert_eq!(runs(&unseen), [Some(RUN)], "{at:?}");
    }

    // Editing a record marks what it shows as seen.
    call(
        &ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![Value::resource(&groceries), Value::text("Shopping list")],
    );
    ada.commit();
    let runtime = ada.host.runtime();
    assert!(runtime.unseen("ada", &groceries).unwrap().is_empty());
    // Marking one seen moves its mark to its newest version, kept for her own record.
    assert!(runtime.mark_seen("ada", &landlord).unwrap());
    assert!(runtime.unseen("ada", &landlord).unwrap().is_empty());
    let newest = ada.host.store().versions(&landlord).unwrap();
    assert_eq!(
        runtime.take_seen(),
        [SeenWrite::Resource {
            person: "ada".to_owned(),
            resource: landlord.id_string(),
            mark: SeenMark::of(newest.last().unwrap()),
        }]
    );
    // Mark all seen on the run: every version it made, in both apps, in one step; kept for
    // her own record as the run.
    assert!(runtime.mark_run_seen("ada", RUN));
    assert!(!runtime.mark_run_seen("ada", RUN), "reviewed once");
    for at in &all {
        assert!(runtime.unseen("ada", at).unwrap().is_empty(), "{at:?}");
    }
    assert_eq!(
        runtime.take_seen(),
        [SeenWrite::Run {
            person: "ada".to_owned(),
            run: RUN.to_owned(),
        }]
    );
    // Someone else's seen marks are their own: Ben has seen nothing of the run.
    assert_eq!(runs(&runtime.unseen("ben", &made).unwrap()), [Some(RUN)]);

    // A later run's change is unseen again, alone.
    call(
        &ada,
        AGENT,
        NOTES_APP,
        "retitle",
        vec![Value::resource(&made), Value::text("Meeting notes")],
    );
    ada.commit_in_run(AGENT, LATER_RUN, "Name the new note");
    let runtime = ada.host.runtime();
    assert_eq!(
        runs(&runtime.unseen("ada", &made).unwrap()),
        [Some(LATER_RUN)]
    );
    // The host keeps what she saw in her own record and gives it back on another device: the
    // runs and marks it sets are what `unseen` reads.
    let (mut other, _) = Replica::new(
        "ada",
        ada.doc.fork(),
        &[(NOTES, NOTES_IDS), (TASKS, TASKS_IDS)],
    );
    other.host.sync_from(&other.doc);
    let elsewhere = other.host.runtime();
    assert_eq!(elsewhere.unseen("ada", &made).unwrap().len(), 2);
    elsewhere.set_seen_runs("ada", &[RUN.to_owned()]);
    assert_eq!(
        runs(&elsewhere.unseen("ada", &made).unwrap()),
        [Some(LATER_RUN)]
    );
    let mark = SeenMark::of(ada.host.store().versions(&made).unwrap().last().unwrap());
    elsewhere.set_seen("ada", &made.id_string(), Some(mark));
    assert!(elsewhere.unseen("ada", &made).unwrap().is_empty());
    other.commit();
}

const SHELF: &str = r#"app so.tilo.lang.shelf "Shelf"

type Book {
    title: Text
}

type Shelf {
    books: Collection<Book>
}

root Shelf

default view shelf for Shelf

action shelve(s: Shelf, title: Text) -> Book {
    s.books.insert(Book(title: title))
}

action retitle(b: Book, title: Text) {
    b.title = title
}

action drop(s: Shelf, b: Book) {
    s.books.remove(b)
}

/// What the person looking has not seen of a book, and what the run that last changed it did
/// in all.
action describe(b: Book) -> Text {
    let fresh = unseen(b)
    if let last = history(b).last(), run = last.task {
        let field = last.fields.first() ?? "-"
        "{fresh.len()} unseen; last {last.kind} {field} by {run.title} ({run.task}); run made {changes(of: run).len()}"
    } else {
        "{fresh.len()} unseen; no run"
    }
}

view shelf(s: Shelf) {
    stack {
        for b in s.books {
            text(b.title)
        }
    }
}
"#;

#[test]
fn code_reads_a_versions_run_kind_and_fields_and_what_is_unseen() {
    let app = "so.tilo.lang.shelf";
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(SHELF, "")]);
    let shelf = Value::resource(&ada.host.root_of(app).unwrap());
    ada.commit();
    let book = call(
        &ada,
        "ada",
        app,
        "shelve",
        vec![shelf.clone(), Value::text("Dune")],
    );
    let gone = call(
        &ada,
        "ada",
        app,
        "shelve",
        vec![shelf.clone(), Value::text("Old")],
    );
    ada.commit();
    let describe = |r: &Replica, b: &Value| match call(r, "ada", app, "describe", vec![b.clone()]) {
        Value::Text(text) => text,
        other => panic!("{other:?}"),
    };
    assert_eq!(describe(&ada, &book), "0 unseen; no run");
    call(
        &ada,
        AGENT,
        app,
        "retitle",
        vec![book.clone(), Value::text("Dune Messiah")],
    );
    call(&ada, AGENT, app, "drop", vec![shelf, gone.clone()]);
    ada.commit_in_run(AGENT, RUN, "Sort the shelf");
    // The book, the removed one and the shelf whose collection changed.
    assert_eq!(
        describe(&ada, &book),
        "1 unseen; last changed title by Sort the shelf (issue-7); run made 3"
    );
    assert_eq!(
        describe(&ada, &gone),
        "1 unseen; last removed - by Sort the shelf (issue-7); run made 3"
    );
    assert!(ada.host.runtime().mark_run_seen("ada", RUN));
    assert!(describe(&ada, &book).starts_with("0 unseen"));
}
