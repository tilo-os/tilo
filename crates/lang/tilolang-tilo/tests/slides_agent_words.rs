//! An agent editing a slide text box's words through the field edit every resource publishes
//! (`edit-text` on `body`): the box holds the words it set, and the deck's window shows them.

use tilolang_runtime::{Actor, Value};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::PlanRequest;

use crate::fixtures::{Replica, Workspace};

const SLIDES: (&str, &str) = (
    include_str!("../../../../apps/tl/slides/app.tl"),
    include_str!("../../../../apps/tl/slides/app.ids"),
);
const APP: &str = "so.tilo.slides";

fn person(name: &str) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name))
}

fn read(replica: &Replica, at: &Value, field: &str) -> Value {
    let Value::Ref(at) = at else {
        panic!("a resource: {at:?}")
    };
    replica
        .host
        .runtime()
        .read(&at.target, field)
        .unwrap_or_else(|error| panic!("the {field} of {at:?}: {error}"))
}

fn view_id(replica: &Replica, name: &str) -> String {
    let meta = replica.host.package(APP).unwrap().meta;
    meta.views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("no view {name}"))
        .id
        .clone()
}

/// The first box on the deck's starter slide.
fn first_box(replica: &Replica, deck: &Value) -> Value {
    let Value::List(slides) = read(replica, deck, "slides") else {
        panic!("a deck's slides read as a list")
    };
    let slide = slides.first().expect("the starter slide").clone();
    let Value::List(elements) = read(replica, &slide, "elements") else {
        panic!("a slide's elements read as a list")
    };
    elements.first().expect("a box on the slide").clone()
}

/// An agent's `edit-text` of a box's `body` is the box's words, and the deck's window draws
/// the editor bound to them.
#[test]
fn an_agents_field_edit_of_a_boxes_words_shows_in_the_deck() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[SLIDES]);
    ada.host.set_clock(|| "2026-11-24T10:00:00Z".to_owned());
    let deck = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();

    let words = first_box(&ada, &deck);
    let Value::Ref(box_ref) = &words else {
        panic!("a box: {words:?}")
    };
    let id = box_ref.target.id_string();

    // The field edit every resource publishes, as an agent calls it on the box's words.
    let meta = ada.host.package(APP).unwrap().meta;
    let kind = meta.kind_of("Element");
    let args = serde_json::Map::from_iter([
        ("field".to_owned(), serde_json::json!("body")),
        ("value".to_owned(), serde_json::json!("The harbour at noon")),
    ]);
    ada.carry_out(&PlanRequest {
        app: APP,
        kind: &kind,
        action_id: tilolang_tilo::action::EDIT_TEXT_ACTION,
        resource: Some(&id),
        args: &args,
        caller: None,
        from: None,
    })
    .expect("the field edit runs");
    ada.commit();

    let Value::Rich(body) = read(&ada, &words, "body") else {
        panic!("a box's words read as rich text")
    };
    assert_eq!(
        body.written_text(),
        "The harbour at noon",
        "the box holds the words the agent set"
    );

    // The deck's view shows them: the editor the slide's place holds draws the box's words.
    let mut view = ada
        .host
        .runtime()
        .open_view(&person("ada"), &view_id(&ada, "deck"), vec![deck])
        .expect("the deck's window opens");
    view.poll_patches().unwrap();
    let mut editors = 0usize;
    of_kind(
        view.tree(),
        "ui.BlockEditor",
        &mut editors,
        &mut |node| matches!(node.props.get("value"), Some(Value::Rich(rich)) if rich.written_text() == "The harbour at noon"),
    );
    assert!(editors > 0, "the deck draws the box's words the agent set");
}

fn of_kind(
    node: &tilolang_runtime::UiNode,
    kind: &str,
    out: &mut usize,
    holds: &mut dyn FnMut(&tilolang_runtime::UiNode) -> bool,
) {
    if node.kind == kind && holds(node) {
        *out = out.saturating_add(1);
    }
    for child in &node.children {
        of_kind(child, kind, out, holds);
    }
}
