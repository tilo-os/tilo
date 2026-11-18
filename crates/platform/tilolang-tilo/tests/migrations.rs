//! A package's migrations (`migration action`): a migration may call an action of its own, the
//! first start of a package fills its fresh root, and a second open changes nothing.

use automerge::AutoCommit;
use tilolang_runtime::UiNode;

use crate::fixtures::Replica;

/// A gallery whose first start fills its root by an action it calls.
const GALLERY: &str = r#"app so.tilo.gallery "Gallery"

type Group {
  title: Text
}

type Gallery {
  groups: Collection<Group>
}

action fill(g: Gallery) {
  if g.groups.is_empty() {
    g.groups.insert(Group(title: "Everyday parts"))
    g.groups.insert(Group(title: "Collections"))
  }
}

migration action seed(g: Gallery) {
  fill(g)
}

root Gallery

default view board for Gallery

view board(g: Gallery) {
  page {
    for group in g.groups {
      text(group.title)
    }
  }
}
"#;

const APP: &str = "so.tilo.gallery";

/// The words the window shows.
fn texts(node: &UiNode, out: &mut Vec<String>) {
    if (node.kind == "text" || node.kind == "ui.Text")
        && let Some(tilolang_runtime::Value::Text(text)) = node.props.get("value")
    {
        out.push(text.clone());
    }
    for child in &node.children {
        texts(child, out);
    }
}

/// Do the packages' migrations once over this workspace, as its window does when it opens.
fn migrate(replica: &Replica) -> usize {
    replica.host.migrate()
}

fn shown(replica: &Replica) -> Vec<String> {
    let view = replica.view(APP);
    let mut out = Vec::new();
    texts(view.tree(), &mut out);
    out
}

#[test]
fn a_migration_that_calls_an_action_fills_a_fresh_root_and_a_second_open_changes_nothing() {
    let (mut replica, packages) = Replica::new("ada", AutoCommit::new(), &[(GALLERY, "")]);
    // The root the package's window opens, as the first start makes it.
    assert!(shown(&replica).is_empty(), "a fresh root shows nothing yet");
    replica.commit();

    assert_eq!(migrate(&replica), 1, "the first start fills the root");
    replica.commit();
    assert_eq!(
        shown(&replica),
        vec!["Everyday parts".to_owned(), "Collections".to_owned()],
        "the root the action filled"
    );

    assert_eq!(migrate(&replica), 0, "it happened once");
    assert_eq!(replica.commit(), 0, "and made no change");
    let ids = packages[0].ids.clone();
    let peer = Replica::new("peer", replica.doc.fork(), &[(GALLERY, ids.as_str())]).0;
    assert_eq!(
        migrate(&peer),
        0,
        "a peer opening it afterwards changes nothing"
    );
    assert_eq!(
        shown(&peer),
        vec!["Everyday parts".to_owned(), "Collections".to_owned()],
        "a peer reads what the seed wrote"
    );
}
