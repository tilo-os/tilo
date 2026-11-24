//! The gallery app (`apps/tl/gallery`) as a window first opens it: a store with nothing in it
//! shows the gallery's components, every group filled, from the first frame.

use crate::fixtures::Workspace;
use tilolang_runtime::UiNode;
use tilolang_runtime::value::Value;

use crate::fixtures::Replica;

/// The gallery's own sources, as its package is written beside the app.
const GALLERY: &str = concat!(
    include_str!("../../../../apps/tl/gallery/app.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/charts_flow.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/charts_series.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/charts_shape.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/controls.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/custom.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/data.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/feedback.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/map.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/media.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/pies.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/shell.tl"),
    "\n",
    include_str!("../../../../apps/tl/gallery/states.tl"),
);

const IDS: &str = include_str!("../../../../apps/tl/gallery/app.ids");

const APP: &str = "so.tilo.lang.gallery";

/// Every word the window shows.
fn words(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn shows(shown: &[String], word: &str) -> bool {
    shown.iter().any(|seen| seen.contains(word))
}

/// Open the gallery as its window does on a store that holds nothing: the root is made with what
/// a new gallery starts with, one change committed before the view is drawn.
fn window(ids: &str) -> Replica {
    let (replica, _packages) = Replica::new("ada", Workspace::new(), &[(GALLERY, ids)]);
    let root = replica.host.root_of(APP).expect("the gallery's root");
    assert_eq!(
        replica.commit(),
        1,
        "the gallery and all it starts with are its making"
    );
    let person =
        tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"));
    {
        let runtime = replica.host.runtime();
        assert!(
            runtime.unseen(&person, &root).unwrap().is_empty(),
            "nothing of what the gallery starts with is a change to catch anyone up on"
        );
        assert_eq!(
            runtime.read(&root, "controls_title").unwrap(),
            Value::Text("Controls".into()),
            "the gallery's text fields start at the words its type declares"
        );
        let history = runtime.versions(&root).unwrap();
        assert!(
            history.len() == 1 && history[0].action == tilolang_runtime::store::CREATE_ACTION,
            "the gallery's history is its making: {history:?}"
        );
    }
    replica
}

#[test]
fn a_store_with_nothing_in_it_shows_every_group_filled() {
    let mut replica = window(IDS);
    let view = replica.view(APP);
    let first = words(view.tree());
    for group in [
        "Everyday parts",
        "Collections",
        "Media and places",
        "The shell",
        "Your own",
    ] {
        assert!(
            first.iter().any(|word| word == group),
            "the sidebar's `{group}`: {first:?}"
        );
    }
    assert!(
        shows(&first, "Release planning"),
        "the basics' page: {first:?}"
    );

    // The group's documents are made with the root, so a person meets them at once: each
    // page's items stand in the window, and each collection's components read them.
    let root = replica.host.root_of(APP).expect("the gallery's root");
    for (view, kind, count, what) in [
        ("release_board", "ui.BoardCard", 4, "the board's cards"),
        ("data", "ui.DataTableRow", 3, "the table's rows"),
        ("data", "ui.TreeView", 1, "the tree"),
        (
            "data",
            "ui.ConversationEntry",
            5,
            "the conversation's lines",
        ),
        ("data", "ui.CardGridCard", 3, "the grid's cards"),
        ("data", "ui.ActivityEntry", 2, "the timeline's changes"),
        ("media", "ui.MediaCard", 4, "the library's photos"),
        ("shell_parts", "ui.Chrome", 2, "the bar and the dock"),
    ] {
        let id = replica
            .host
            .package(APP)
            .and_then(|package| package.view_id(view))
            .unwrap_or_else(|| panic!("the package names `{view}`"));
        let page = replica
            .host
            .open_resource(&root, Some(&id))
            .unwrap_or_else(|error| panic!("`{view}` opens: {error}"));
        let shown = page.tree().of_kind(kind).len();
        assert!(
            shown >= count,
            "`{view}`: {what}, {shown} of {kind} stand in the page"
        );
    }
    for (view, word) in [
        ("data", "Readme.md"),
        ("data", "Moved to Done"),
        ("data", "Are we ready to ship the release tomorrow?"),
        ("media", "Dawn"),
        ("shell_parts", "Tasks"),
    ] {
        let id = replica
            .host
            .package(APP)
            .and_then(|package| package.view_id(view))
            .unwrap_or_else(|| panic!("the package names `{view}`"));
        let page = replica
            .host
            .open_resource(&root, Some(&id))
            .unwrap_or_else(|error| panic!("`{view}` opens: {error}"));
        let words_on_the_page = words(page.tree());
        assert!(
            shows(&words_on_the_page, word),
            "`{view}` shows its `{word}`: {words_on_the_page:?}"
        );
    }
    let _ = &mut replica;
}

/// The store the workspace keeps every app's resources in decides a change against the app's
/// surface: the gallery's root is created there as a change of its own, and the store admits it.
/// The gallery's surface describes its record by the fields it lists, and its `$create` writes
/// that record; a surface read without its own record refused every app's first change.
#[test]
fn the_gallery_s_surface_admits_the_root_its_host_creates() {
    use std::collections::BTreeMap;

    use tilo_resource_store::{Cell, Proposal, Received, Resource, Schema, Stamp, Store};

    let (_, packages) = Replica::new("ada", Workspace::new(), &[(GALLERY, "")]);
    let meta = &packages[0].meta;
    let mut there = Store::new();
    for surface in tilolang_tilo::surface::package_surfaces(meta, &tilolang_runtime::SourceWords) {
        there.declare(Schema::from_surface(&surface));
    }
    let kind = tilolang_tilo::meta::MetaExt::resource_type(meta);
    // A field its record holds, by the id a document stores it under.
    let field = there
        .schema()
        .get(&kind)
        .and_then(|ty| ty.fields.first())
        .map(|field| field.name.clone())
        .expect("the gallery's record declares its fields");
    let making = Stamp {
        verb: tilolang_runtime::store::CREATE_ACTION.to_owned(),
        invoker: "ada".to_owned(),
        invocation: "i1".to_owned(),
        membership: vec!["m1".to_owned()],
        ..Stamp::default()
    };
    let written = Store::new()
        .put(
            &kind,
            "g",
            Resource {
                readers: "*".to_owned(),
                parent: None,
                order: None,
                fields: BTreeMap::from([(
                    field,
                    Cell::One(tilo_resource_store::Value::from("Gallery")),
                )]),
            },
            &making,
        )
        .expect("the making is a change");
    let mut refused = Vec::new();
    for write in &written {
        refused.extend(
            there
                .receive(
                    &write.doc,
                    &write.readers,
                    vec![Received {
                        change: write.change.clone(),
                        author: [7; 32],
                        claimed: None,
                    }],
                    &|_: &Proposal<'_>| true,
                )
                .expect("the store takes the change"),
        );
    }
    assert!(
        refused.is_empty(),
        "the gallery's root is created: {refused:?}"
    );
    let all = |_: &str| true;
    assert!(there.get(&kind, "g", &all).is_some(), "the root stands");
}
