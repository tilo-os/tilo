//! A root that is a named collection (`type Board = Collection<Card>`, `root board: Board`) and
//! a collection of a union of types, through the room record and a second replica.

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, Principal, Value};

use crate::fixtures::Replica;

const APP: &str = "so.tilo.lang.pins";

const PINS: &str = r#"app so.tilo.lang.pins "Pins"

type Note {
    text: Text (title)
}

type Link {
    url: Text (title)
    visits: Int merge counter
}

type Pin = Note | Link

type Board = Collection<Pin>

root board: Board

default view pins for board

action note(b: Board, text: Text) -> Pin {
    b.insert(Note(text: text))
}

action link(b: Board, url: Text) -> Pin {
    b.insert(Link(url: url))
}

/// What each pin says, in order.
action read(b: Board) -> Text {
    let out = ""
    for p in b {
        out += match p {
            n: Note => "[{n.text}]",
            l: Link => "<{l.url}>",
        }
    }
    out
}

view pins(b: Board) {
    stack {
        for p in b {
            match p {
                n: Note => text(n.text),
                l: Link => text(l.url),
            }
        }
    }
}
"#;

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
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
        .invoke_action(&Principal::new(r.name.as_str()), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

#[test]
fn a_named_root_collection_holds_items_of_each_type_of_its_union() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(PINS, "")]);
    let board = ada.host.root_of(APP).unwrap();
    ada.commit();
    call(
        &ada,
        "note",
        vec![Value::resource(&board), Value::text("milk")],
    );
    call(
        &ada,
        "link",
        vec![Value::resource(&board), Value::text("tilo.so")],
    );
    ada.commit();
    assert_eq!(
        call(&ada, "read", vec![Value::resource(&board)]),
        Value::text("[milk]<tilo.so>")
    );
    let mut ben = ada.fork("ben", &[(PINS, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    call(
        &ben,
        "note",
        vec![Value::resource(&board), Value::text("eggs")],
    );
    ben.commit();
    ada.sync_with(&mut ben);
    assert_eq!(
        call(&ada, "read", vec![Value::resource(&board)]),
        Value::text("[milk]<tilo.so>[eggs]")
    );
    let view = ada.view(APP);
    let shown = format!("{:?}", view.tree());
    for want in ["milk", "tilo.so", "eggs"] {
        assert!(shown.contains(want), "{want} in {shown}");
    }
}
