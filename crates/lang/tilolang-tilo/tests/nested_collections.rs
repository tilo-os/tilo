//! A collection held by an embedded record: a record that is a field of another, not an item of a
//! collection. Its items move among themselves and some of them stand under others, as the items
//! of a record that is an item do — the collection a resource is in is found through the field
//! that holds its holder, not only through a collection.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::{Replica, Workspace, target};

const APP: &str = "so.tilo.lang.nested";

/// An embedded record's collection of items, each of which may stand under another.
const NESTED: &str = r#"app so.tilo.lang.nested "Nested"

type Item {
    name: Text
    parent: Option<Item>
}

type Inner {
    items: Collection<Item>
}

type Root {
    inner: Inner
}

root Root

default view window for Root

view window(r: Root) {
    text("Nested")
}

action add(r: Root, name: Text) {
    r.inner.items.insert(Item(name: name))
}

action move_before(r: Root, moved: Item, before: Item) {
    r.inner.items.move(moved, before: before)
}

action move_after(r: Root, moved: Item, after: Item) {
    r.inner.items.move(moved, after: after)
}

action nest(r: Root, moved: Item, above: Item) {
    r.inner.items.place(moved, .parent, under: above)
}

action lift(r: Root, moved: Item) {
    r.inner.items.place(moved, .parent, inside: .none)
}
"#;

/// The root of a fresh workbench.
fn root() -> (Replica, ResourceRef) {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(NESTED, "")]);
    let at = ada.root(APP);
    ada.commit();
    (ada, at)
}

/// The items of the embedded record's collection, in the order the collection keeps them.
fn items(ada: &Replica, root: &ResourceRef) -> Vec<ResourceRef> {
    let inner = match ada.host.runtime().read(root, "inner") {
        Ok(Value::Ref(inner)) => target(&Value::Ref(inner)),
        other => panic!("the inner record: {other:?}"),
    };
    match ada.host.runtime().read(&inner, "items") {
        Ok(Value::List(items)) => items.iter().map(target).collect(),
        other => panic!("the inner's items: {other:?}"),
    }
}

/// The names of the items, in the collection's order.
fn names(ada: &Replica, root: &ResourceRef) -> Vec<String> {
    items(ada, root)
        .iter()
        .map(|item| match ada.host.runtime().read(item, "name") {
            Ok(Value::Text(name)) => name,
            other => panic!("an item's name: {other:?}"),
        })
        .collect()
}

/// Three items, a, b and c, in that order.
fn three(ada: &Replica, root: &ResourceRef) -> Vec<ResourceRef> {
    for name in ["a", "b", "c"] {
        ada.call(APP, "add", vec![Value::resource(root), Value::text(name)]);
    }
    ada.commit();
    let items = items(ada, root);
    assert_eq!(names(ada, root), ["a", "b", "c"]);
    items
}

/// An item of a collection inside an embedded record moves before and after another, as one of a
/// collection on an item does.
#[test]
fn an_embedded_records_items_move_before_and_after_each_other() {
    let (ada, root) = root();
    let items = three(&ada, &root);
    ada.call(
        APP,
        "move_before",
        vec![
            Value::resource(&root),
            Value::resource(&items[2]),
            Value::resource(&items[0]),
        ],
    );
    assert_eq!(names(&ada, &root), ["c", "a", "b"]);

    ada.call(
        APP,
        "move_after",
        vec![
            Value::resource(&root),
            Value::resource(&items[2]),
            Value::resource(&items[0]),
        ],
    );
    assert_eq!(names(&ada, &root), ["a", "c", "b"]);
}

/// An item of an embedded record's collection stands under another and comes back out, its order
/// kept through the collection.
#[test]
fn an_embedded_records_items_stand_under_another_and_come_out() {
    let (ada, root) = root();
    let items = three(&ada, &root);
    ada.call(
        APP,
        "nest",
        vec![
            Value::resource(&root),
            Value::resource(&items[1]),
            Value::resource(&items[0]),
        ],
    );
    let parent = match ada.host.runtime().read(&items[1], "parent") {
        Ok(Value::Opt(Some(parent))) => target(&parent),
        other => panic!("the item's parent: {other:?}"),
    };
    assert_eq!(parent, items[0], "the item stands under the one above it");
    assert_eq!(names(&ada, &root), ["a", "b", "c"], "beside it, as placed");

    ada.call(
        APP,
        "lift",
        vec![Value::resource(&root), Value::resource(&items[1])],
    );
    match ada.host.runtime().read(&items[1], "parent") {
        Ok(Value::Opt(None)) => {}
        other => panic!("the item stands at the top level again: {other:?}"),
    }
}
