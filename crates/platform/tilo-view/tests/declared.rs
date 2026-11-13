//! `#[view]` declares a view, and the registry finds it by resource kind and name — the lookup a
//! shell does when a place names a view of a resource of an app written in Rust.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed step in an integration test is the assertion"
)]

use tilo_view::ViewRegistry;
use tilo_view::ui::{self, Gap};
use tilo_view::{Node, ResourceRef, ViewCx, ViewResource};
use tilo_view_macros::view;

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct Channel {
    name: String,
    handle: String,
    videos: Vec<Video>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct Video {
    title: String,
}

impl ViewResource for Channel {
    const KIND: &'static str = "test.channel";
    const APP: &'static str = "test";
}

impl ViewResource for Video {
    const KIND: &'static str = "test.video";
    const APP: &'static str = "test";
}

#[view(default)]
fn page(c: &Channel, cx: &ViewCx) -> Node {
    let mut children = vec![ui::TextBuilder::new(&cx.key("name"), &c.name).into()];
    for (index, video) in c.videos.iter().enumerate() {
        children.push(
            ui::ListRowBuilder::new(&cx.key(format!("v{index}")))
                .title(&video.title)
                .into(),
        );
    }
    ui::stack(&cx.key("page"), Gap::Sibling, children)
}

#[view]
fn short(c: &Channel, cx: &ViewCx) -> Node {
    ui::TextBuilder::new(&cx.key("name"), &c.name).into()
}

fn input(reading: serde_json::Value) -> tilo_view::registry::ViewInput<'static> {
    tilo_view::registry::ViewInput {
        target: ResourceRef {
            id: "test.channel:1".to_owned(),
            ty: "test.channel".to_owned(),
        },
        reading: Some(Box::leak(Box::new(reading))),
    }
}

fn cx() -> ViewCx {
    ViewCx::new(
        ResourceRef {
            id: "test.channel:1".to_owned(),
            ty: "test.channel".to_owned(),
        },
        "page",
        tilo_view::Principal {
            id: "Pat".to_owned(),
            name: "Pat".to_owned(),
        },
        std::collections::BTreeMap::new(),
    )
}

fn texts(node: &Node) -> Vec<String> {
    let mut out: Vec<String> = node
        .props
        .values()
        .filter_map(|value| match value {
            tilo_view::Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    for child in &node.children {
        out.extend(texts(child));
    }
    out
}

#[test]
fn a_view_is_found_by_its_types_kind_and_its_name() {
    assert!(ViewRegistry::get("test.channel", "page").is_some());
    assert!(ViewRegistry::get("test.channel", "short").is_some());
    assert!(
        ViewRegistry::get("test.channel", "nothing").is_none(),
        "a name no view declares finds none"
    );
}

#[test]
fn the_default_view_is_the_one_a_place_shows_unasked() {
    let default = ViewRegistry::default_for("test.channel").expect("page is default");
    assert_eq!(default.name, "page");
    assert_eq!(
        ViewRegistry::resolve("test.channel", "").map(|view| view.name),
        Some("page"),
        "a place naming no view draws the default"
    );
    assert_eq!(
        ViewRegistry::resolve("test.channel", "short").map(|view| view.name),
        Some("short"),
        "a place naming a view draws it"
    );
}

#[test]
fn the_view_reads_its_resource_and_draws_its_described_components() {
    let view = ViewRegistry::get("test.channel", "page").expect("page");
    let tree = (view.render)(
        &input(serde_json::json!({
            "name": "MKBHD",
            "handle": "@mkbhd",
            "videos": [{"title": "A phone"}, {"title": "A review"}],
        })),
        &cx(),
    );
    let shown = texts(&tree);
    for expected in ["MKBHD", "A phone", "A review"] {
        assert!(
            shown.contains(&expected.to_owned()),
            "{expected} shows: {shown:?}"
        );
    }
    assert!(tree.find("ui.ListRow").is_some() || has_kind(&tree, "ui.ListRow"));
    passes(&tree);
}

#[test]
fn a_resource_whose_reading_has_not_arrived_shows_what_it_can() {
    let view = ViewRegistry::get("test.channel", "page").expect("page");
    let tree = (view.render)(
        &tilo_view::registry::ViewInput {
            target: ResourceRef {
                id: "test.channel:2".to_owned(),
                ty: "test.channel".to_owned(),
            },
            reading: None,
        },
        &cx(),
    );
    passes(&tree);
}

fn has_kind(node: &Node, kind: &str) -> bool {
    node.kind == kind || node.children.iter().any(|child| has_kind(child, kind))
}

fn passes(tree: &Node) {
    let validator = tilo_view::patch::validate::Validator::new(
        tilo_view::node::catalogue(),
        tilo_view::patch::validate::PatchLimits::default(),
    );
    let batch = vec![tilo_view::patch::ToUi::Open {
        lane: tilo_view::patch::Lane::Root,
        target: ResourceRef {
            id: "test.channel:1".to_owned(),
            ty: "test.channel".to_owned(),
        },
        tree: tree.clone(),
        front: None,
    }];
    validator
        .to_ui(&batch)
        .expect("the view is one the host builds");
}
