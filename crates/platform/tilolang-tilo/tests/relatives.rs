//! The items under a node by a field of theirs, and the ones above it: a query of the store's
//! index over the field's column, answering what the node's subtree holds whatever the
//! collection's size, and following a change of a parent and a removal as it lands.

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, Principal, Value};

use crate::fixtures::Replica;

/// A tree whose nodes hang under each other by `parent`.
const TREE: &str = r#"app so.tilo.tree "Tree"

type Node {
  title: Text
  parent: Option<Node> = .none
}

type Tree {
  nodes: Collection<Node>
}

root Tree

default view tree for Tree

view tree(t: Tree) {
  for n in t.nodes {
    text(n.title)
  }
}

action add(t: Tree, title: Text, under: Option<Node>) -> Node {
  t.nodes.insert(Node(title: title, parent: under))
}

action titled(t: Tree, title: Text) -> Option<Node> {
  return t.nodes.find(n -> n.title == title)
}

action descendants_of(t: Tree, n: Node) -> List<Node> {
  return t.nodes.descendants(n, .parent)
}

action ancestors_of(t: Tree, n: Node) -> List<Node> {
  return t.nodes.ancestors(n, .parent)
}

action reparent(t: Tree, n: Node, under: Node) {
  n.parent = .some(under)
}

action hang(t: Tree, n: Node, under: Node) -> List<Node> {
  n.parent = .some(under)
  return t.nodes.descendants(under, .parent)
}

action add_under(t: Tree, title: Text, under: Node) -> List<Node> {
  t.nodes.insert(Node(title: title, parent: .some(under)))
  return t.nodes.descendants(under, .parent)
}

action remove_branch(t: Tree, n: Node) {
  for x in t.nodes.descendants(n, .parent) {
    t.nodes.remove(x)
  }
  t.nodes.remove(n)
}
"#;

const APP: &str = "so.tilo.tree";

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
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

/// Ada's replica of an empty tree, and the tree's root.
fn workspace() -> (Replica, Value) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(TREE, "")]);
    let root = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    (ada, root)
}

fn add(ada: &mut Replica, root: &Value, title: &str, under: Option<&Value>) -> Value {
    let made = call(
        ada,
        "add",
        vec![
            root.clone(),
            Value::text(title),
            under.map_or_else(Value::none, |node| Value::some(node.clone())),
        ],
    );
    ada.commit();
    made
}

fn titles(ada: &Replica, items: Value) -> Vec<String> {
    let Value::List(items) = items else {
        panic!("a list: {items:?}")
    };
    items
        .iter()
        .map(|item| {
            let Value::Ref(at) = item else {
                panic!("a reference: {item:?}")
            };
            match ada.host.runtime().read(&at.target, "title").unwrap() {
                Value::Text(title) => title,
                other => panic!("a text: {other:?}"),
            }
        })
        .collect()
}

#[test]
fn descendants_and_ancestors_follow_a_parent_through_reparenting_and_removing() {
    let (mut ada, root) = workspace();
    let top = add(&mut ada, &root, "top", None);
    let mid = add(&mut ada, &root, "mid", Some(&top));
    let leaf = add(&mut ada, &root, "leaf", Some(&mid));
    let side = add(&mut ada, &root, "side", Some(&top));
    let loose = add(&mut ada, &root, "loose", None);
    let under = |ada: &Replica, node: &Value, action: &str| {
        titles(ada, call(ada, action, vec![root.clone(), node.clone()]))
    };

    // The item itself is neither under nor above itself; the items under it come in the order
    // the collection keeps them however deep the tree hangs, and the ones above it from the
    // nearest.
    assert_eq!(under(&ada, &top, "descendants_of"), ["mid", "leaf", "side"]);
    assert_eq!(under(&ada, &mid, "descendants_of"), ["leaf"]);
    assert_eq!(under(&ada, &leaf, "descendants_of"), Vec::<String>::new());
    assert_eq!(under(&ada, &loose, "descendants_of"), Vec::<String>::new());
    assert_eq!(under(&ada, &leaf, "ancestors_of"), ["mid", "top"]);
    assert_eq!(under(&ada, &side, "ancestors_of"), ["top"]);
    assert_eq!(under(&ada, &top, "ancestors_of"), Vec::<String>::new());
    assert_eq!(under(&ada, &loose, "ancestors_of"), Vec::<String>::new());

    // A node moved under another is read under it from then on, and no longer under the one it
    // hung under.
    call(
        &ada,
        "reparent",
        vec![root.clone(), leaf.clone(), top.clone()],
    );
    ada.commit();
    assert_eq!(under(&ada, &top, "descendants_of"), ["mid", "leaf", "side"]);
    assert_eq!(under(&ada, &mid, "descendants_of"), Vec::<String>::new());
    assert_eq!(under(&ada, &leaf, "ancestors_of"), ["top"]);

    // A node hung under another in an action is under it in that same action, before the index
    // has the write, and a node added in an action is under the one it was added to at once.
    let hung = call(&ada, "hang", vec![root.clone(), loose.clone(), mid.clone()]);
    assert_eq!(titles(&ada, hung), ["loose"]);
    ada.commit();
    assert_eq!(under(&ada, &mid, "descendants_of"), ["loose"]);
    assert_eq!(under(&ada, &loose, "ancestors_of"), ["mid", "top"]);
    let added = call(
        &ada,
        "add_under",
        vec![root.clone(), Value::text("fresh"), leaf.clone()],
    );
    assert_eq!(titles(&ada, added), ["fresh"]);
    ada.commit();
    assert_eq!(
        under(&ada, &top, "descendants_of"),
        ["mid", "leaf", "side", "loose", "fresh"]
    );
    let fresh = titled(&ada, &root, "fresh");
    assert_eq!(under(&ada, &fresh, "ancestors_of"), ["leaf", "top"]);

    // A node removed with what hangs under it is neither, and the rest of the tree answers as
    // it is now.
    call(&ada, "remove_branch", vec![root.clone(), mid.clone()]);
    ada.commit();
    assert_eq!(
        under(&ada, &top, "descendants_of"),
        ["leaf", "side", "fresh"]
    );
    assert_eq!(under(&ada, &mid, "descendants_of"), Vec::<String>::new());
    assert_eq!(under(&ada, &leaf, "ancestors_of"), ["top"]);
}

/// The node titled `title` in the tree.
fn titled(ada: &Replica, root: &Value, title: &str) -> Value {
    match call(ada, "titled", vec![root.clone(), Value::text(title)]) {
        Value::Opt(Some(node)) => *node,
        other => panic!("a node titled {title}: {other:?}"),
    }
}
