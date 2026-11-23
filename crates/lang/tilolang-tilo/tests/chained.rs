//! A list method chain read the store's way: `items.filter(..).sort_by(..).map(..)` in a
//! function hands each step what the step before it read, as the same chain split into `let`
//! steps does.

use crate::fixtures::Workspace;
use tilolang_runtime::{Actor, UiNode, Value};

use crate::fixtures::Replica;

/// A bag whose items a function reads in one chained expression, and in `let` steps.
const CHAIN: &str = r#"app so.tilo.chain "Chain"

type Item {
  group: Text merge register
  title: Text merge register
  rank: Int
}

type Bag {
  items: Collection<Item>
}

action seed(b: Bag) {
  b.items.insert(Item(group: "a", title: "first", rank: 2))
  b.items.insert(Item(group: "b", title: "other", rank: 0))
  b.items.insert(Item(group: "a", title: "second", rank: 1))
}

root Bag

default view main for Bag

view main(b: Bag) {
  for t in chained(b, "a") {
    text(t)
  }
  for t in stepped(b, "a") {
    text(t)
  }
}

fn chained(b: Bag, group: Text) -> List<Text> {
  return b.items.filter(i -> i.group == group).sort_by(i -> i.rank).map(i -> i.title)
}

fn stepped(b: Bag, group: Text) -> List<Text> {
  let mine = b.items.filter(i -> i.group == group)
  let keys = mine.sort_by(i -> i.rank).map(i -> i.title)
  return keys
}
"#;

const APP: &str = "so.tilo.chain";

/// Every text the view shows, in order.
fn texts(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .filter(|node| node.kind == "text" || node.kind == "ui.Text")
        .filter_map(|node| match node.props.get("value") {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// The bag seeded with its items: the replica, and the root to open.
fn seeded() -> (Replica, tilolang_runtime::ResourceRef) {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(CHAIN, "")]);
    let meta = packages[0].meta.clone();
    let bag = ada.host.root_of(APP).unwrap();
    ada.commit();
    let seed = meta
        .actions
        .iter()
        .find(|a| a.name == "seed")
        .unwrap()
        .id
        .clone();
    ada.host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (seed.as_str(), vec![Value::resource(&bag)]),
        )
        .unwrap();
    (ada, bag)
}

#[test]
fn a_chain_reads_what_its_steps_read() {
    let (ada, bag) = seeded();
    // As the layout's own writes leave it: the items written, not yet indexed.
    let just_written = texts(ada.host.open_resource(&bag, None).unwrap().tree());
    ada.commit();
    let indexed = texts(ada.host.open_resource(&bag, None).unwrap().tree());
    assert_eq!(
        just_written,
        ["second", "first", "second", "first"],
        "just written: the chained list, then the same steps split into lets"
    );
    assert_eq!(
        indexed,
        ["second", "first", "second", "first"],
        "indexed: the chained list, then the same steps split into lets"
    );
}
