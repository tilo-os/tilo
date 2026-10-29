//! Agents' query words in a task's programs (agents.md, "Querying", "Reading and describing",
//! "Errors that teach"): `grep`, `find`, the chaining functions, `read`, `describe` and `print`,
//! over every app the task may read.
//!
//! Two app hosts as the door reaches them: one runs Tasks and a journal, the other Notes, Chat
//! and Board with their scenarios. The task's programs run in a host of the task's own, which
//! holds no records: what a program asks of an app is answered by that app's host
//! (`read-for-program`) while it runs.

use std::fmt::Write as _;
use std::rc::Rc;

use serde_json::Value as Json;
use tilolang_runtime::Principal;
use tilolang_tilo::task::TaskSession;

use crate::task_door::Apps;

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
    title: Text (title)
    body: Text (body)
}

type Entries = Collection<Entry>

root entries: Entries

default view journal for entries

view journal(all: Entries) {
    page {
        text("Journal")
    }
}

action write(all: Entries, title: Text, body: Text) -> Entry {
    all.insert(Entry(title: title, body: body))
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
        &[app!("tasks"), (JOURNAL, "")],
        &[app!("notes"), app!("chat"), app!("board")],
    ]);
    {
        let todo = apps.replica(TODO);
        todo.host.set_clock(tilolang_tilo::host::system_time);
    }
    for app in [
        "so.tilo.lang.notes",
        "so.tilo.lang.chat",
        "so.tilo.lang.board",
    ] {
        let mut other = apps.replica(app);
        let package = other.host.package(app).unwrap();
        let scenario = package.meta.scenarios.first().unwrap().action.clone();
        other
            .host
            .runtime()
            .invoke_action(&Principal::new("ada"), (scenario.as_str(), vec![]))
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
        self.task.run(source, true)
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
    let out = printed(&w.run(r"grep(/\bChen\b/, in: notes).count_by(.doc)"));
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0]
            .starts_with(r"grep /\bChen\b/: 2 matches in 2 of 14 resources searched (notes 14)"),
        "counts first: {out}"
    );
    assert!(
        lines[1].starts_with("@notes.Note:"),
        "a reference first: {out}"
    );
    assert!(out.contains("\"Standup, Monday\"  1"), "{out}");
    assert!(out.contains("\"Standup, Thursday\"  1"), "{out}");
}

#[test]
fn grep_reads_several_apps_and_each_match_says_where_it_is() {
    let w = world();
    let out = printed(&w.run(r"grep(/(?i)release/, in: notes + board)"));
    assert!(out.contains("(notes 14, board "), "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("@notes.Note:") && l.contains(" ¶1: Release candidate cut.")),
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
        unquoted.ends_with("\n¶2\n¶3.1"),
        "a list prints a line per item: {unquoted}"
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
        r"grep(/^Two blockers (.*)/, in: notes).each(m -> tasks.add(tasks.tasks, m.group(1)))",
    );
    assert_eq!(answer["state"], "done", "{answer:#}");
    assert_eq!(answer["changed"], 1, "{answer:#}");
    let titles = printed(&w.run("tasks.tasks.items.map(.title)"));
    assert_eq!(titles, "left. Chen is out on Friday.");
}

#[test]
fn find_takes_the_palettes_words_or_a_type_and_its_filters() {
    let w = world();
    let words = printed(&w.run(r#"find("standup chen")"#));
    assert_eq!(words.lines().count(), 2, "{words}");
    assert!(
        words.lines().all(|l| l.starts_with("@notes.Note:")),
        "{words}"
    );

    // Typed, chained by key paths.
    let pinned = printed(&w.run("find(Note, pinned: true).sort_by(.title).map(.title)"));
    assert_eq!(pinned, "Groceries\nQ3 plan\nRunning plan");
    let grouped =
        printed(&w.run(r#"find("standup").of(Note).group(.pinned).map(g -> g.items.len())"#));
    assert_eq!(grouped, "5", "one group: no standup is pinned");
    let taken = printed(&w.run(r"find(Note, pinned: false).take(2).len()"));
    assert_eq!(taken, "2");
}

#[test]
fn single_is_one_result_or_an_error_listing_the_candidates() {
    let w = world();
    let one = printed(&w.run(r#"find("groceries").single()"#));
    assert!(one.contains("\"Groceries\""), "{one}");
    let several = refused(&w.run(r#"find("standup").single()"#));
    assert!(
        several.contains("single: 5 match, not one; say which:"),
        "{several}"
    );
    assert!(
        several.contains("Standup, Wednesday"),
        "the candidates: {several}"
    );
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
    let foreign = printed(&w.run(r"read(grep(/Chen is out/, in: notes).single().doc)"));
    assert!(
        foreign.contains("\"Standup, Thursday\"  Note: 2 paragraphs"),
        "{foreign}"
    );
    assert!(
        foreign.contains(" ¶1  Two blockers left. Chen is out on Friday."),
        "{foreign}"
    );
    assert!(foreign.contains(" .pinned  false"), "{foreign}");
}

#[test]
fn describe_gives_declarations_of_apps_types_actions_fields_and_resources() {
    let w = world();
    let app = printed(&w.run("describe(notes)"));
    assert!(app.starts_with("app notes \"Notes\""), "{app}");
    assert!(app.contains("type Note { title: Text, body: Text"), "{app}");
    assert!(
        app.contains("action notes.pin(n: Note, pinned: Bool = not n.pinned)"),
        "{app}"
    );
    let ty = printed(&w.run("describe(Task)"));
    assert!(ty.contains("type tasks.Task {"), "{ty}");
    assert!(ty.contains("    done: Bool\n"), "{ty}");
    assert!(
        ty.contains("filters: inbox, today, upcoming, done, due, project, priority"),
        "{ty}"
    );
    let action = printed(&w.run("describe(tasks.add)"));
    assert!(action.starts_with("/// "), "its doc first: {action}");
    assert!(
        action.contains("action tasks.add(tasks: Tasks, title: Text) -> Task"),
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
    w.run("tasks.add(tasks.tasks, \"Milk\")");
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
    let filter = refused(&w.run("find(Task, completed: true)"));
    assert!(
        filter.contains("has no filter `completed`; did you mean `done`?"),
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
        r"grep(/\bChen\b/, in: notes + chat).count_by(.doc)",
        r"grep(/\bTide\b/, in: journal, not: .quote)",
        r"grep(/(?i)release/).count_by(.written_by)",
        r"grep(/Tide/, in: journal, mentions: me).len()",
        r"grep(/^- \[ \] (.*launch.*)/i, in: journal, changed: -14d).each(t -> tasks.add(tasks.todos, t.group(1)))",
        "print(tasks.todos.items, fields: [.title, .done])",
        r#"find("standup chen")"#,
        r#"find("standup").take(2)"#,
        r#"find("standup").single()"#,
        r#"find("groceries").single()"#,
        "find(Todo, done: false).sort_by(.title).map(.title)",
        r"read(grep(/Chen is out/, in: notes).single().doc, matching: /Chen/)",
        "describe(notes)",
        "describe(Todo)",
        "describe(tasks.add)",
        "describe(Todo.done)",
        "let list = tasks.todos\ndescribe(list)",
        "find(Todo, completed: true)",
        "grep(/x/, in: docs)",
        "tasks.todos.items.cout_by(.done)",
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
    assert!(transcript.contains("grep /\\bChen\\b/: "), "{transcript}");
    assert!(
        transcript.contains("single: 5 match, not one"),
        "{transcript}"
    );
}
