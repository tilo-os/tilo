//! Reflection over the log: `created(r)`, `updated(r)` and `history(r)` read who made each
//! version of a resource, when and by which action, from the room record's changes, on every
//! replica. A scenario's `as who at time` blocks are recorded as that person's, at that time.

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, Principal, Value};

use crate::fixtures::Replica;

const TALK: &str = r#"app so.tilo.lang.talk "Talk"

type Line {
    said: Text
}

type Talk {
    lines: Collection<Line>
}

root Talk

default view talk for Talk

action say(t: Talk, said: Text) -> Line {
    t.lines.insert(Line(said: said, ))
}

action reword(l: Line, said: Text) {
    l.said = said
}

/// Who wrote a line and when, who changed it last, and how many versions it has.
action describe(l: Line) -> Text {
    if let c = created(l), u = updated(l) {
        "{c.who} {c.at} {u.who} {u.at} {u.what} {history(l).len()}"
    } else {
        "unknown"
    }
}

/// The time an action runs at.
action stamp(t: Talk) -> Text {
    "{now}"
}

view talk(t: Talk) {
    stack {
        for l in t.lines {
            if let c = created(l) {
                text("{l.said} ({c.who} at {c.at})")
            }
        }
    }
}

scenario "a morning" {
    let t = root Talk
    as person("Mia") at 2026-06-29T07:02Z {
        say(t, "Morning!")
    }
    as person("Leo") at 2026-06-29T07:20Z {
        say(t, "Hi Mia")
        stamp(t)
    }
}
"#;

fn call(r: &Replica, who: &str, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package("so.tilo.lang.talk").unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap()
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new(who), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

fn text(v: Value) -> String {
    match v {
        Value::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

#[test]
fn versions_name_who_made_them_when_and_by_which_action_on_every_replica() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(TALK, "")]);
    let talk = ada.host.root_of("so.tilo.lang.talk").unwrap();
    ada.commit();
    ada.host
        .runtime()
        .set_clock(|| "2026-10-28T09:00:00Z".to_owned());
    let line = call(
        &ada,
        "ada",
        "say",
        vec![Value::resource(&talk), Value::text("hello")],
    );
    ada.commit();
    ada.host
        .runtime()
        .set_clock(|| "2026-10-28T09:05:00Z".to_owned());
    call(
        &ada,
        "ada",
        "reword",
        vec![line.clone(), Value::text("hello there")],
    );
    ada.commit();
    let described = text(call(&ada, "ada", "describe", vec![line.clone()]));
    assert_eq!(
        described,
        "ada 2026-10-28T09:00:00Z ada 2026-10-28T09:05:00Z Reword 2"
    );
    // Another replica reads the same versions from the record.
    let mut ben = ada.fork("ben", &[(TALK, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    ben.host
        .runtime()
        .set_clock(|| "2026-10-28T10:00:00Z".to_owned());
    call(&ben, "ben", "reword", vec![line.clone(), Value::text("hi")]);
    ben.commit();
    assert_eq!(
        text(call(&ben, "ben", "describe", vec![line.clone()])),
        "ada 2026-10-28T09:00:00Z ben 2026-10-28T10:00:00Z Reword 3"
    );
    ada.sync_with(&mut ben);
    assert_eq!(
        text(call(&ada, "ada", "describe", vec![line])),
        "ada 2026-10-28T09:00:00Z ben 2026-10-28T10:00:00Z Reword 3"
    );
}

#[test]
fn a_scenario_records_each_block_as_its_person_at_its_time() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(TALK, "")]);
    let talk = ada.host.root_of("so.tilo.lang.talk").unwrap();
    ada.commit();
    ada.host
        .runtime()
        .set_clock(|| "2026-10-28T09:00:00Z".to_owned());
    let scenario = loaded[0].meta.scenarios[0].action.clone();
    ada.host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.as_str(), vec![]))
        .unwrap();
    ada.commit();
    let lines = match ada.host.runtime().read(&talk, "lines").unwrap() {
        Value::List(items) => items,
        other => panic!("{other:?}"),
    };
    let described: Vec<String> = lines
        .iter()
        .map(|l| text(call(&ada, "ada", "describe", vec![l.clone()])))
        .collect();
    assert_eq!(
        described,
        [
            // Seed data is the scenario's doing, so history names it as the action.
            "scenario:Mia 2026-06-29T07:02:00Z scenario:Mia 2026-06-29T07:02:00Z a morning 1",
            "scenario:Leo 2026-06-29T07:20:00Z scenario:Leo 2026-06-29T07:20:00Z a morning 1",
        ]
    );
    // The window shows them as the log has them.
    let view = ada.view("so.tilo.lang.talk");
    let shown = format!("{:?}", view.tree());
    assert!(
        shown.contains("Morning! (scenario:Mia at 2026-06-29T07:02:00Z)"),
        "{shown}"
    );
}

#[test]
fn the_last_version_is_the_latest_on_every_replica_whichever_order_they_arrive_in() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(TALK, "")]);
    let talk = ada.host.root_of("so.tilo.lang.talk").unwrap();
    ada.commit();
    ada.host
        .runtime()
        .set_clock(|| "2026-10-28T09:00:00Z".to_owned());
    let line = call(
        &ada,
        "ada",
        "say",
        vec![Value::resource(&talk), Value::text("hello")],
    );
    ada.commit();
    let mut ben = ada.fork("ben", &[(TALK, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    // Each changes the line apart: Ada later than Ben, though Ben's change reaches Ada after
    // her own, and Ada's reaches Ben after his.
    ada.host
        .runtime()
        .set_clock(|| "2026-10-28T10:00:00Z".to_owned());
    call(
        &ada,
        "ada",
        "reword",
        vec![line.clone(), Value::text("hi all")],
    );
    ada.commit();
    ben.host
        .runtime()
        .set_clock(|| "2026-10-28T09:30:00Z".to_owned());
    call(&ben, "ben", "reword", vec![line.clone(), Value::text("hi")]);
    ben.commit();
    ada.sync_with(&mut ben);
    let want = "ada 2026-10-28T09:00:00Z ada 2026-10-28T10:00:00Z Reword 3";
    assert_eq!(
        text(call(&ada, "ada", "describe", vec![line.clone()])),
        want
    );
    assert_eq!(text(call(&ben, "ben", "describe", vec![line])), want);
}

/// Topics, each with its notes, listed by when each last changed, the latest first.
const TOPICS: &str = r#"app so.tilo.lang.topics "Topics"

type Note {
    said: Text
}

type Topic {
    title: Text
    notes: Collection<Note>
}

type Topics {
    topics: Collection<Topic>
}

root Topics

default view board for Topics

action open_topic(b: Topics, title: Text) -> Topic {
    b.topics.insert(Topic(title: title))
}

action post(t: Topic, said: Text) -> Note {
    t.notes.insert(Note(said: said))
}

row view topic_row for Topic

view board(b: Topics) {
    std.list(b, b.topics.sort_by(t -> updated(t), desc))
}

view topic_row(t: Topic) {
    row(t.title)
}
"#;

fn shown(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
    out.extend(node.props.values().filter_map(|value| match value {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        shown(child, out);
    }
}

fn list_key(node: &tilolang_runtime::UiNode) -> Option<String> {
    if node.kind == "ui.VirtualList" {
        return Some(node.key.clone());
    }
    node.children.iter().find_map(list_key)
}

/// The topics the first `n` rows of a view's list show, in order, as a virtual list asks for
/// them.
fn rows(view: &mut tilolang_runtime::ViewInstance, n: usize) -> Vec<String> {
    let list = list_key(view.tree()).expect("a list");
    let mut out = Vec::new();
    for i in 0..n {
        if let Some(row) = view.row(&list, i).unwrap() {
            shown(row, &mut out);
        }
    }
    out.retain(|t| t.starts_with("topic "));
    out
}

/// A room of `n` topics, opened one a minute from 08:00, and the root.
fn topics(n: usize) -> (Replica, tilolang_runtime::value::ResourceRef, Vec<Value>) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(TOPICS, "")]);
    let root = ada.host.root_of("so.tilo.lang.topics").unwrap();
    ada.commit();
    let mut made = Vec::new();
    for i in 0..n {
        let at = format!(
            "2026-10-28T{:02}:{:02}:00Z",
            (i / 60).saturating_add(8),
            i % 60
        );
        ada.host.runtime().set_clock(move || at.clone());
        made.push(call_in(
            &ada,
            "so.tilo.lang.topics",
            "open_topic",
            vec![Value::resource(&root), Value::text(format!("topic {i}"))],
        ));
        ada.commit();
    }
    (ada, root, made)
}

fn call_in(r: &Replica, app: &str, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(app).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap()
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new(&r.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

#[test]
fn a_list_sorted_by_when_each_last_changed_reads_its_order_from_the_index() {
    let (mut ada, _, made) = topics(3);
    let mut view = ada.view("so.tilo.lang.topics");
    assert_eq!(rows(&mut view, 3), ["topic 2", "topic 1", "topic 0"]);
    // A note posted in the oldest topic is its latest change: it comes first.
    ada.host
        .runtime()
        .set_clock(|| "2026-10-28T12:00:00Z".to_owned());
    call_in(
        &ada,
        "so.tilo.lang.topics",
        "post",
        vec![made[0].clone(), Value::text("news")],
    );
    ada.commit();
    view.poll_patches().unwrap();
    assert_eq!(rows(&mut view, 3), ["topic 0", "topic 2", "topic 1"]);
}

#[test]
fn opening_a_list_sorted_by_versions_reads_what_it_shows_whatever_the_size() {
    let work = |n: usize| {
        let (ada, _, _) = topics(n);
        let before = ada.host.store().read_stats();
        let mut view = ada.view("so.tilo.lang.topics");
        let first = rows(&mut view, 10).first().cloned();
        let after = ada.host.store().read_stats();
        (
            first,
            after.doc.entries - before.doc.entries,
            after.evaluated - before.evaluated,
            after.index.pages - before.index.pages,
        )
    };
    let (small_first, small_entries, small_evaluated, small_pages) = work(40);
    let (large_first, large_entries, large_evaluated, large_pages) = work(400);
    assert_eq!(small_first.as_deref(), Some("topic 39"));
    assert_eq!(large_first.as_deref(), Some("topic 399"));
    // Nothing of the record, no item checked one by one, and the pages of the rows shown.
    assert_eq!((small_entries, large_entries), (0, 0));
    assert_eq!((small_evaluated, large_evaluated), (0, 0));
    assert!(
        large_pages <= small_pages + 2,
        "{large_pages} pages at 400, {small_pages} at 40"
    );
}
