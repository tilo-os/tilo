//! Agents' query words in a task's programs (agents.md, "Querying", "Reading and describing",
//! "Errors that teach"): `grep`, `find`, the chaining functions, `read`, `describe` and `print`,
//! over every app the task may read.
//!
//! Two workspaces' worth of apps: Tasks and a journal in one, Notes, Chat and Board with their
//! scenarios in the other. The task's programs run in a host of the task's own, which reads
//! each app's resources through its host, as the API answers them.

use std::fmt::Write as _;
use std::rc::Rc;

use serde_json::Value as Json;
use tilolang_runtime::Actor;
use tilolang_tilo::task::TaskSession;

use crate::task_api::Apps;

macro_rules! app {
    ($name:literal) => {
        (
            include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
            include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
        )
    };
}

/// A package of the test's own: entries whose body is written by an action, for the facts
/// grep reads of text (block kinds, writers, times).
const JOURNAL: &str = r#"app so.test.journal "Journal"

type Entry {
    title: Text
    body: Text
}

type Journal {
    items: Collection<Entry>
}

root entries: Journal

default view journal for Journal

view journal(all: Journal) {
    page {
        text("Journal")
    }
}

action write(all: Journal, title: Text, body: Text) -> Entry {
    all.items.insert(Entry(title: title, body: body))
}
"#;

const TODO: &str = "so.tilo.lang.tasks";

/// The task's world: Tasks and the journal in one host, Notes, Chat and Board in another, each
/// filled by its first scenario, and a task of them all whose programs run in a host of its own.
struct World {
    task: TaskSession,
}

fn world() -> World {
    let apps = Apps::new(&[
        &[app!("discussion"), app!("tasks"), (JOURNAL, "")],
        &[app!("notes"), app!("chat"), app!("board")],
    ]);
    {
        let todo = apps.replica(TODO);
        todo.host.set_clock(tilolang_tilo::host::system_time);
    }
    for app in ["so.tilo.notes", "so.tilo.lang.chat", "so.tilo.lang.board"] {
        let other = apps.replica(app);
        let package = other.host.package(app).unwrap();
        let scenario = package.meta.scenarios.first().unwrap().action.clone();
        other
            .host
            .runtime()
            .invoke_action(
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                (scenario.as_str(), vec![]),
            )
            .unwrap();
        other.commit();
    }
    let apps = Rc::new(apps);
    let task = apps.task();
    // The task reads times as they are: what an app wrote just now was changed within any recent window.
    task.set_clock(tilolang_tilo::host::system_time);
    World { task }
}

impl World {
    /// Run `source` as the task's program, confirmed; its answer.
    fn run(&self, source: &str) -> Json {
        self.task.run(
            source,
            true,
            &tilolang_tilo::runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::agent(
                "task-1",
            )),
        )
    }
}

/// What a program that ran to its end printed.
fn printed(answer: &Json) -> String {
    assert_eq!(answer["state"], "done", "{answer:#}");
    answer["output"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Why a program did not run.
fn refused(answer: &Json) -> String {
    assert_ne!(answer["state"], "done", "{answer:#}");
    answer["detail"].to_string()
}

#[test]
fn grep_counts_matches_per_resource_in_an_app_the_host_does_not_hold() {
    let w = world();
    let out = printed(&w.run(r"grep(/\blemons?\b/, in: notes).count_by(.doc)"));
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0]
            .starts_with(r"grep /\blemons?\b/: 2 matches in 2 of 8 resources searched (notes 8)"),
        "counts first: {out}"
    );
    assert!(
        lines[1].starts_with("@notes.Note:"),
        "a reference first: {out}"
    );
    assert!(out.contains("\"Groceries\"  1"), "{out}");
    assert!(out.contains("\"Lentil soup\"  1"), "{out}");
}

#[test]
fn grep_reads_several_apps_and_each_match_says_where_it_is() {
    let w = world();
    let out = printed(&w.run(r"grep(/(?i)importer/, in: notes + board)"));
    assert!(out.contains("(notes 8, board "), "{out}");
    assert!(
        out.lines().any(|l| l.starts_with("@notes.Note:")
            && l.contains("body ¶1: The importer handles tables now.")),
        "a match is its resource, its paragraph and the text: {out}"
    );
    assert!(out.lines().any(|l| l.starts_with("@board.")), "{out}");
}

#[test]
fn grep_knows_block_kinds_groups_times_and_writers() {
    let w = world();
    let body = r"> Tide was its name in the old plan\nWe still call the app Tide\n- [ ] record the launch video for Tide\n- [x] launch checklist";
    let wrote = w.run(&format!(
        "journal.write(journal.entries, \"Launch\", \"{body}\")"
    ));
    assert_eq!(wrote["state"], "done", "{wrote:#}");

    let all = printed(&w.run(r"grep(/\bTide\b/, in: journal).len()"));
    assert!(all.ends_with("\n3"), "{all}");
    let unquoted = printed(&w.run(r"grep(/\bTide\b/, in: journal, not: .quote).map(.anchor)"));
    assert!(
        unquoted.ends_with("\nbody ¶2\nbody ¶3.1"),
        "a list prints a line per item, cited by the field that holds it: {unquoted}"
    );

    let tasks = printed(
        &w.run(r"grep(/^- \[ \] (.*launch.*)/i, in: journal, changed: -14d).map(t -> t.group(1))"),
    );
    assert!(
        tasks.ends_with("\nrecord the launch video for Tide"),
        "{tasks}"
    );
    let stale = printed(&w.run(r"grep(/launch/, in: journal, changed: ..2026-01-01).len()"));
    assert!(stale.ends_with("\n0"), "{stale}");

    let writers = printed(&w.run(r"grep(/Tide/, in: journal).count_by(.written_by)"));
    assert!(
        writers.ends_with("task-1  3"),
        "the one who wrote it: {writers}"
    );

    let mentions = printed(&w.run(r"grep(/Tide/, in: journal, mentions: me).len()"));
    assert!(
        mentions.contains("plain text, which records no mentions"),
        "what it could not cover: {mentions}"
    );
}

#[test]
fn a_grep_result_feeds_actions_in_the_same_program() {
    let w = world();
    let answer = w.run(
        r"grep(/^Oat milk, (.*)/, in: notes).each(m -> tasks.new_task(tasks.tasks, title: m.group(1)))",
    );
    assert_eq!(answer["state"], "done", "{answer:#}");
    assert_eq!(answer["changed"], 1, "{answer:#}");
    let titles = printed(&w.run("tasks.tasks.items.map(.title)"));
    assert_eq!(titles, "eggs, two lemons.");
}

#[test]
fn find_takes_the_palettes_words_or_a_type_and_its_filters() {
    let w = world();
    let words = printed(&w.run(r#"find("lemon")"#));
    assert_eq!(words.lines().count(), 2, "{words}");
    assert!(
        words.lines().all(|l| l.starts_with("@notes.Note:")),
        "{words}"
    );

    // Typed, chained by key paths.
    let pinned = printed(&w.run("find(Note, pinned: true).sort_by(.title).map(.title)"));
    assert_eq!(pinned, "Lentil soup\nQ3 plan");
    let grouped =
        printed(&w.run(r"find(Note, pinned: false).group(.pinned).map(g -> g.items.len())"));
    assert_eq!(grouped, "6", "one group: none of them is pinned");
    let taken = printed(&w.run(r"find(Note, pinned: false).take(2).len()"));
    assert_eq!(taken, "2");
}

#[test]
fn single_is_one_result_or_an_error_listing_the_candidates() {
    let w = world();
    let one = printed(&w.run(r#"find(notes.Note).filter(n -> n.title == "Groceries").single()"#));
    assert!(one.contains("\"Groceries\""), "{one}");
    let several = refused(&w.run(r#"find("lemon").single()"#));
    assert!(
        several.contains("single: 2 match, not one; say which:"),
        "{several}"
    );
    assert!(several.contains("Lentil soup"), "the candidates: {several}");
}

#[test]
fn read_states_the_size_first_and_shows_what_matches_with_anchors() {
    let w = world();
    let body = (1..=50)
        .map(|n| format!("Line {n} of the plan"))
        .collect::<Vec<_>>()
        .join("\\n");
    w.run(&format!(
        "journal.write(journal.entries, \"Long plan\", \"{body}\")"
    ));
    let first = printed(&w.run(r"read(journal.entries.items.single())"));
    let mut lines = first.lines();
    let head = lines.next().unwrap();
    assert!(
        head.starts_with("@journal.Entry:")
            && head.contains("Entry: 51 paragraphs")
            && head.contains("page 1 of 2"),
        "{head}"
    );
    assert!(
        lines
            .clone()
            .any(|l| l.ends_with(" ¶1  Line 1 of the plan")),
        "{first}"
    );
    assert!(first.lines().last().unwrap().contains("page: 2"), "{first}");
    let matching = printed(
        &w.run(r"read(journal.entries.items.single(), matching: /Line 4[0-2]\b/, around: 1)"),
    );
    assert!(matching.contains("3 paragraphs match"), "{matching}");
    assert_eq!(matching.lines().count(), 1 + 5, "{matching}");

    // A resource of an app this host does not hold reads through its own host.
    let foreign = printed(&w.run(r"read(grep(/sync bug/, in: notes).single().doc)"));
    assert!(
        foreign.contains("\"Monday\"  Note: 2 paragraphs"),
        "{foreign}"
    );
    assert!(
        foreign.contains("body ¶1  Ada is on the sync bug."),
        "{foreign}"
    );
    assert!(foreign.contains(" .pinned  false"), "{foreign}");
}

#[test]
fn describe_gives_declarations_of_apps_types_actions_fields_and_resources() {
    let w = world();
    let app = printed(&w.run("describe(notes)"));
    assert!(app.starts_with("app notes \"Notes\""), "{app}");
    assert!(
        app.contains("type Note { title: Text, body: RichText"),
        "{app}"
    );
    assert!(
        app.contains("action notes.pin(n: Note, pinned: Bool = not n.pinned)"),
        "{app}"
    );
    let ty = printed(&w.run("describe(Task)"));
    assert!(ty.contains("type tasks.Task {"), "{ty}");
    assert!(
        ty.contains("    done: Bool\n"),
        "the declaration says the field is the one that is finished: {ty}"
    );
    assert!(
        ty.contains(
            "filters: inbox, today, upcoming, done, assigned_to_me, due, project, priority"
        ),
        "{ty}"
    );
    let action = printed(&w.run("describe(tasks.new_task)"));
    assert!(action.starts_with("/// "), "its doc first: {action}");
    assert!(
        action.contains("action tasks.new_task(tasks: Tasks, p: Option<Project> = .none"),
        "{action}"
    );
    let field = printed(&w.run("describe(Task.done)"));
    assert!(field.starts_with("tasks.Task.done: Bool"), "{field}");
    let root = printed(&w.run("describe(tasks.tasks)"));
    assert_eq!(root, "root tasks.tasks: Tasks");
    let resource = printed(&w.run("let list = tasks.tasks\ndescribe(list)"));
    assert!(resource.starts_with("@tasks.Tasks:"), "{resource}");
    assert!(resource.contains("  a Tasks\n"), "{resource}");
}

#[test]
fn print_shows_rows_with_the_fields_asked_or_the_types_filters() {
    let w = world();
    w.run("tasks.new_task(tasks.tasks, title: \"Milk\")");
    let asked = printed(&w.run("print(tasks.tasks.items, fields: [.title, .done])"));
    assert!(asked.starts_with("@tasks.Task:"), "{asked}");
    assert!(
        asked.ends_with("\"Milk\"  title: Milk  done: false"),
        "{asked}"
    );
    let filters = printed(&w.run("tasks.tasks.items"));
    assert!(filters.contains("done: false"), "{filters}");
}

#[test]
fn errors_teach_what_is_valid() {
    let w = world();
    // No word of a declaration spells `completed`: the error teaches the filters there are.
    let filter = refused(&w.run("find(Task, completed: true)"));
    assert!(
        filter.contains("has no filter `completed`")
            && filter.contains("its filters: ")
            && filter.contains("done"),
        "{filter}"
    );
    let source = refused(&w.run("grep(/x/, in: notez)"));
    assert!(
        source.contains("`notez` is no app this program may read; did you mean `notes`?"),
        "{source}"
    );
    let method = refused(&w.run("tasks.tasks.items.cout_by(.done)"));
    assert!(method.contains("did you mean `count_by`?"), "{method}");
    let kind = refused(&w.run("grep(/x/, not: .quotes)"));
    assert!(kind.contains("did you mean `quote`?"), "{kind}");
    // A `/` after a value still divides.
    let divided = printed(&w.run("10 / 2"));
    assert_eq!(divided, "5");
}

/// The agents.md examples in scope, one program after another in one session, as an agent
/// sends them: each one's answer, printed (`--no-capture` shows the transcript). Every entry
/// runs to its end or fails with its error; none keeps asking to read.
#[test]
fn a_session_of_the_agents_doc_examples() {
    let w = world();
    let entries = [
        r#"journal.write(journal.entries, "Launch", "> Tide was its name in the old plan\nWe still call the app Tide\n- [ ] record the launch video for Tide\n- [x] launch checklist")"#,
        r"grep(/\blemons?\b/, in: notes + chat).count_by(.doc)",
        r"grep(/\bTide\b/, in: journal, not: .quote)",
        r"grep(/(?i)release/).count_by(.written_by)",
        r"grep(/Tide/, in: journal, mentions: me).len()",
        r"grep(/^- \[ \] (.*launch.*)/i, in: journal, changed: -14d).each(t -> tasks.new_task(tasks.tasks, title: t.group(1)))",
        "print(tasks.tasks.items, fields: [.title, .done])",
        r#"find("lemon")"#,
        r#"find("lemon").take(2)"#,
        r#"find("lemon").single()"#,
        r#"find(notes.Note).filter(n -> n.title == "Groceries").single()"#,
        "find(Task, done: false).sort_by(.title).map(.title)",
        r"read(grep(/sync bug/, in: notes).single().doc, matching: /sync/)",
        "describe(notes)",
        "describe(Task)",
        "describe(tasks.new_task)",
        "describe(Task.done)",
        "let list = tasks.tasks\ndescribe(list)",
        "find(Task, completed: true)",
        "grep(/x/, in: docs)",
        "tasks.tasks.items.cout_by(.done)",
        "grep(/x/, not: .quotes)",
    ];
    let mut transcript = String::new();
    for entry in entries {
        let answer = w.run(entry);
        let _ = writeln!(transcript, "tl> {}", entry.replace('\n', "\n... "));
        for line in answer["output"].as_array().into_iter().flatten() {
            transcript.push_str(line.as_str().unwrap_or_default());
            transcript.push('\n');
        }
        match answer["state"].as_str() {
            Some("done") => {}
            Some("rejected") => {
                for line in answer["detail"].as_array().into_iter().flatten() {
                    let _ = writeln!(transcript, "error: {}", line.as_str().unwrap_or_default());
                }
            }
            _ => {
                let _ = writeln!(
                    transcript,
                    "error: {}",
                    answer["detail"].as_str().unwrap_or("the program failed")
                );
            }
        }
        let changed = answer["changed"].as_u64().unwrap_or(0);
        if changed > 0 {
            let _ = writeln!(transcript, "({changed} changed)");
        }
    }
    println!("{transcript}");
    assert!(
        transcript.contains("grep /\\blemons?\\b/: "),
        "{transcript}"
    );
    assert!(
        transcript.contains("single: 2 match, not one"),
        "{transcript}"
    );
}

/// What changed in the person's notes this week: the notes found by when each last changed,
/// read from each note's own history, and who changed it then. Nothing about a note's time is
/// one of its fields, so a program that asks a field for it, or the clock, is taught the filter.
#[test]
fn what_changed_in_the_notes_this_week_is_found_by_when_each_last_changed() {
    let apps = Rc::new(Apps::new(&[&[app!("discussion"), app!("notes")]]));
    let clock = Rc::new(std::cell::RefCell::new("2026-10-01T09:00:00Z".to_owned()));
    {
        let notes = apps.replica("so.tilo.notes");
        let now = Rc::clone(&clock);
        notes.host.set_clock(move || now.borrow().clone());
        let package = notes.host.package("so.tilo.notes").unwrap();
        let scenario = package.meta.scenarios.first().unwrap().action.clone();
        notes
            .host
            .runtime()
            .invoke_action(
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                (scenario.as_str(), vec![]),
            )
            .unwrap();
        notes.commit();
    }
    let task = apps.task();
    task.set_clock(tilolang_tilo::host::system_time);
    let run = |source: &str| {
        task.run(
            source,
            true,
            &tilolang_tilo::runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::agent(
                "task-1",
            )),
        )
    };
    // Groceries changes now; every other note was written in October.
    *clock.borrow_mut() = tilolang_tilo::host::system_time();
    let pinned = run(r#"notes.pin(find(notes.Note).filter(n -> n.title == "Groceries").single())"#);
    assert_eq!(pinned["state"], "done", "{pinned:#}");

    let week = printed(&run("find(notes.Note, changed: -7d)"));
    assert_eq!(week.lines().count(), 1, "{week}");
    assert!(week.contains("\"Groceries\""), "{week}");
    let all = printed(&run("find(notes.Note, changed: 2026-09-01..).len()"));
    assert_eq!(all, "8", "{all}");

    // Who changed it and when, and what it says now.
    let when = printed(&run(
        r#"updated(find(notes.Note).filter(n -> n.title == "Groceries").single())"#,
    ));
    assert!(
        !when.starts_with("2026-10-01") && when.contains("  changed by "),
        "{when}"
    );
    let body = printed(&run(
        r#"find(notes.Note).filter(n -> n.title == "Lentil soup").single().body"#,
    ));
    assert!(
        !body.is_empty() && body != "rich text",
        "a body prints as its text: {body}"
    );

    // A note's time asked of a field, or of the clock, is taught the filter.
    let field = refused(&run("find(notes.Note).filter(n -> n.edited)"));
    assert!(
        field.contains("1:32: E021 `Note` has no field `edited`")
            && field.contains("find(Note, changed: -7d)")
            && field.contains("updated(r)"),
        "{field}"
    );
    let clocked = refused(&run(
        "find(notes.Note).filter(n -> if let u = updated(n) { u.at > now - 7d } else { false })",
    ));
    assert!(
        clocked.contains("a program cannot") && clocked.contains("changed: -7d"),
        "{clocked}"
    );
    let typed = refused(&run(r#"grep(notes.Note, "week")"#));
    assert!(
        typed.contains("find them with `find(notes.Note)`")
            && typed.contains("`grep(/word/, in: notes)`"),
        "{typed}"
    );
    let ui = refused(&run(
        "text(find(notes.Note).filter(n -> n.title == \"Groceries\").single().body)",
    ));
    assert!(ui.contains("`read(r)`"), "{ui}");
    let described = printed(&run("describe(notes.Note)"));
    assert!(
        described.contains("changed: when it last changed  (find(Note, changed: -7d))"),
        "{described}"
    );
}

/// "Which of my tasks are due this week?": a program asks a date as search writes it, since a
/// program reads no clock of its own; what it reaches for first (`today`, `!`, `&&`) is taught.
#[test]
fn due_this_week_is_a_filter_and_the_words_a_program_reaches_for_first_are_taught() {
    let w = world();
    let week = w.run("find(tasks.Task, due: today..today + 7d, done: false).len()");
    assert_eq!(week["state"], "done", "{week:#}");
    let clock = refused(&w.run("print(today)"));
    assert!(
        clock.contains("a program cannot") && clock.contains("due: today"),
        "{clock}"
    );
    let negated = refused(&w.run("find(tasks.Task).filter(t -> !t.done && t.title != \"x\")"));
    assert!(
        negated.contains("tilolang writes `not x`")
            && negated.contains("tilolang writes `a and b`"),
        "{negated}"
    );
}
