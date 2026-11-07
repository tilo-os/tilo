//! A view places the actions of what it shows: `actions:` on an item's node and on the window's
//! shell names, in order, the ones it puts there, with the run of commands and the primary one it
//! gives them. Everything else the type offers stays reachable — the palette, an agent and a
//! program still offer every action — but the view's surfaces show what the view placed.

use crate::common::{codes, compile_with};

fn first_message(body: &str) -> String {
    let src = format!("app t.place \"Place\"\n{body}\n");
    compile_with(&src, &[])
        .err()
        .unwrap_or_default()
        .first()
        .map_or_else(
            || "<no diagnostic>".to_owned(),
            |d| format!("{}: {}", d.code, d.message),
        )
}

const DECLARATIONS: &str = r#"type Task {
    title: Text
    done: Bool = false
}

type Board {
    tasks: Collection<Task>
}

root board: Board

default view board for Board

action check(t: Task, done: Bool = not t.done) { t.done = done }

action schedule(t: Task, at: Time) { }

action remove(t: Task) { board.tasks.remove(t) }

action new_task() -> Task { return board.tasks.insert(Task(title: "")) }
"#;

/// The `row` a view of a board draws its tasks with, its rows placing `actions`.
fn board(actions: &str) -> String {
    format!(
        "{DECLARATIONS}\nview board(b: Board) {{\n    std.list(b.tasks) {{ (t, at) ->\n        \
         ui.ListItem(item: t, presence: at, actions: {actions}) {{ }}\n    }}\n}}\n"
    )
}

fn says(body: &str, code: &str, needle: &str) {
    let got = first_message(body);
    assert!(
        got.starts_with(code) && got.contains(needle),
        "expected {code} saying {needle:?}, got:\n{got}"
    );
}

#[test]
fn a_view_places_the_actions_it_names() {
    let src = board(r#"[check, schedule (group: "Plan"), remove (primary: true)]"#);
    let got = codes(&src);
    assert!(
        got.is_empty(),
        "expected no errors, got {got:?}: {}",
        first_message(&src)
    );
}

#[test]
fn a_name_that_is_not_an_action_is_reported_with_the_nearest_one() {
    says(&board("[chek]"), "E137", "did you mean `check`?");
}

#[test]
fn an_action_that_does_not_apply_to_the_items_type_is_reported() {
    says(
        &board("[new_task]"),
        "E138",
        "`new_task` does not apply to a Task",
    );
}

#[test]
fn a_placement_is_a_list_of_names() {
    says(&board("check"), "E137", "`actions: [name, ...]`");
}

#[test]
fn an_entry_takes_only_group_and_primary() {
    says(
        &board(r#"[check (icon: "x")]"#),
        "E137",
        "takes `group:` and `primary:`",
    );
}
