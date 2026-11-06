//! Field roles: what a field is to the platform, and the fields a role names. A repeat says which
//! day it counts from, a reminder which day, time of day and person it reads, and the checker
//! holds every repeat and reminder to saying so.

use crate::common::{codes, compile_body};
use tilolang::{parse, print_module};

const ROLES: &str = "type Task {
    title: Text (title)
    notes: RichText (body)
    old: Text (previously)
    due: Option<Date> = .none
    at: Option<Time> = .none
    assignee: Option<Principal> = .none
    repeats: Option<std.Repeat> (repeats: due) = .none
    reminds: Option<std.Reminder> (reminds: due, at, who: assignee) = .none
}
";

fn says(src: &str, needle: &str) {
    let full = format!("app t.x \"X\"\n{src}\n");
    let diags = tilolang::compile_package_with(
        &full,
        &tilolang::IdsLock::new(),
        &[],
        &mut tilolang::SeededEntropy(7),
    )
    .err()
    .unwrap_or_default();
    let got = diags.first().map_or_else(
        || "<no diagnostic>".to_owned(),
        |d| format!("{}: {}", d.code, d.message),
    );
    assert!(
        got.starts_with("E092") && got.contains(needle),
        "expected E092 saying {needle:?} for:\n{src}\ngot:\n{got}"
    );
}

#[test]
fn a_repeat_and_a_reminder_name_the_fields_they_read() {
    assert_eq!(codes(ROLES), Vec::<String>::new());
    let meta = compile_body(ROLES).meta;
    let task = meta
        .types
        .iter()
        .find(|ty| ty.name == "Task")
        .expect("Task");
    let role = |name: &str| {
        task.fields
            .iter()
            .find(|field| field.name == name)
            .and_then(|field| field.role.as_ref().map(tilolang::types::FieldRole::text))
    };
    assert_eq!(role("title").as_deref(), Some("title"));
    assert_eq!(role("notes").as_deref(), Some("body"));
    assert_eq!(role("old").as_deref(), Some("previously"));
    assert_eq!(role("repeats").as_deref(), Some("repeats: due"));
    assert_eq!(
        role("reminds").as_deref(),
        Some("reminds: due, at, who: assignee"),
        "the fields a reminder reads, as it names them"
    );
    assert_eq!(role("due"), None);
}

#[test]
fn a_role_may_name_a_field_declared_after_it() {
    assert_eq!(
        codes(
            "type T {\n    r: std.Repeat (repeats: due)\n    due: Date\n}\ntype U {\n    m: std.Reminder (reminds: due)\n    due: Option<Date> = .none\n}"
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_repeat_or_a_reminder_says_which_day_it_reads() {
    says(
        "type T {\n    due: Date\n    r: std.Repeat\n}",
        "says which day it reads",
    );
    says(
        "type T {\n    due: Date\n    r: Option<std.Reminder>\n}",
        "says which day it reads",
    );
}

#[test]
fn a_role_names_a_field_of_the_type() {
    says(
        "type T {\n    r: std.Repeat (repeats: when)\n}",
        "has no field `when`",
    );
    says(
        "type T {\n    at: Time\n    r: std.Repeat (repeats: at)\n}",
        "reads a Date",
    );
    says(
        "type T {\n    due: Date\n    r: std.Reminder (reminds: due, nope)\n}",
        "has no field `nope`",
    );
    says(
        "type T {\n    due: Date\n    at: Option<Text>\n    r: std.Reminder (reminds: due, at)\n}",
        "reads a Time",
    );
    says(
        "type T {\n    due: Date\n    who: Text\n    r: std.Reminder (reminds: due, who: who)\n}",
        "reads a Principal",
    );
}

#[test]
fn a_repeat_counts_from_one_day_and_a_reminder_reads_one_time_of_day() {
    says(
        "type T {\n    due: Date\n    at: Time\n    r: std.Repeat (repeats: due, at)\n}",
        "reads one day",
    );
    says(
        "type T {\n    due: Date\n    at: Time\n    later: Time\n    r: std.Reminder (reminds: due, at, later)\n}",
        "reads one day",
    );
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due, day: due)\n}",
        "not one of its names",
    );
}

#[test]
fn a_type_holds_at_most_one_repeat_and_one_reminder() {
    says(
        "type T {\n    due: Date\n    a: std.Repeat (repeats: due)\n    b: std.Repeat (repeats: due)\n}",
        "at most one",
    );
    says(
        "type T {\n    due: Date\n    a: std.Reminder (reminds: due)\n    b: std.Reminder (reminds: due)\n}",
        "at most one",
    );
}

#[test]
fn only_a_repeat_holds_a_repeat_role_and_only_the_type_holding_the_value_names_the_day() {
    says(
        "type T {\n    due: Date\n    r: Text (repeats: due)\n}",
        "only a `std.Repeat` field repeats",
    );
    says(
        "type T {\n    due: Date\n    r: std.Reminder (repeats: due)\n}",
        "`(repeats: …)` is not its marker",
    );
    says(
        "type T {\n    due: Date\n    when: Text (title: due)\n}",
        "`(title)` names no field",
    );
    says(
        "type T {\n    when: Option<Date> (previously: due)\n}",
        "`(previously)` names no field",
    );
    says(
        "type T {\n    when: Date (banana: due)\n}",
        "is not a field marker",
    );
}

#[test]
fn a_role_travels_in_the_metadata_as_the_fields_it_names() {
    use tilolang::types::FieldRole;
    let meta = compile_body(ROLES).meta;
    let task = meta
        .types
        .iter()
        .find(|ty| ty.name == "Task")
        .expect("Task");
    let role = |name: &str| {
        task.fields
            .iter()
            .find(|field| field.name == name)
            .and_then(|field| field.role.clone())
    };
    assert_eq!(
        serde_json::to_value(role("reminds")).unwrap(),
        serde_json::json!({"reminds": {"date": "due", "time": "at", "who": "assignee"}}),
        "a reminder names the fields it reads"
    );
    assert_eq!(
        serde_json::to_value(role("repeats")).unwrap(),
        serde_json::json!({"repeats": {"date": "due"}}),
        "a repeat names the day it counts from"
    );
    assert_eq!(
        serde_json::to_value(FieldRole::Title).unwrap(),
        serde_json::json!("title"),
        "the roles records were published with before keep their spelling"
    );
    assert_eq!(
        serde_json::from_value::<FieldRole>(serde_json::json!("body")).unwrap(),
        FieldRole::Body
    );
}

#[test]
fn a_marker_is_printed_as_it_is_written() {
    let src = format!("app t.x \"X\"\n{ROLES}");
    let once = print_module(&parse(&src).expect("parses"));
    assert!(once.contains("(repeats: due)"), "{once}");
    assert!(once.contains("(reminds: due, at, who: assignee)"), "{once}");
    let twice = print_module(&parse(&once).expect("printed parses"));
    assert_eq!(once, twice);
}
