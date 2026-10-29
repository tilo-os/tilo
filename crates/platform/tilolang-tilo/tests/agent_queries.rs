//! Agents' query words on the tilolang host (agents.md, "Querying", "Reading and describing",
//! "Errors that teach"): `grep`, `find`, the chaining functions, `read`, `describe` and `print`,
//! over the app the program runs in and, through another host, over apps it does not hold.
//!
//! Two hosts as the door sees them: one runs Todo, where the programs run, the other holds
//! Notes, Chat and Board with their scenarios. [`run`] plays the door: what a program asks to
//! read of the other apps is answered by the other host's `read-for-program`, and the program
//! runs again with the answers.

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilolang_runtime::Principal;
use tilolang_tilo::HostEffect;
use tilolang_tilo::publish::Caller;

use crate::fixtures::Replica;

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

const TODO: &str = "so.tilo.lang.todo";
const NOTES: &str = "so.tilo.lang.notes";
const CHAT: &str = "so.tilo.lang.chat";
const BOARD: &str = "so.tilo.lang.board";

fn agent() -> Caller {
    Caller {
        actor: "task-1".to_owned(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
    }
}

/// The host programs run in: Todo, with a list.
fn worker() -> Replica {
    let (mut todo, _) = Replica::new("ada", AutoCommit::new(), &[app!("todo"), (JOURNAL, "")]);
    todo.host.set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    todo.host.root_of(TODO).unwrap();
    todo.host.root_of("so.test.journal").unwrap();
    todo.commit();
    todo
}

/// The host of the other apps, each filled by its first scenario.
fn holder() -> Replica {
    let packages = [app!("notes"), app!("chat"), app!("board")];
    let (mut other, loaded) = Replica::new("ada", AutoCommit::new(), &packages);
    for package in &loaded {
        other.host.root_of(&package.meta.app).unwrap();
        other.commit();
        let scenario = package.meta.scenarios.first().unwrap().action.clone();
        other
            .host
            .runtime()
            .invoke_action(&Principal::new("ada"), (scenario.as_str(), vec![]))
            .unwrap();
        other.commit();
    }
    other
}

/// Run `source` in `at` as the agent's program, reading the other apps of `other` as the door
/// does; its answer.
fn run(at: &mut Replica, other: &Replica, source: &str) -> Json {
    let apps = json!({"todo": TODO, "notes": NOTES, "chat": CHAT, "board": BOARD}).to_string();
    let mut reads: Vec<Json> = Vec::new();
    for _round in 0..8 {
        let answer = at
            .host
            .answer(
                HostEffect::RunProgram {
                    source: source.to_owned(),
                    session: None,
                    confirm: Some(true),
                    apps: Some(apps.clone()),
                    reads: Some(Json::Array(reads.clone()).to_string()),
                },
                Some(&agent()),
            )
            .unwrap()
            .value
            .unwrap();
        at.commit();
        if answer["state"] != "reads" {
            return answer;
        }
        for request in answer["requests"].as_array().unwrap() {
            let read = other
                .host
                .answer(
                    HostEffect::ReadForProgram {
                        request: request.to_string(),
                    },
                    Some(&agent()),
                )
                .map_or_else(|why| json!({"unavailable": why}), |out| out.value.unwrap());
            reads.push(json!([request, read]));
        }
    }
    panic!("the program kept asking to read");
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
    let other = holder();
    let mut todo = worker();
    let out = printed(&run(
        &mut todo,
        &other,
        r"grep(/\bChen\b/, in: notes).count_by(.doc)",
    ));
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
    let other = holder();
    let mut todo = worker();
    let out = printed(&run(
        &mut todo,
        &other,
        r"grep(/(?i)release/, in: notes + board)",
    ));
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
    let other = holder();
    let mut todo = worker();
    let body = r#"> Tide was its name in the old plan\nWe still call the app Tide\n- [ ] record the launch video for Tide\n- [x] launch checklist"#;
    let wrote = run(
        &mut todo,
        &other,
        &format!("journal.write(journal.entries, \"Launch\", \"{body}\")"),
    );
    assert_eq!(wrote["state"], "done", "{wrote:#}");

    let all = printed(&run(
        &mut todo,
        &other,
        r"grep(/\bTide\b/, in: journal).len()",
    ));
    assert!(all.ends_with("\n3"), "{all}");
    let unquoted = printed(&run(
        &mut todo,
        &other,
        r"grep(/\bTide\b/, in: journal, not: .quote).map(.anchor)",
    ));
    assert!(
        unquoted.ends_with("\n¶2\n¶3.1"),
        "a list prints a line per item: {unquoted}"
    );

    let tasks = printed(&run(
        &mut todo,
        &other,
        r"grep(/^- \[ \] (.*launch.*)/i, in: journal, changed: -14d).map(t -> t.group(1))",
    ));
    assert!(
        tasks.ends_with("\nrecord the launch video for Tide"),
        "{tasks}"
    );
    let stale = printed(&run(
        &mut todo,
        &other,
        r"grep(/launch/, in: journal, changed: ..2026-01-01).len()",
    ));
    assert!(stale.ends_with("\n0"), "{stale}");

    let writers = printed(&run(
        &mut todo,
        &other,
        r"grep(/Tide/, in: journal).count_by(.written_by)",
    ));
    assert!(
        writers.ends_with("task-1  3"),
        "the one who wrote it: {writers}"
    );

    let mentions = printed(&run(
        &mut todo,
        &other,
        r"grep(/Tide/, in: journal, mentions: me).len()",
    ));
    assert!(
        mentions.contains("plain text, which records no mentions"),
        "what it could not cover: {mentions}"
    );
}

#[test]
fn a_grep_result_feeds_actions_in_the_same_program() {
    let other = holder();
    let mut todo = worker();
    let answer = run(
        &mut todo,
        &other,
        r"grep(/^Two blockers (.*)/, in: notes).each(m -> todo.add(todo.todos, m.group(1)))",
    );
    assert_eq!(answer["state"], "done", "{answer:#}");
    assert_eq!(answer["changed"], 1, "{answer:#}");
    let titles = printed(&run(&mut todo, &other, "todo.todos.items.map(.title)"));
    assert_eq!(titles, "left. Chen is out on Friday.");
}

#[test]
fn find_takes_the_palettes_words_or_a_type_and_its_filters() {
    let other = holder();
    let mut todo = worker();
    let words = printed(&run(&mut todo, &other, r#"find("standup chen")"#));
    assert_eq!(words.lines().count(), 2, "{words}");
    assert!(
        words.lines().all(|l| l.starts_with("@notes.Note:")),
        "{words}"
    );

    // In the host that holds Notes, typed, chained by key paths.
    let mut notes = holder();
    let pinned = printed(&run(
        &mut notes,
        &other,
        "find(Note, pinned: true).sort_by(.title).map(.title)",
    ));
    assert_eq!(pinned, "Groceries\nQ3 plan\nRunning plan");
    let grouped = printed(&run(
        &mut notes,
        &other,
        r#"find("standup").of(Note).group(.pinned).map(g -> g.items.len())"#,
    ));
    assert_eq!(grouped, "5", "one group: no standup is pinned");
    let taken = printed(&run(
        &mut notes,
        &other,
        r#"find(Note, pinned: false).take(2).len()"#,
    ));
    assert_eq!(taken, "2");
}

#[test]
fn single_is_one_result_or_an_error_listing_the_candidates() {
    let other = holder();
    let mut todo = worker();
    let one = printed(&run(&mut todo, &other, r#"find("groceries").single()"#));
    assert!(one.contains("\"Groceries\""), "{one}");
    let several = refused(&run(&mut todo, &other, r#"find("standup").single()"#));
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
    let other = holder();
    let mut todo = worker();
    let body = (1..=50)
        .map(|n| format!("Line {n} of the plan"))
        .collect::<Vec<_>>()
        .join("\\n");
    run(
        &mut todo,
        &other,
        &format!("journal.write(journal.entries, \"Long plan\", \"{body}\")"),
    );
    let first = printed(&run(
        &mut todo,
        &other,
        r#"read(journal.entries.items.single())"#,
    ));
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
    let matching = printed(&run(
        &mut todo,
        &other,
        r"read(journal.entries.items.single(), matching: /Line 4[0-2]\b/, around: 1)",
    ));
    assert!(matching.contains("3 paragraphs match"), "{matching}");
    assert_eq!(matching.lines().count(), 1 + 5, "{matching}");

    // A resource of an app this host does not hold reads through its own host.
    let foreign = printed(&run(
        &mut todo,
        &other,
        r"read(grep(/Chen is out/, in: notes).single().doc)",
    ));
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
    let other = holder();
    let mut todo = worker();
    let app = printed(&run(&mut todo, &other, "describe(notes)"));
    assert!(app.starts_with("app notes \"Notes\""), "{app}");
    assert!(app.contains("type Note { title: Text, body: Text"), "{app}");
    assert!(
        app.contains("action notes.pin(n: Note, pinned: Bool = not n.pinned)"),
        "{app}"
    );
    let ty = printed(&run(&mut todo, &other, "describe(Todo)"));
    assert!(ty.contains("type todo.Todo {"), "{ty}");
    assert!(ty.contains("    done: Bool\n"), "{ty}");
    assert!(ty.contains("filters: open, done, due"), "{ty}");
    let action = printed(&run(&mut todo, &other, "describe(todo.add)"));
    assert!(action.starts_with("/// "), "its doc first: {action}");
    assert!(
        action.contains("action todo.add(list: TodoList, title: Text) -> Todo"),
        "{action}"
    );
    let field = printed(&run(&mut todo, &other, "describe(Todo.done)"));
    assert!(field.starts_with("todo.Todo.done: Bool"), "{field}");
    let root = printed(&run(&mut todo, &other, "describe(todo.todos)"));
    assert_eq!(root, "root todo.todos: TodoList");
    let resource = printed(&run(
        &mut todo,
        &other,
        "let list = todo.todos\ndescribe(list)",
    ));
    assert!(resource.starts_with("@todo.TodoList:"), "{resource}");
    assert!(resource.contains("  a TodoList\n"), "{resource}");
}

#[test]
fn print_shows_rows_with_the_fields_asked_or_the_types_filters() {
    let other = holder();
    let mut todo = worker();
    run(&mut todo, &other, "todo.add(todo.todos, \"Milk\")");
    let asked = printed(&run(
        &mut todo,
        &other,
        "print(todo.todos.items, fields: [.title, .done])",
    ));
    assert!(asked.starts_with("@todo.Todo:"), "{asked}");
    assert!(
        asked.ends_with("\"Milk\"  title: Milk  done: false"),
        "{asked}"
    );
    let filters = printed(&run(&mut todo, &other, "todo.todos.items"));
    assert!(filters.contains("done: false"), "{filters}");
}

#[test]
fn errors_teach_what_is_valid() {
    let other = holder();
    let mut todo = worker();
    let filter = refused(&run(&mut todo, &other, "find(Todo, completed: true)"));
    assert!(
        filter.contains("has no filter `completed`; did you mean `done`?"),
        "{filter}"
    );
    let source = refused(&run(&mut todo, &other, "grep(/x/, in: notez)"));
    assert!(
        source.contains("`notez` is no app this program may read; did you mean `notes`?"),
        "{source}"
    );
    let method = refused(&run(&mut todo, &other, "todo.todos.items.cout_by(.done)"));
    assert!(method.contains("did you mean `count_by`?"), "{method}");
    let kind = refused(&run(&mut todo, &other, "grep(/x/, not: .quotes)"));
    assert!(kind.contains("did you mean `quote`?"), "{kind}");
    // A `/` after a value still divides.
    let divided = printed(&run(&mut todo, &other, "10 / 2"));
    assert_eq!(divided, "5");
}
