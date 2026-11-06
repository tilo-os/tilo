//! Triggers: a package declares when one of its actions runs by itself, and the metadata says
//! what the platform runs and when.

use crate::common::{compile, compile_with};
use tilolang::meta::{TriggerArgMeta, TriggerWhenMeta};

const SOURCE: &str = r#"app t.trig "Trig"

type Task {
    title: Text
    at: Option<Time> = .none
    rings: Int merge counter
}

action ring(t: Task) { t.rings += 1 }

action remind(t: Task, at: Time) { t.at = .some(at) }

trigger often(t: Task) every 30m: ring(t)

trigger due(t: Task) at t.at: ring(t)

root Task

default view task for Task

view task(t: Task) {
    page { text("{t.rings}") }
}
"#;

fn first_message(body: &str) -> String {
    let src = format!("app t.x \"X\"\n{body}\n");
    compile_with(&src, &[])
        .err()
        .unwrap_or_default()
        .first()
        .map_or_else(
            || "<no diagnostic>".to_owned(),
            |d| format!("{}: {}", d.code, d.message),
        )
}

fn says(body: &str, code: &str, needle: &str) {
    let got = first_message(body);
    assert!(
        got.starts_with(code) && got.contains(needle),
        "expected {code} saying {needle:?} for:\n{body}\ngot:\n{got}"
    );
}

const TYPES: &str = r#"type Task {
    title: Text
    at: Option<Time> = .none
}

action ring(t: Task) { }
"#;

#[test]
fn a_trigger_says_what_it_runs_and_when() {
    let compiled = compile(SOURCE);
    let meta = &compiled.meta;
    assert_eq!(meta.triggers.len(), 2, "{:#?}", meta.triggers);

    let often = meta.triggers.iter().find(|t| t.name == "often").unwrap();
    let ring = meta.actions.iter().find(|a| a.name == "ring").unwrap();
    let task = meta.types.iter().find(|t| t.name == "Task").unwrap();
    assert_eq!(often.action, ring.id, "the action it runs");
    assert_eq!(often.ty, task.id, "the records it runs for");
    assert_eq!(often.args, vec![TriggerArgMeta::Subject]);
    assert_eq!(often.when, TriggerWhenMeta::Every(30 * 60));

    let due = meta.triggers.iter().find(|t| t.name == "due").unwrap();
    let at = task.fields.iter().find(|f| f.name == "at").unwrap();
    assert_eq!(due.when, TriggerWhenMeta::At(at.id.clone()));
    assert_eq!(due.args, vec![TriggerArgMeta::Subject]);
}

#[test]
fn a_trigger_runs_one_of_the_packages_actions_and_a_record_of_it() {
    says(
        "type Task { title: Text }\naction ring(t: Task) { }\ntrigger x(t: Task) every 30m: missing(t)\n",
        "E134",
        "declares no action `missing`",
    );
    says(
        "type Task { title: Text }\naction ring(t: Task) { }\ntrigger x(t: Text) every 30m: ring(t)\n",
        "E131",
        "records of a type of this package",
    );
}

#[test]
fn a_trigger_at_a_field_reads_a_time_the_record_holds() {
    says(
        // `title` is text, not a time.
        &format!("{TYPES}\ntrigger x(t: Task) at t.title: ring(t)\n"),
        "E133",
        "is not a time",
    );
    says(
        &format!("{TYPES}\ntrigger x(t: Task) at 30m: ring(t)\n"),
        "E133",
        "`at` reads the time",
    );
    says(
        "type Task { title: Text }\naction ring(t: Task) { }\ntrigger x(t: Task) every 0m: ring(t)\n",
        "E132",
        "above zero",
    );
    says(
        &format!("{TYPES}\ntrigger x(t: Task, u: Task) every 30m: ring(t)\n"),
        "E003",
        "runs for one record",
    );
}

#[test]
fn a_trigger_hands_its_action_only_the_record_or_a_root() {
    says(
        "type Task { at: Option<Time> = .none }\naction ring(t: Task, at: Time) { }\ntrigger x(t: Task) at t.at: ring(t, at: \"2026-10-27T10:00:00Z\")\n",
        "E135",
        "makes no other argument",
    );
    says(
        "type Task { at: Option<Time> = .none }\naction ring(t: Task, at: Time) { }\ntrigger x(t: Task) at t.at: ring(t)\n",
        "E135",
        "no argument and the action has no default",
    );
}
