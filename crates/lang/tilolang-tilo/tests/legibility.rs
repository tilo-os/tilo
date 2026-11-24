//! Seeing what a task did (task-legibility.md §2, §4): an agent's run creates, changes and
//! removes documents in Notes and Tasks; every version names the run that made it and what it
//! did, removals included; the run's changes are listed across both apps; and what the person
//! has not seen is what the run did after their seen mark, until they review the run, edit
//! what it changed, or mark it seen.

use crate::fixtures::Workspace;
use tilolang_runtime::seen::{SeenMark, SeenWrite};
use tilolang_runtime::store::{Version, VersionKind};
use tilolang_runtime::{Actor, ResourceRef, Store as _, Value};

use tilolang_tilo::host::Host;

use crate::fixtures::{Replica, TASKS_SET, call, target};

pub(crate) const NOTES_APP: &str = "so.tilo.notes";
pub(crate) const TASKS_APP: &str = "so.tilo.tasks";

pub(crate) const AGENT: &str = "helper";

/// The agent's run: the work it was asked, and its run of it.
pub(crate) const RUN: &str = "issue-7@8000000000000001";

/// The agent that tidies: [`AGENT`] as the runtime names it.
pub(crate) fn helper() -> tilolang_tilo::runtime::Actor {
    tilolang_tilo::runtime::ids::MemberId::agent(AGENT).into()
}

/// What is written from now on in `replica` is a step of the agent's run [`RUN`], titled
/// `title`, as the host stamps the writes of an agent's session.
pub(crate) fn in_run(replica: &Replica, title: &str) {
    replica
        .host
        .store()
        .set_run(Some(tilolang_tilo::runtime::store::Run {
            id: RUN.to_owned(),
            title: title.to_owned(),
        }));
}

fn field(r: &Replica, app: &str, ty: &str, name: &str) -> String {
    crate::fixtures::field_id(&r.host.package(app).unwrap().meta, ty, name)
}

fn runs(versions: &[Version]) -> Vec<Option<&str>> {
    versions
        .iter()
        .map(|v| v.run.as_ref().map(|t| t.id.as_str()))
        .collect()
}

fn kinds(versions: &[Version]) -> Vec<VersionKind> {
    versions.iter().map(|v| v.kind).collect()
}

/// Ada's workspace after an agent's run tidied her notes and tasks.
pub(crate) struct Tidied {
    pub(crate) ada: Replica,
    /// The note the run made, the one it renamed and the one it deleted.
    pub(crate) made: ResourceRef,
    pub(crate) groceries: ResourceRef,
    pub(crate) old: ResourceRef,
    /// The task it added, the one it checked off and the one it removed.
    pub(crate) dentist: ResourceRef,
    pub(crate) landlord: ResourceRef,
    pub(crate) stale: ResourceRef,
}

impl Tidied {
    pub(crate) fn all(&self) -> [&ResourceRef; 6] {
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

/// Ada marks seen everything the agent changed, as "Mark all seen" on the run does.
pub(crate) fn mark_all_seen(tidied: &Tidied) {
    let me = tilolang_tilo::personal::actor("ada");
    for at in tidied.all() {
        let _ = tidied.ada.host.runtime().mark_seen(&me, at);
    }
}

/// Ada's workspace as another of her devices opens it over the same store: the same lists, and
/// what a list shows of what left it.
pub(crate) fn built_over(ada: &Replica) -> Host {
    let host = Host::open(
        ada.doc.store(),
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("phone")),
        None,
        || None,
    )
    .expect("the workspace opens");
    for (source, ids) in TASKS_SET {
        host.load_builtin(source, ids).expect("the package loads");
    }
    host
}

pub(crate) fn an_agent_tidies_notes_and_tasks() -> Tidied {
    let (ada, _) = Replica::new("ada", Workspace::new(), TASKS_SET);
    ada.host.set_clock(|| "2026-10-30T09:00:00Z".to_owned());
    let notes = Value::resource(&ada.host.root_of(NOTES_APP).unwrap());
    let tasks = Value::resource(&ada.host.root_of(TASKS_APP).unwrap());
    ada.commit();

    // What Ada had before the run: two notes and two tasks of her own.
    let groceries = call(&ada, "ada", NOTES_APP, "new_note", vec![notes.clone()]);
    ada.commit();
    let named = vec![groceries.clone(), Value::text("Groceries")];
    call(&ada, "ada", NOTES_APP, "retitle", named);
    let old = call(&ada, "ada", NOTES_APP, "new_note", vec![notes.clone()]);
    let landlord = call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Call the landlord"),
    );
    let stale = call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Stale"),
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
        assert!(
            ada.host
                .runtime()
                .unseen(&tilolang_tilo::personal::actor("ada"), at)
                .unwrap()
                .is_empty()
        );
    }

    // The agent's run: a note made, one renamed and one deleted; a task added, one checked off
    // and one removed.
    ada.host.set_clock(|| "2026-10-30T09:10:00Z".to_owned());
    in_run(&ada, "Tidy my notes and tasks");
    let made = call(&ada, &helper(), NOTES_APP, "new_note", vec![notes.clone()]);
    call(
        &ada,
        &helper(),
        NOTES_APP,
        "retitle",
        vec![Value::resource(&groceries), Value::text("Shopping")],
    );
    call(
        &ada,
        &helper(),
        NOTES_APP,
        "delete",
        vec![notes, Value::resource(&old)],
    );
    let dentist = call(
        &ada,
        &helper(),
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Book the dentist"),
    );
    call(
        &ada,
        &helper(),
        TASKS_APP,
        "check",
        vec![Value::resource(&landlord), Value::Bool(true)],
    );
    call(
        &ada,
        &helper(),
        TASKS_APP,
        "remove",
        vec![Value::resource(&stale)],
    );
    ada.host.store().set_run(None);
    ada.commit();
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
fn an_agents_versions_in_two_apps_name_what_made_them_and_what_they_did_removals_included() {
    let Tidied {
        ada,
        made,
        groceries,
        old,
        dentist,
        landlord,
        stale,
    } = an_agent_tidies_notes_and_tasks();
    let store = ada.host.store();

    // Each version names who made it and what it did.
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
    assert_eq!(last.who, Some(helper()));
    assert_eq!(last.fields, [field(&ada, NOTES_APP, "Note", "title")]);
    assert_eq!(last.run.as_ref().unwrap().title, "Tidy my notes and tasks");
    assert_eq!(
        kinds(&store.versions(&made).unwrap()),
        [VersionKind::Created]
    );
    assert_eq!(
        kinds(&store.versions(&dentist).unwrap()),
        [VersionKind::Created]
    );
    let checked = store.versions(&landlord).unwrap();
    // Checking writes what the person changed — the box and the status the flag stands beside,
    // and nothing else: whether it folds is worked out from what is under and above it, not
    // written with it, and when it changed and who changed it are the store's version, never a
    // field.
    let mut written = checked.last().unwrap().fields.clone();
    written.sort();
    let mut expected = vec![
        field(&ada, TASKS_APP, "Task", "done"),
        field(&ada, TASKS_APP, "Task", "status"),
    ];
    expected.sort();
    assert_eq!(written, expected);
    // A removed resource stays answerable: who removed it.
    for removed in [&old, &stale] {
        let versions = store.versions(removed).unwrap();
        let last = versions.last().unwrap();
        assert_eq!(last.kind, VersionKind::Removed, "{removed:?}: {versions:?}");
        assert_eq!(last.who, Some(helper()));
        assert_eq!(last.run.as_ref().map(|t| t.id.as_str()), Some(RUN));
    }

    // The run's changes, across both apps, oldest first: each resource it touched.
    let changes = store.run_changes(RUN).unwrap();
    let mut touched: Vec<String> = changes.iter().map(|v| v.resource.clone()).collect();
    touched.sort();
    touched.dedup();
    for at in [&made, &groceries, &old, &dentist, &landlord, &stale] {
        assert!(touched.contains(&at.id_string()), "{at:?} in {touched:?}");
    }
    assert!(changes.iter().all(|v| v.who == Some(helper())));
    assert!(changes.windows(2).all(|w| w[0].at <= w[1].at));
}

#[test]
fn what_a_run_did_stays_unseen_until_the_person_reviews_edits_or_marks_it() {
    let tidied = an_agent_tidies_notes_and_tasks();
    let all = tidied.all().map(Clone::clone);
    let Tidied {
        ada,
        made,
        groceries,
        landlord,
        ..
    } = tidied;
    let me = tilolang_tilo::personal::actor("ada");

    // Everything the agent did is unseen by Ada: one version of each resource it changed.
    let runtime = ada.host.runtime();
    for at in &all {
        let unseen = runtime.unseen(&me, at).unwrap();
        assert_eq!(unseen.len(), 1, "{at:?}: {unseen:?}");
        assert_eq!(unseen[0].who, Some(helper()), "{at:?}");
    }

    // Editing a resource marks what it shows as seen.
    call(
        &ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![Value::resource(&groceries), Value::text("Shopping list")],
    );
    ada.commit();
    let runtime = ada.host.runtime();
    assert!(runtime.unseen(&me, &groceries).unwrap().is_empty());
    // Marking one seen moves its mark to its newest version, kept for her own state.
    assert!(runtime.mark_seen(&me, &landlord).unwrap());
    assert!(runtime.unseen(&me, &landlord).unwrap().is_empty());
    let newest = ada.host.store().versions(&landlord).unwrap();
    assert_eq!(
        runtime.take_seen(),
        [SeenWrite::Resource {
            person: me.clone(),
            resource: landlord.id_string(),
            mark: Some(SeenMark::of(newest.last().unwrap())),
        }]
    );
    // Someone else's seen marks are their own: Ben has seen none of it.
    assert_eq!(
        runtime
            .unseen(&tilolang_tilo::personal::actor("ben"), &made)
            .unwrap()
            .len(),
        1
    );

    // A later change of the agent's is unseen again, alone.
    call(
        &ada,
        &helper(),
        NOTES_APP,
        "retitle",
        vec![Value::resource(&made), Value::text("Meeting notes")],
    );
    let runtime = ada.host.runtime();
    assert!(
        runtime
            .unseen(&me, &made)
            .unwrap()
            .iter()
            .all(|version| version.who == Some(helper())),
    );
    // Another device of hers reads the same versions of the workspace and marks what she saw
    // there, as the runtime hands the host's marks to it.
    let (other, _) = Replica::new("ada", ada.doc.fork(), TASKS_SET);
    let elsewhere = other.host.runtime();
    assert_eq!(elsewhere.unseen(&me, &made).unwrap().len(), 2);
    let mark = SeenMark::of(ada.host.store().versions(&made).unwrap().last().unwrap());
    elsewhere.set_seen(&me, &made.id_string(), Some(mark));
    assert!(elsewhere.unseen(&me, &made).unwrap().is_empty());
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
    if let last = history(b).last(), run = last.run {
        let field = last.fields.first() ?? "-"
        "{fresh.len()} unseen; last {last.kind} {field} by {run.title} ({run.of}); run made {changes(of: run).len()}"
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

tilo_test_env::scenario! {
    fn a_runs_versions_are_read_by_code_and_marked_seen;

    phase code_reads_a_versions_run_kind_and_fields_and_what_is_unseen;
    {
        let app = "so.tilo.lang.shelf";
        let (ada, _) = Replica::new("ada", Workspace::new(), &[(SHELF, "")]);
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
        in_run(&ada, "Sort the shelf");
        call(
            &ada,
            &helper(),
            app,
            "retitle",
            vec![book.clone(), Value::text("Dune Messiah")],
        );
        call(&ada, &helper(), app, "drop", vec![shelf, gone.clone()]);
        ada.host.store().set_run(None);
        ada.commit();
        // The book, the removed one and the shelf whose collection changed.
        assert_eq!(
            describe(&ada, &book),
            "1 unseen; last changed title by Sort the shelf (issue-7); run made 3"
        );
        assert_eq!(
            describe(&ada, &gone),
            "1 unseen; last removed - by Sort the shelf (issue-7); run made 3"
        );
        assert!(ada.host.runtime().mark_run_seen(&tilolang_tilo::personal::actor("ada"), RUN));
        assert!(describe(&ada, &book).starts_with("0 unseen"));
    }
}
