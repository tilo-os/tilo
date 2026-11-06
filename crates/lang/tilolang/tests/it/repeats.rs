//! Repeats and reminders: both are held as their RFC 5545 text, written as a string literal, and
//! read back as the day they come round again and the moment they remind.

use crate::common::{codes, compile_body, compile_with};
use tilolang::Diagnostic;

fn messages(src: &str) -> Vec<Diagnostic> {
    let full = format!("app t.x \"X\"\n{src}\n");
    compile_with(&full, &[]).err().unwrap_or_default()
}

fn first_message(src: &str) -> String {
    messages(src).first().map_or_else(
        || "<no diagnostic>".to_owned(),
        |d| format!("{}: {}", d.code, d.message),
    )
}

fn says(src: &str, code: &str, needle: &str) {
    let got = first_message(src);
    assert!(
        got.starts_with(code) && got.contains(needle),
        "expected {code} saying {needle:?} for:\n{src}\ngot:\n{got}"
    );
}

#[test]
fn a_repeat_is_held_as_its_rule_from_a_literal_checked_when_compiled() {
    let meta = compile_body(
        "type Task {\n    due: Date\n    repeats: std.Repeat (repeats: due) = std.Repeat(\"rrule:freq=weekly;interval=2\")\n    reminders: Set<std.Reminder>\n}\nfn label(t: Task) -> Text {\n    return t.repeats.label()\n}\nfn next(t: Task, from: Date, done: Date) -> Date {\n    return t.repeats.next(from, done)\n}\nfn next_at(t: Task, at: Time, day: Date) -> Time {\n    return t.repeats.next_at(at, day)\n}",
    )
    .meta;
    let task = meta.types.iter().find(|t| t.name == "Task").expect("Task");
    let repeats = task
        .fields
        .iter()
        .find(|f| f.name == "repeats")
        .expect("repeats");
    assert_eq!(repeats.ty, "std.Repeat");
    assert_eq!(
        repeats.default,
        Some(serde_json::json!("FREQ=WEEKLY;INTERVAL=2"))
    );
    let reminders = task
        .fields
        .iter()
        .find(|f| f.name == "reminders")
        .expect("reminders");
    assert_eq!(reminders.ty, "Set<std.Reminder>");
}

#[test]
fn a_reminder_is_held_as_its_trigger_and_is_read_back_at_a_due_day() {
    let meta = compile_body(
        "type Task {\n    due: Date\n    reminds: std.Reminder (reminds: due) = std.Reminder(\"P1DT2H\")\n}\nfn at(t: Task, due: Date, at: Option<Time>) -> Time {\n    return t.reminds.at(due, at)\n}\nfn label(t: Task) -> Text {\n    return t.reminds.label()\n}",
    )
    .meta;
    let reminds = meta.types[0]
        .fields
        .iter()
        .find(|f| f.name == "reminds")
        .expect("reminds");
    assert_eq!(reminds.ty, "std.Reminder");
    assert_eq!(reminds.default, Some(serde_json::json!("P1DT2H")));
}

#[test]
fn a_repeat_or_a_reminder_refuses_text_it_cannot_hold() {
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due) = std.Repeat(\"FREQ=HOURLY\")\n}",
        "E099",
        "FREQ",
    );
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due) = std.Repeat(\"every day\")\n}",
        "E099",
        "FREQ",
    );
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due) = std.Repeat(\"FREQ=DAILY;BYMONTH=1\")\n}",
        "E099",
        "BYMONTH",
    );
    says(
        "type T {\n    due: Date\n    m: std.Reminder (reminds: due) = std.Reminder(\"15m\")\n}",
        "E099",
        "starts with `P`",
    );
    says(
        "type T {\n    due: Date\n    m: std.Reminder (reminds: due) = std.Reminder(\"PT15M before\")\n}",
        "E099",
        "more after it",
    );
}

#[test]
fn a_repeat_or_a_reminder_is_made_from_a_literal_and_never_from_computed_text() {
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due) = std.Repeat(\"FREQ=DAILY\")\n}\naction a(t: T, s: Text) {\n    t.r = std.Repeat(s)\n}",
        "E099",
        "one string literal",
    );
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n}\naction a(t: T) {\n    t.r = std.Repeat(text(\"FREQ={t.r}\"))\n}",
        "E099",
        "one string literal",
    );
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n}\naction a(t: T) {\n    t.r = std.Repeat(\"FREQ=DAILY\") ?? std.Repeat(\"FREQ=WEEKLY\")\n}",
        "E106",
        "std.Repeat",
    );
}

#[test]
fn a_repeat_or_a_reminder_is_a_field_type_of_the_standard_library_and_nothing_else() {
    assert_eq!(
        codes("type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n}"),
        Vec::<String>::new()
    );
    says(
        "type T {\n    r: std.Address\n}",
        "E013",
        "no type `Address`",
    );
    says(
        "type T {\n    due: Date\n    r: Repeat (repeats: due)\n}",
        "E013",
        "std.Repeat",
    );
}

#[test]
fn a_repeat_reads_in_a_text_only_through_its_label() {
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n}\nview v(t: T) {\n    text(\"{t.r}\")\n}",
        "E020",
        "its label",
    );
}

#[test]
fn a_repeat_has_next_next_at_and_label_and_a_reminder_at_and_label() {
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n}\nfn f(t: T, d: Date) -> Date {\n    return t.r.at(d, .none)\n}",
        "E027",
        "its methods: next, next_at, label",
    );
    says(
        "type T {\n    due: Date\n    m: std.Reminder (reminds: due)\n}\nfn f(t: T, d: Date) -> Date {\n    return t.m.next(d, d)\n}",
        "E027",
        "its methods: at, label",
    );
    says(
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n}\nfn f(t: T) -> Date {\n    return t.r.next(\"today\", \"tomorrow\")\n}",
        "E020",
        "expected Date, found Text",
    );
}

#[test]
fn two_repeats_compare_by_their_stored_text() {
    assert_eq!(
        codes(
            "type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n}\nfn f(t: T) -> Bool {\n    return t.r == std.Repeat(\"FREQ=DAILY\")\n}"
        ),
        Vec::<String>::new()
    );
}

#[test]
fn what_a_repeat_compiles_to_is_valid_wasm() {
    use crate::common::validate;
    let cases = [
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n}\naction a(t: T, from: Date, done: Date) {\n    let d = t.r.next(from, done)\n}",
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n}\naction a(t: T, at: Time, day: Date) uses time {\n    let d = t.r.next_at(at, day)\n}",
        "type T {\n    due: Date\n    r: std.Repeat (repeats: due)\n    s: Text = \"\"\n}\naction a(t: T) {\n    t.s = t.r.label()\n}",
        "type T {\n    m: std.Reminder (reminds: due, at)\n    at: Option<Time>\n    due: Date\n    when: Option<Time>\n}\naction a(t: T) uses time {\n    t.when = .some(t.m.at(t.due, t.at))\n}",
        "type T {\n    due: Date\n    m: std.Reminder (reminds: due)\n    s: Text = \"\"\n}\naction a(t: T) {\n    t.s = t.m.label()\n}",
    ];
    for src in cases {
        let wasm = compile_body(src).wasm;
        validate(&wasm);
    }
}
