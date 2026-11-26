//! A root that holds a named collection (`type Board { pins: Collection<Pin> }`,
//! `root board: Board`) and a collection of a union of types, through the workspace document and
//! a second replica.

use crate::fixtures::Workspace;
use tilolang_runtime::Value;

use crate::fixtures::Replica;

const APP: &str = "so.tilo.lang.pins";

const PINS: &str = r#"app so.tilo.lang.pins "Pins"

type Note {
    text: Text
}

type Link {
    url: Text
    visits: Int merge counter
}

type Pin = Note | Link

type Board {
    pins: Collection<Pin>
}

root board: Board

default view pins for Board

action note(b: Board, text: Text) -> Pin {
    b.pins.insert(Note(text: text))
}

action link(b: Board, url: Text) -> Pin {
    b.pins.insert(Link(url: url))
}

/// What each pin says, in order.
action read(b: Board) -> Text {
    let out = ""
    for p in b.pins {
        out += match p {
            n: Note => "[{n.text}]",
            l: Link => "<{l.url}>",
        }
    }
    out
}

view pins(b: Board) {
    stack {
        for p in b.pins {
            match p {
                n: Note => text(n.text),
                l: Link => text(l.url),
            }
        }
    }
}
"#;

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    r.call(APP, action, args)
}

#[test]
fn a_named_root_collection_holds_items_of_each_type_of_its_union() {
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(PINS, "")]);
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
    let ben = ada.fork("ben", &[(PINS, &loaded[0].ids)]);
    call(
        &ben,
        "note",
        vec![Value::resource(&board), Value::text("eggs")],
    );
    ben.commit();
    ada.sync_with(&ben);
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
