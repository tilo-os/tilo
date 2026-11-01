//! A view of one tilolang package drawn inline in another's tree (shell.md §8, "The canvas
//! composes views"): the callee's view runs in the caller's runtime as the callee's own code,
//! over the reading its app publishes of the resource, and its presses reach the callee's
//! actions as child invocations carrying the callee's module as their author.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::AutoCommit;
use serde_json::json;
use tilolang::IdsLock;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{ModuleHash, Principal, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

/// The callee: an app whose items another app's views show, with a row view of its own.
const CALLEE: &str = r#"app test.b "B"

type Item {
  title: Text
  done: Bool
}

action done(x: Item) {
  x.done = true
}

default view b_card for Item

view b_card(x: Item) {
  column {
    text("Item")
    text(x.title)
  }
}

view b_row(x: Item) {
  row(gap: .related) {
    text(x.title)
    button("Done", on: done(x)) as finish
  }
}
"#;

/// The caller: a view that calls the callee's view by name, and one that composes it from text
/// as an arrangement names it.
const CALLER: &str = r#"app test.a "A"

use test.b as b

view a_main(x: b.Item) {
  column {
    text("Shopping")
    b.b_row(x)
  }
}

view a_canvas() {
  column {
    if let r = resource("item-1") {
      show(r, view: "b_row")
      show(r)
    } else {
      text("no item yet")
    }
    if let r = resource("c-1") {
      show(r)
    } else {
      text("a well")
    }
  }
}
"#;

const CALLEE_APP: &str = "test.b";

/// Records what the composed view invokes of the callee's app.
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// A host running the caller's package, with the callee's package loaded as another app's
/// (its module, its records elsewhere), and the callee's item reading `title`.
struct Room {
    host: Host,
    callee: ModuleHash,
    caller: ModuleHash,
    callee_meta: tilolang_runtime::Meta,
    caller_meta: tilolang_runtime::Meta,
}

fn room(title: &str) -> Room {
    let room = unread_room();
    room.host
        .runtime()
        .set_reading(
            &item(),
            &json!({ "title": title, "done": false }).to_string(),
        )
        .unwrap();
    room
}

/// The same host before any reading of the callee's item arrived.
fn unread_room() -> Room {
    let doc = AutoCommit::new();
    let host = Host::new(&doc, Principal::new("ada"), || None).unwrap();
    let shown = tilolang::compile_package(CALLEE, &IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let shown_module = host
        .runtime()
        .load_foreign_package(&shown.wasm, shown.meta.clone())
        .unwrap();
    let showing =
        tilolang::compile_package(CALLER, &IdsLock::new(), std::slice::from_ref(&shown.meta))
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let showing_module = tilolang_runtime::Runtime::module_hash(&showing.wasm);
    host.load_compiled(&showing).unwrap();
    Room {
        host,
        callee: shown_module,
        caller: showing_module,
        callee_meta: shown.meta,
        caller_meta: showing.meta,
    }
}

/// The callee's item, as its app publishes it: by app and type, under its id.
fn item() -> ResourceRef {
    ResourceRef::parse_id("item-1", format!("{CALLEE_APP}#Item")).expect("a resource id")
}

fn view_id(meta: &tilolang_runtime::Meta, name: &str) -> String {
    meta.views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("the view `{name}`"))
        .id
        .clone()
}

fn anchor_id(meta: &tilolang_runtime::Meta, name: &str) -> String {
    meta.views
        .iter()
        .flat_map(|view| &view.anchors)
        .find(|anchor| anchor.name == name)
        .unwrap_or_else(|| panic!("the anchor `{name}`"))
        .id
        .clone()
}

impl Room {
    fn main(&self) -> ViewInstance {
        self.host
            .open_resource(&item(), Some(&view_id(&self.caller_meta, "a_main")))
            .unwrap()
    }

    fn canvas(&self) -> ViewInstance {
        self.host
            .runtime()
            .open_view(
                &Principal::new("ada"),
                &view_id(&self.caller_meta, "a_canvas"),
                Vec::new(),
            )
            .unwrap()
    }

    fn finish(&self) -> String {
        anchor_id(&self.callee_meta, "finish")
    }
}

/// Every text the tree shows, in order.
fn texts(node: &UiNode) -> Vec<String> {
    fn walk(node: &UiNode, out: &mut Vec<String>) {
        if (node.kind == "text" || node.kind == "ui.Text")
            && let Some(Value::Text(t)) = node.props.get("value")
        {
            out.push(t.clone());
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

fn kinds(node: &UiNode) -> Vec<String> {
    fn walk(node: &UiNode, out: &mut Vec<String>) {
        out.push(node.kind.clone());
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

#[test]
fn a_view_of_another_package_renders_inline_over_its_reading() {
    let room = room("Milk");
    let view = room.main();
    let shown = texts(view.tree());
    assert_eq!(shown, vec!["Shopping", "Milk"], "{:#?}", view.tree());
    assert!(
        view.find(&room.finish()).is_some(),
        "the callee's button stands in the caller's tree: {:#?}",
        view.tree()
    );
    assert!(
        kinds(view.tree()).iter().all(|kind| kind != "window"),
        "nothing opens a window"
    );
}

#[test]
fn a_new_reading_runs_the_composed_view_again() {
    let room = room("Milk");
    let mut view = room.main();
    room.host
        .runtime()
        .set_reading(
            &item(),
            &json!({ "title": "Eggs", "done": false }).to_string(),
        )
        .unwrap();
    let patches = view.poll_patches().unwrap();
    assert!(!patches.is_empty(), "the new reading reaches the tree");
    assert_eq!(texts(view.tree()), vec!["Shopping", "Eggs"]);
}

#[test]
fn a_press_in_the_composed_view_invokes_the_callees_action_as_the_callee() {
    let room = room("Milk");
    let view = room.main();
    let sent = Rc::new(RefCell::new(Vec::new()));
    room.host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.press(&room.finish())
        .expect("the button invokes the callee's action");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app,
            action,
            author,
            subject,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one invocation of the callee's action: {sent:?}");
    };
    let done = room
        .callee_meta
        .actions
        .iter()
        .find(|a| a.name == "done")
        .expect("the action `done`");
    assert_eq!(app, CALLEE_APP);
    assert_eq!(action, &done.id);
    assert_eq!(subject.as_ref(), Some(&item()));
    assert_eq!(
        author.as_ref(),
        Some(&room.callee),
        "the callee's code asked, never the caller's"
    );
    assert_ne!(author.as_ref(), Some(&room.caller));
}

#[test]
fn show_draws_another_apps_resource_found_by_id_through_a_named_or_its_default_view() {
    let room = room("Milk");
    let view = room.canvas();
    let shown = texts(view.tree());
    // The item through `b_row` (named), then through `b_card` (its type's default); the id no
    // reading named stands as nothing found.
    assert_eq!(
        shown,
        vec!["Milk", "Item", "Milk", "a well"],
        "{:#?}",
        view.tree()
    );
    assert!(
        view.find(&room.finish()).is_some(),
        "the drawn view's button stands in the tree"
    );
}

#[test]
fn a_press_in_a_composed_place_carries_the_callees_author() {
    let room = room("Milk");
    let view = room.canvas();
    let sent = Rc::new(RefCell::new(Vec::new()));
    room.host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.press(&room.finish())
        .expect("the press reaches the callee");
    let sent = sent.borrow();
    let [Invocation::Action { app, author, .. }] = sent.as_slice() else {
        panic!("one invocation: {sent:?}");
    };
    assert_eq!(app, CALLEE_APP);
    assert_eq!(author.as_ref(), Some(&room.callee));
}

#[test]
fn a_new_reading_runs_a_composed_place_again() {
    let room = room("Milk");
    let mut view = room.canvas();
    room.host
        .runtime()
        .set_reading(
            &item(),
            &json!({ "title": "Eggs", "done": false }).to_string(),
        )
        .unwrap();
    assert!(!view.poll_patches().unwrap().is_empty());
    assert_eq!(texts(view.tree()), vec!["Eggs", "Item", "Eggs", "a well"]);
}

#[test]
fn a_composed_place_waits_for_its_first_reading_and_draws_it_when_it_arrives() {
    let room = unread_room();
    let mut view = room.canvas();
    let shown = texts(view.tree());
    assert_eq!(
        shown,
        vec!["no item yet", "a well"],
        "before its reading the resource is not known here: {shown:?}"
    );
    room.host
        .runtime()
        .set_reading(
            &item(),
            &json!({ "title": "Milk", "done": false }).to_string(),
        )
        .unwrap();
    assert!(
        !view.poll_patches().unwrap().is_empty(),
        "its first reading runs the view that looked it up"
    );
    assert_eq!(texts(view.tree()), vec!["Milk", "Item", "Milk", "a well"]);
}
