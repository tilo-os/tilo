//! Reflection over the log: `created(r)`, `updated(r)` and `history(r)` read who made each
//! version of a resource, when and by which action, from the workspace record's changes, on every
//! replica. A scenario's `invoke(..., by: who, at: time)` calls are recorded as that person's,
//! at that time.

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, Principal, Value};
use tilolang_tilo::host::RootView;

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

action write(t: Talk, said: Text) -> Line {
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

/// The version that last changed what a line says: who, when and by which action.
action changed_by(l: Line) -> Text {
    if let v = changed(l, .said) {
        "{v.who} {v.at} {v.what}"
    } else {
        "never"
    }
}

/// Every version of a line, oldest first: who made it, when and by which action.
action trail(l: Line) -> Text {
    history(l).map(v -> "{v.who} {v.at} {v.what}").join(" | ")
}

/// The steps of its history, newest first: who made each and how many changes it folds.
action folds(l: Line) -> Text {
    steps(l).map(s -> "{s.who} {s.changes}").join(" | ")
}

/// What the person looking has not seen of it, oldest first: who made it and when.
action unread(l: Line) -> Text {
    unseen(l).map(v -> "{v.who} {v.at}").join(" | ")
}

/// The time an action runs at.
action stamp(t: Talk) -> Text uses time {
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
    invoke(write(t, "Morning!"), by: person("Mia"), at: 2026-06-29T07:02Z)
    invoke(write(t, "Hi Mia"), by: person("Leo"), at: 2026-06-29T07:20Z)
    invoke(stamp(t), by: person("Leo"), at: 2026-06-29T07:20Z)
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
    ada.host.set_clock(|| "2026-10-28T09:00:00Z".to_owned());
    let line = call(
        &ada,
        "ada",
        "write",
        vec![Value::resource(&talk), Value::text("hello")],
    );
    ada.commit();
    ada.host.set_clock(|| "2026-10-28T09:05:00Z".to_owned());
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
    ben.host.set_clock(|| "2026-10-28T10:00:00Z".to_owned());
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
    ada.host.set_clock(|| "2026-10-28T09:00:00Z".to_owned());
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
            // Each row names the action the call ran, as every change does, and the person and
            // moment the scenario's `invoke` gave it.
            "scenario:Mia 2026-06-29T07:02:00Z scenario:Mia 2026-06-29T07:02:00Z Write 1",
            "scenario:Leo 2026-06-29T07:20:00Z scenario:Leo 2026-06-29T07:20:00Z Write 1",
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
    ada.host.set_clock(|| "2026-10-28T09:00:00Z".to_owned());
    let line = call(
        &ada,
        "ada",
        "write",
        vec![Value::resource(&talk), Value::text("hello")],
    );
    ada.commit();
    let mut ben = ada.fork("ben", &[(TALK, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    // Each changes the line apart: Ada later than Ben, though Ben's change reaches Ada after
    // her own, and Ada's reaches Ben after his.
    ada.host.set_clock(|| "2026-10-28T10:00:00Z".to_owned());
    call(
        &ada,
        "ada",
        "reword",
        vec![line.clone(), Value::text("hi all")],
    );
    ada.commit();
    ben.host.set_clock(|| "2026-10-28T09:30:00Z".to_owned());
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

/// The same two changes reaching each replica in either order: each person's change arrives
/// before or after the other replica's own, and both read the one made later as the last
/// version either way.
#[test]
fn the_last_version_is_the_same_whichever_change_reaches_a_replica_first() {
    for ada_first in [true, false] {
        let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(TALK, "")]);
        let talk = ada.host.root_of("so.tilo.lang.talk").unwrap();
        ada.commit();
        ada.host.set_clock(|| "2026-10-28T09:00:00Z".to_owned());
        let line = call(
            &ada,
            "ada",
            "write",
            vec![Value::resource(&talk), Value::text("hello")],
        );
        ada.commit();
        let mut ben = ada.fork("ben", &[(TALK, &loaded[0].ids)]);
        ben.host.sync_from(&ben.doc);
        ada.host.set_clock(|| "2026-10-28T10:00:00Z".to_owned());
        call(
            &ada,
            "ada",
            "reword",
            vec![line.clone(), Value::text("hi all")],
        );
        ada.commit();
        ben.host.set_clock(|| "2026-10-28T09:30:00Z".to_owned());
        call(&ben, "ben", "reword", vec![line.clone(), Value::text("hi")]);
        ben.commit();
        // Ada's change reaches Ben first, or his reaches her first.
        if ada_first {
            ada.send_to(&mut ben);
            ben.send_to(&mut ada);
        } else {
            ben.send_to(&mut ada);
            ada.send_to(&mut ben);
        }
        let want = "ada 2026-10-28T09:00:00Z ada 2026-10-28T10:00:00Z Reword 3";
        assert_eq!(
            text(call(&ada, "ada", "describe", vec![line.clone()])),
            want,
            "ada's change first: {ada_first}"
        );
        assert_eq!(
            text(call(&ben, "ben", "describe", vec![line])),
            want,
            "ada's change first: {ada_first}"
        );
    }
}

/// One line changed by three changes made apart, reaching the replica that holds none of them
/// in either order: every replica reads one history, folds one set of steps, and counts what
/// is unseen the same.
#[test]
fn every_replica_reads_one_history_and_its_steps_whichever_order_changes_arrive_in() {
    for ada_first in [true, false] {
        let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(TALK, "")]);
        let talk = ada.host.root_of("so.tilo.lang.talk").unwrap();
        ada.host.set_clock(|| "2026-10-28T09:00:00Z".to_owned());
        let line = call(
            &ada,
            "ada",
            "write",
            vec![Value::resource(&talk), Value::text("hello")],
        );
        ada.commit();
        let mut ben = ada.fork("ben", &[(TALK, &loaded[0].ids)]);
        let mut cy = ada.fork("cy", &[(TALK, &loaded[0].ids)]);
        ben.host.sync_from(&ben.doc);
        cy.host.sync_from(&cy.doc);
        // Ben changes the line twice, one on top of the other, and Ada once between them in
        // time, made apart from both.
        ada.host.set_clock(|| "2026-10-28T09:30:00Z".to_owned());
        call(
            &ada,
            "ada",
            "reword",
            vec![line.clone(), Value::text("hi all")],
        );
        ada.commit();
        ben.host.set_clock(|| "2026-10-28T09:20:00Z".to_owned());
        call(&ben, "ben", "reword", vec![line.clone(), Value::text("hi")]);
        ben.commit();
        ben.host.set_clock(|| "2026-10-28T09:40:00Z".to_owned());
        call(
            &ben,
            "ben",
            "reword",
            vec![line.clone(), Value::text("hello")],
        );
        ben.commit();
        // Cy holds none of them and receives them in one order or the other; the two authors
        // exchange theirs.
        if ada_first {
            ada.send_to(&mut cy);
            ben.send_to(&mut cy);
        } else {
            ben.send_to(&mut cy);
            ada.send_to(&mut cy);
        }
        ada.sync_with(&mut ben);
        ada.sync_with(&mut cy);
        ben.sync_with(&mut cy);
        let readers = [("ada", &ada), ("ben", &ben), ("cy", &cy)];
        let history = "ada 2026-10-28T09:00:00Z Write | ben 2026-10-28T09:20:00Z Reword \
                       | ada 2026-10-28T09:30:00Z Reword | ben 2026-10-28T09:40:00Z Reword";
        for (who, replica) in readers {
            assert_eq!(
                text(call(replica, who, "trail", vec![line.clone()])),
                history,
                "{who} reads one history; ada's change first: {ada_first}"
            );
        }
        // Four versions, four steps, newest first: each step holds one change's versions, and
        // no step folds changes made apart, however the changes reached the replica.
        for (who, replica) in readers {
            assert_eq!(
                text(call(replica, who, "folds", vec![line.clone()])),
                "ben 1 | ada 1 | ben 1 | ada 1",
                "{who} folds one set of steps; ada's change first: {ada_first}"
            );
        }
        // What is unseen is the versions after the person's own, oldest first, in the one order
        // every replica keeps.
        for (who, replica, unread) in [
            ("ada", &ada, "ben 2026-10-28T09:40:00Z"),
            ("ben", &ben, ""),
            (
                "cy",
                &cy,
                "ada 2026-10-28T09:00:00Z | ben 2026-10-28T09:20:00Z \
                 | ada 2026-10-28T09:30:00Z | ben 2026-10-28T09:40:00Z",
            ),
        ] {
            assert_eq!(
                text(call(replica, who, "unread", vec![line.clone()])),
                unread,
                "{who} counts what is unseen; ada's change first: {ada_first}"
            );
        }
    }
}

/// Changes made one on top of another within one second: each is the last version once it is
/// made, before and after it is committed, and on a replica that receives them all at once;
/// never the one it was made on, whichever change's hash is smaller.
#[test]
fn of_changes_within_one_second_the_last_version_is_the_one_made_last() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(TALK, "")]);
    let talk = ada.host.root_of("so.tilo.lang.talk").unwrap();
    ada.commit();
    ada.host.set_clock(|| "2026-10-28T09:00:00Z".to_owned());
    let line = call(
        &ada,
        "ada",
        "write",
        vec![Value::resource(&talk), Value::text("hello")],
    );
    ada.commit();
    let mut ben = ada.fork("ben", &[(TALK, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    // Two changes' hashes order either way half the time: twelve rewords in one second
    // would each come out first by hash about once.
    for round in 0..12 {
        let who = if round % 2 == 0 { "mia" } else { "ada" };
        call(
            &ada,
            who,
            "reword",
            vec![line.clone(), Value::text(format!("hello {round}"))],
        );
        let want = format!("ada 2026-10-28T09:00:00Z {who} 2026-10-28T09:00:00Z Reword");
        let provisional = text(call(&ada, "ada", "describe", vec![line.clone()]));
        assert!(
            provisional.starts_with(&want),
            "round {round}, before the record holds it: {provisional}"
        );
        ada.commit();
        let committed = text(call(&ada, "ada", "describe", vec![line.clone()]));
        assert!(
            committed.starts_with(&want),
            "round {round}, once the record holds it: {committed}"
        );
    }
    ada.sync_with(&mut ben);
    assert_eq!(
        text(call(&ben, "ben", "describe", vec![line])),
        "ada 2026-10-28T09:00:00Z ada 2026-10-28T09:00:00Z Reword 13"
    );
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

action retitle(t: Topic, title: Text) {
    t.title = title
}

row view topic_row for Topic

view board(b: Topics) {
    std.list(b.topics.sort_by(t -> updated(t), desc)) { (t, at) ->
        ui.ListItem(item: t, presence: at) {
            topic_row(t)
        }
    }
}

view lately(b: Topics) {
    std.list(b.topics.sort_by(t -> changed(t, .title)?.at, desc)) { (t, at) ->
        ui.ListItem(item: t, presence: at) {
            topic_row(t)
        }
    }
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

/// A workspace of `n` topics, opened one a minute from 08:00, and the root.
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
        ada.host.set_clock(move || at.clone());
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
    ada.host.set_clock(|| "2026-10-28T12:00:00Z".to_owned());
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

/// `r`'s person takes their last step back, as their window's Undo does.
#[cfg(feature = "record")]
fn undo(r: &mut Replica) {
    use collaboration::history_source::verbs::{EDIT_TYPE, HistoryEdit};
    use tilo_actions::{ActionContext, ActionInvoker, EncodedAction};
    use tilolang_tilo::instance::record::HostHistoryEdit;

    let edit = HistoryEdit::Undo {
        steps: None,
        run: None,
        by: None,
        to: None,
        step: None,
    };
    let payload = serde_json::to_vec(&edit).unwrap();
    let action: HostHistoryEdit = serde_json::from_slice(&payload).unwrap();
    let context = ActionContext::for_resource("workspace", "history", "history")
        .with_invoker(ActionInvoker::actor(r.name.clone()));
    tilo_actions::apply_local_with_context(
        &mut r.doc,
        &action,
        EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload,
        },
        Some(r.name.as_bytes()),
        context,
    )
    .expect("undo is never refused");
    r.doc.commit();
    r.host.sync_from(&r.doc);
}

#[test]
fn a_field_names_the_version_that_last_changed_it() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(TALK, "")]);
    let talk = ada.host.root_of("so.tilo.lang.talk").unwrap();
    ada.commit();
    ada.host.set_clock(|| "2026-10-28T09:00:00Z".to_owned());
    let line = call(
        &ada,
        "ada",
        "write",
        vec![Value::resource(&talk), Value::text("hello")],
    );
    ada.commit();
    assert_eq!(
        text(call(&ada, "ada", "changed_by", vec![line.clone()])),
        "ada 2026-10-28T09:00:00Z Write",
        "the version that made it set every field it was made with"
    );
    ada.host.set_clock(|| "2026-10-28T09:05:00Z".to_owned());
    call(
        &ada,
        "ada",
        "reword",
        vec![line.clone(), Value::text("hello there")],
    );
    ada.commit();
    assert_eq!(
        text(call(&ada, "ada", "changed_by", vec![line.clone()])),
        "ada 2026-10-28T09:05:00Z Reword"
    );
    // Somebody else's change of it names them, on every replica.
    let mut ben = ada.fork("ben", &[(TALK, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    ben.host.set_clock(|| "2026-10-28T10:00:00Z".to_owned());
    call(&ben, "ben", "reword", vec![line.clone(), Value::text("hi")]);
    ben.commit();
    let want = "ben 2026-10-28T10:00:00Z Reword";
    assert_eq!(
        text(call(&ben, "ben", "changed_by", vec![line.clone()])),
        want
    );
    ada.sync_with(&mut ben);
    assert_eq!(text(call(&ada, "ada", "changed_by", vec![line])), want);
}

/// A change taken back is no change of the field: what last changed it is the version still in
/// effect, so the day a task was finished goes back with the box.
#[cfg(feature = "record")]
#[test]
fn undo_puts_a_field_back_to_the_version_that_last_changed_it() {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(TALK, "")]);
    let talk = ada.host.root_of("so.tilo.lang.talk").unwrap();
    ada.commit();
    ada.host.set_clock(|| "2026-10-28T09:00:00Z".to_owned());
    let line = call(
        &ada,
        "ada",
        "write",
        vec![Value::resource(&talk), Value::text("hello")],
    );
    ada.commit();
    ada.host.set_clock(|| "2026-10-28T09:05:00Z".to_owned());
    call(
        &ada,
        "ada",
        "reword",
        vec![line.clone(), Value::text("hello there")],
    );
    ada.commit();
    ada.host.set_clock(|| "2026-10-28T09:10:00Z".to_owned());
    undo(&mut ada);
    assert_eq!(
        text(call(&ada, "ada", "changed_by", vec![line.clone()])),
        "ada 2026-10-28T09:00:00Z Write",
        "the reword was taken back: what last changed the line is the version that made it"
    );
    let described = text(call(&ada, "ada", "describe", vec![line]));
    assert!(
        described.starts_with("ada 2026-10-28T09:00:00Z ada ") && described.ends_with(" History 3"),
        "while the line has a version for the undo itself: {described}"
    );
}

#[test]
fn a_list_sorted_by_when_a_field_last_changed_reads_that_field_from_the_index() {
    let (mut ada, _, made) = topics(3);
    let mut view = lately(&ada);
    assert_eq!(
        rows(&mut view, 3),
        ["topic 2", "topic 1", "topic 0"],
        "made one a minute apart"
    );
    // A note posted in the oldest topic changes the topic, its title no more than any other.
    ada.host.set_clock(|| "2026-10-28T12:00:00Z".to_owned());
    call_in(
        &ada,
        "so.tilo.lang.topics",
        "post",
        vec![made[0].clone(), Value::text("news")],
    );
    ada.commit();
    view.poll_patches().unwrap();
    assert_eq!(rows(&mut view, 3), ["topic 2", "topic 1", "topic 0"]);
    // Retitling it makes its title the latest change of the three.
    ada.host.set_clock(|| "2026-10-28T13:00:00Z".to_owned());
    call_in(
        &ada,
        "so.tilo.lang.topics",
        "retitle",
        vec![made[0].clone(), Value::text("topic 0 again")],
    );
    ada.commit();
    view.poll_patches().unwrap();
    assert_eq!(rows(&mut view, 3), ["topic 0 again", "topic 2", "topic 1"]);
}

/// The topics' board, through its view of when each title last changed.
fn lately(ada: &Replica) -> tilolang_runtime::ViewInstance {
    ada.host
        .open_layer(
            "so.tilo.lang.topics",
            &RootView {
                root: None,
                view: Some("lately".to_owned()),
            },
        )
        .expect("the lately view opens")
}

#[test]
fn opening_a_list_sorted_by_when_a_field_last_changed_reads_the_index_whatever_the_size() {
    let work = |n: usize| {
        let (ada, _, _) = topics(n);
        let before = ada.host.store().read_stats();
        let mut view = lately(&ada);
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
    // Nothing of the record, no item checked one by one, and the pages of the rows shown: a
    // field's last change is one value a row, kept as the log is applied.
    assert_eq!((small_entries, large_entries), (0, 0));
    assert_eq!((small_evaluated, large_evaluated), (0, 0));
    assert!(
        large_pages <= small_pages + 2,
        "{large_pages} pages at 400, {small_pages} at 40"
    );
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
