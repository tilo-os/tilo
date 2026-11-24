//! Record values end to end: a record built with its fields, read from text or taken out of a
//! list is a value its code edits field by field, in a fn or an action, without asking the store;
//! the store holds the edited values once an action inserts them, and a resource's field set
//! still writes the resource. A value of a type an action declares as a parameter's is one the
//! action may be handed: it reads it, and a change to it fails where it is written.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = r#"app so.tilo.stops "Stops"

type Stop {
    name: Text
    seq: Int
    late: Bool
    from_name: Text
}

type Line {
    stops: Collection<Stop>
    label: Text
    note: Text
}

root Line

default view line for Line

view line(l: Line) {
    text(l.label)
}

/// The first stop found, renamed and moved on.
fn first_renamed(found: List<Stop>) -> Stop {
    if let s = found.first() {
        s.name = "{s.name} (first)"
        s.seq += 10
        s.late = true
        s
    } else {
        Stop(name: "none", seq: 0, late: false, from_name: "")
    }
}

/// The stop a text describes, marked as read.
fn read_stop(text: Text) -> Stop {
    match json.decode<Stop>(text) {
        .ok(s) => {
            s.from_name = "decoded"
            s
        }
        .err(why) => Stop(name: why, seq: 0, late: false, from_name: ""),
    }
}

/// Two stops edited from one: each edit is its own, and the one they came from keeps its name.
fn two_from_one() -> List<Stop> {
    let base = Stop(name: "base", seq: 1, late: false, from_name: "")
    let a = base
    a.name = "a"
    let b = base
    b.name = "b"
    b.seq -= 1
    [base, a, b]
}

action keep(l: Line, found_text: Text, one_text: Text) {
    match json.decode<List<Stop>>(found_text) {
        .ok(found) => {
            l.stops.insert(first_renamed(found))
            for s in found {
                s.from_name = "kept"
                l.stops.insert(s)
            }
            if let first = found.first() {
                l.note = "first still {first.name}, from '{first.from_name}'"
            }
        }
        .err(why) => { l.note = why }
    }
    l.stops.insert(read_stop(one_text))
    for s in two_from_one() {
        l.stops.insert(s)
    }
    let made = l.stops.insert(Stop(name: "made", seq: 0, late: false, from_name: ""))
    made.from_name = "stored"
    l.label = "kept"
}
"#;

    const FOUND: &str = r#"[{"name":"Elm","seq":1,"late":false,"from_name":""},{"name":"Oak","seq":2,"late":false,"from_name":""}]"#;
    const ONE: &str = r#"{"name":"Ash","seq":5,"late":true,"from_name":"text"}"#;

    fn text(rt: &Runtime, at: &ResourceRef, field: &str) -> String {
        match rt.read(at, field).unwrap() {
            Value::Text(t) => t,
            other => panic!("{field}: {other:?}"),
        }
    }

    /// Each stop the line holds, as (name, seq, late, from_name).
    fn stops(rt: &Runtime, line: &ResourceRef) -> Vec<(String, i64, bool, String)> {
        let Value::List(items) = rt.read(line, "stops").unwrap() else {
            panic!("a list of stops");
        };
        items
            .iter()
            .map(|item| {
                let Value::Ref(r) = item else {
                    panic!("a stored stop: {item:?}");
                };
                let at = &*r.target;
                let seq = match rt.read(at, "seq").unwrap() {
                    Value::Int(n) => n,
                    other => panic!("seq: {other:?}"),
                };
                let late = match rt.read(at, "late").unwrap() {
                    Value::Bool(b) => b,
                    other => panic!("late: {other:?}"),
                };
                (text(rt, at, "name"), seq, late, text(rt, at, "from_name"))
            })
            .collect()
    }

    #[test]
    fn edited_record_values_are_what_the_store_holds_once_inserted() {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let line = rt.create("Line").unwrap();
        let keep = app.action("keep");
        rt.invoke_action(
            &me(),
            (
                keep.as_str(),
                vec![
                    Value::resource(&line),
                    Value::Text(FOUND.to_owned()),
                    Value::Text(ONE.to_owned()),
                ],
            ),
        )
        .unwrap();
        let s = |name: &str, seq: i64, late: bool, from: &str| {
            (name.to_owned(), seq, late, from.to_owned())
        };
        assert_eq!(
            stops(&rt, &line),
            vec![
                s("Elm (first)", 11, true, ""),
                s("Elm", 1, false, "kept"),
                s("Oak", 2, false, "kept"),
                s("Ash", 5, true, "decoded"),
                s("base", 1, false, ""),
                s("a", 1, false, ""),
                s("b", 0, false, ""),
                s("made", 0, false, "stored"),
            ],
            "each record value is stored as its code edited it, and a stored stop's field set \
             writes the stop"
        );
        assert_eq!(
            text(&rt, &line, "note"),
            "first still Elm, from ''",
            "editing a value taken out of a list leaves the list as it was"
        );
        assert_eq!(text(&rt, &line, "label"), "kept");
    }

    const IMPORTING: &str = r#"app so.tilo.importing "Importing"

type Part {
    name: Text
}

type Holder {
    parts: List<Part>
}

type Box {
    parts: Collection<Part>
    note: Text
}

root Box

default view box for Box

view box(b: Box) {
    text(b.note)
}

/// Keeps one part a file brings.
action keep_one(b: Box, p: Part) {
    b.parts.insert(p)
}

/// Reads the parts a file holds into the box.
action adopt(b: Box, text: Text) {
    match json.decode<Holder>(text) {
        .ok(h) => {
            for p in h.parts {
                keep_one(b, p)
            }
            b.note = "read"
        }
        .err(why) => { b.note = why }
    }
}

/// Renames one part a file brings, expecting the name to be kept.
action rename_one(b: Box, p: Part) {
    p.name = "renamed"
}

action rename_in(b: Box, text: Text) {
    match json.decode<Holder>(text) {
        .ok(h) => {
            for p in h.parts {
                rename_one(b, p)
            }
            b.note = "renamed"
        }
        .err(why) => { b.note = why }
    }
}
"#;

    const PARTS: &str = r#"{"parts":[{"name":"First"},{"name":"Second"}]}"#;

    fn part_names(rt: &Runtime, b: &ResourceRef) -> Vec<String> {
        let Value::List(items) = rt.read(b, "parts").unwrap() else {
            panic!("a list of parts");
        };
        items
            .iter()
            .map(|item| {
                let Value::Ref(r) = item else {
                    panic!("a stored part: {item:?}");
                };
                text(rt, &r.target, "name")
            })
            .collect()
    }

    #[test]
    fn a_decoded_record_reaches_the_action_that_takes_its_type() {
        let app = App::compile(IMPORTING, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let b = rt.create("Box").unwrap();
        let adopt = app.action("adopt");
        rt.invoke_action(
            &me(),
            (
                adopt.as_str(),
                vec![Value::resource(&b), Value::Text(PARTS.to_owned())],
            ),
        )
        .unwrap_or_else(|e| panic!("the value the decoder checked is handed over: {e}"));
        assert_eq!(part_names(&rt, &b), vec!["First", "Second"]);
        assert_eq!(text(&rt, &b, "note"), "read");
    }

    #[test]
    fn a_record_value_of_another_type_is_refused_by_its_name() {
        let app = App::compile(IMPORTING, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let b = rt.create("Box").unwrap();
        let holder = app
            .meta()
            .types
            .iter()
            .find(|t| t.name == "Holder")
            .expect("the app declares Holder")
            .id
            .clone();
        let keep = app.action("keep_one");
        let failed = rt
            .invoke_action(
                &me(),
                (
                    keep.as_str(),
                    vec![
                        Value::resource(&b),
                        Value::Rec(tilolang_runtime::RecordInit {
                            ty: holder,
                            fields: std::collections::BTreeMap::new(),
                        }),
                    ],
                ),
            )
            .expect_err("a Holder is no Part");
        let said = format!("{failed}");
        assert!(
            said.contains("a record of `Holder` is no `Part`"),
            "the refusal names the value's own type and the parameter's: {said}"
        );
    }

    #[test]
    fn a_value_an_action_changes_fails_where_it_is_written() {
        let app = App::compile(IMPORTING, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let b = rt.create("Box").unwrap();
        let rename = app.action("rename_in");
        let failed = rt
            .invoke_action(
                &me(),
                (
                    rename.as_str(),
                    vec![Value::resource(&b), Value::Text(PARTS.to_owned())],
                ),
            )
            .expect_err("a value names no resource to change");
        let said = format!("{failed}");
        assert!(
            said.contains("a record value an action is handed names no resource to change"),
            "the failure says what a handed value is: {said}"
        );
        assert!(part_names(&rt, &b).is_empty(), "nothing was kept");
    }
}
