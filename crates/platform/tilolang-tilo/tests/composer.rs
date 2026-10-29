//! Typing is personal: text being composed is each person's lens state, passed to the action that
//! sends it, so two people typing in one conversation never overwrite each other.

use automerge::AutoCommit;
use tilolang_runtime::{UiNode, Value, ViewInstance};

use crate::fixtures::Replica;

const APP: &str = "so.tilo.lang.talk";

const TALK: &str = r#"app so.tilo.lang.talk "Talk"

type Line {
    said: Text
}

type Talk {
    lines: Collection<Line>
}

root Talk

default view talk for Talk

/// Posts a line.
action send(t: Talk, said: Text) (icon: .send) {
    if said != "" {
        t.lines.insert(Line(said: said))
    }
}

view talk(t: Talk) {
    lens draft: Text
    stack {
        for l in t.lines {
            text(l.said)
        }
        input(bind: draft, placeholder: "Message")
        button(send(t, draft).then(set(draft, "")))
    }
}
"#;

fn of_kind<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        of_kind(child, kind, out);
    }
}

fn key(view: &ViewInstance, kind: &str) -> String {
    let mut found = Vec::new();
    of_kind(view.tree(), kind, &mut found);
    found
        .first()
        .unwrap_or_else(|| panic!("no {kind}"))
        .key
        .clone()
}

fn entry(view: &ViewInstance) -> Value {
    view.bound_value(&key(view, "ui.TextInput")).unwrap()
}

fn lines(view: &ViewInstance) -> Vec<String> {
    let mut found = Vec::new();
    of_kind(view.tree(), "ui.Text", &mut found);
    found
        .iter()
        .filter_map(|n| match n.props.get("value") {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn two_people_typing_in_one_conversation_do_not_overwrite_each_other() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(TALK, "")]);
    ada.host.root_of(APP).unwrap();
    ada.commit();
    let mut ben = ada.fork("ben", &[(TALK, &loaded[0].ids)]);
    let mut view_ada = ada.view(APP);
    let mut view_ben = ben.view(APP);

    // Both type at once.
    view_ada
        .edit(&key(&view_ada, "ui.TextInput"), &Value::text("Morning!"))
        .unwrap();
    view_ben
        .edit(&key(&view_ben, "ui.TextInput"), &Value::text("Hi Ada"))
        .unwrap();
    ada.commit();
    ben.commit();
    ada.sync_with(&mut ben);
    view_ada.poll_patches().unwrap();
    view_ben.poll_patches().unwrap();
    assert_eq!(entry(&view_ada), Value::text("Morning!"));
    assert_eq!(entry(&view_ben), Value::text("Hi Ada"));

    // Ada sends: her line is posted and her entry cleared; Ben's text stays his.
    view_ada.press(&key(&view_ada, "ui.Button")).unwrap();
    ada.commit();
    ada.sync_with(&mut ben);
    view_ada.poll_patches().unwrap();
    view_ben.poll_patches().unwrap();
    assert_eq!(entry(&view_ada), Value::text(""));
    assert_eq!(entry(&view_ben), Value::text("Hi Ada"));
    assert_eq!(lines(&view_ben), ["Morning!"]);

    view_ben.press(&key(&view_ben, "ui.Button")).unwrap();
    ben.commit();
    ada.sync_with(&mut ben);
    view_ada.poll_patches().unwrap();
    assert_eq!(lines(&view_ada), ["Morning!", "Hi Ada"]);
    assert_eq!(entry(&view_ada), Value::text(""));
}
