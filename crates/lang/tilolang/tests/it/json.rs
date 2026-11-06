//! `json.decode<T>`, `json.encode` and `json.parse` as the checker reads them: the type a decode
//! states and what text can make, the values between `<` and `>`, a `json.Value` that is no
//! field's type, the six kinds a match over one names, and a `<` that stays a comparison.

use crate::common::codes;

const ITEMS: &str = "type Item {\n    title: Text\n}\n\n\
type Counted {\n    counts: Map<Int>\n}\n\n\
type Counter {\n    n: Int\n}\n\n\
enum State {\n    open\n    closed\n}\n";

fn with_items(body: &str) -> Vec<String> {
    codes(&format!("{ITEMS}{body}"))
}

#[test]
fn a_decode_states_the_type_it_reads_and_text_can_make() {
    for ty in [
        "Item",
        "List<Item>",
        "Option<Item>",
        "Counted",
        "Int",
        "Float",
        "Text",
        "Date",
        "State",
    ] {
        let read = format!(
            "action read(t: Text, c: Counter) {{\n    json.decode<{ty}>(t)\n    c.n = 0\n}}\n"
        );
        assert_eq!(with_items(&read), Vec::<String>::new(), "{ty}");
    }
    let encoded = "action read(c: Counter) {\n    c.n = json.encode(c.n).len()\n}\n";
    assert_eq!(with_items(encoded), Vec::<String>::new());

    // A collection and a set are a field's, not a value a call answers or takes.
    let collects = "action read(t: Text, c: Counter) {\n    json.decode<Collection<Item>>(t)\n    json.decode<Set<Item>>(t)\n    c.n = 0\n}\n";
    assert_eq!(with_items(collects), ["E023", "E023"]);

    // A name nothing declares is the resolver's own error, carried as it says it.
    let unknown = "action read(t: Text, c: Counter) {\n    json.decode<Nope>(t)\n    c.n = 0\n}\n";
    assert_eq!(with_items(unknown), ["E013"]);

    // What text cannot make.
    let unmade = "action read(t: Text, c: Counter) {\n    json.decode<Bytes>(t)\n    json.decode<Duration>(t)\n    c.n = 0\n}\n";
    assert_eq!(with_items(unmade), ["E020", "E020"]);
}

#[test]
fn a_call_between_angle_brackets_says_what_it_takes() {
    let no_type = "action read(t: Text, c: Counter) {\n    json.decode(t)\n    c.n = 0\n}\n";
    assert_eq!(with_items(no_type), ["E022"]);

    let parsed = "action read(t: Text, c: Counter) {\n    json.parse<Item>(t)\n    c.n = 0\n}\n";
    assert_eq!(with_items(parsed), ["E022"]);

    let encoded = "action read(c: Counter) {\n    json.encode<Int>(c.n)\n}\n";
    assert_eq!(with_items(encoded), ["E022"]);
}

#[test]
fn a_json_value_is_no_field_type() {
    let held = "type Box {\n    held: json.Value\n}\n";
    assert_eq!(with_items(held), ["E023"]);
}

#[test]
fn a_match_over_a_value_names_every_kind() {
    let partial = "action look(c: Counter, t: Text) {\n\
    match json.parse(t) {\n\
        .ok(value) => match value {\n\
            .object(fields) => { c.n = fields.keys().len() }\n\
            .array(items) => { c.n = items.len() }\n\
        }\n\
        .err(why) => { c.n = why.len() }\n\
    }\n\
}\n";
    assert_eq!(with_items(partial), ["E035"]);

    let all = "action look(c: Counter, t: Text) {\n\
    match json.parse(t) {\n\
        .ok(value) => match value {\n\
            .object(fields) => { c.n = fields.keys().len() }\n\
            .array(items) => { c.n = items.len() }\n\
            .string(text) => { c.n = text.len() }\n\
            .number(number) => { c.n = number.to_int() }\n\
            .bool(flag) => { c.n = if flag { 1 } else { 0 } }\n\
            .null => { c.n = 0 - 1 }\n\
        }\n\
        .err(why) => { c.n = why.len() }\n\
    }\n\
}\n";
    assert_eq!(with_items(all), Vec::<String>::new());

    let wrong = "action look(c: Counter, t: Text) {\n\
    match json.parse(t) {\n\
        .ok(value) => match value {\n\
            .numbers(number) => { c.n = number.to_int() }\n\
            .object(fields) => { c.n = fields.keys().len() }\n\
            .array(items) => { c.n = items.len() }\n\
            .string(text) => { c.n = text.len() }\n\
            .bool(flag) => { c.n = if flag { 1 } else { 0 } }\n\
            .null => { c.n = 0 - 1 }\n\
        }\n\
        .err(why) => { c.n = why.len() }\n\
    }\n\
}\n";
    let got = with_items(wrong);
    assert!(
        got.contains(&"E036".to_owned()),
        "`.numbers` is no kind: {got:?}"
    );
}

#[test]
fn a_less_than_after_a_name_is_still_a_comparison() {
    let compares = "fn under(a: Int, b: Int) -> Bool {\n    return a < b\n}\n\n\
action pick(c: Counter, a: Int, b: Int) {\n    c.n = if a < b { 1 } else { 2 }\n}\n";
    assert_eq!(with_items(compares), Vec::<String>::new());
}
