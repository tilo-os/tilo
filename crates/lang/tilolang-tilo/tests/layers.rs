//! The window's layer: the root a host names and the view it shows over it. A host that shows a
//! screen of its own rather than a resource opens a view that takes nothing (`view phone()`),
//! which takes no root as an argument; a view that takes the root still gets it.

use crate::fixtures::{Replica, Workspace, nodes};
use tilolang_runtime::{UiNode, Value};
use tilolang_tilo::host::RootView;

const APP: &str = "so.tilo.layers";

const LAYERS: &str = r#"app so.tilo.layers "Layers"

type Board {
  title: Text
}

root Board

default view board for Board

view board(b: Board) {
  text("board {b.title}")
}

view phone() {
  text("phone")
}
"#;

/// Every text a layer shows, in order.
fn texts(node: &UiNode) -> Vec<String> {
    nodes(node)
        .into_iter()
        .filter(|node| node.kind == "text" || node.kind == "ui.Text")
        .filter_map(|node| match node.props.get("value") {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn host() -> Replica {
    let (ada, _packages) = Replica::new("ada", Workspace::new(), &[(LAYERS, "")]);
    ada
}

#[test]
fn a_view_that_takes_nothing_opens_over_a_root_without_being_given_one() {
    let ada = host();
    let view = ada
        .host
        .open_layer(
            APP,
            &RootView {
                root: Some("Board".to_owned()),
                view: Some("phone".to_owned()),
            },
        )
        .expect("a view that takes no arguments opens over the root");
    assert_eq!(texts(view.tree()), ["phone"]);
}

#[test]
fn a_view_that_takes_the_root_still_opens_with_it() {
    let ada = host();
    let view = ada
        .host
        .open_layer(
            APP,
            &RootView {
                root: Some("Board".to_owned()),
                view: Some("board".to_owned()),
            },
        )
        .expect("the root's view opens");
    assert!(
        texts(view.tree())
            .iter()
            .any(|said| said.starts_with("board")),
        "the view took the root: {:?}",
        texts(view.tree())
    );
}
