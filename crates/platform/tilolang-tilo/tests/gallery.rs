//! The gallery app (`apps/tl/gallery`) as a window first opens it: a store with nothing in it
//! shows the gallery's components, every group filled, from the first frame.

use automerge::AutoCommit;
use tilolang_runtime::UiNode;
use tilolang_runtime::value::Value;

use crate::fixtures::Replica;

/// The gallery's own sources, as its package is written beside the app.
const GALLERY: &str = concat!(
    include_str!("../../../../apps/tl/gallery/app.tl"),
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
    include_str!("../../../../apps/tl/gallery/shell.tl"),
);

const IDS: &str = include_str!("../../../../apps/tl/gallery/app.ids");

const APP: &str = "so.tilo.lang.gallery";

/// Every word the window shows.
fn words(node: &UiNode, out: &mut Vec<String>) {
    out.extend(node.props.values().filter_map(|value| match value {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        words(child, out);
    }
}

/// How many nodes of `kind` stand under `node`.
fn counted(node: &UiNode, kind: &str) -> usize {
    usize::from(node.kind == kind)
        + node
            .children
            .iter()
            .map(|child| counted(child, kind))
            .sum::<usize>()
}

fn shows(shown: &[String], word: &str) -> bool {
    shown.iter().any(|seen| seen.contains(word))
}

/// The node of kind `kind` whose `key` prop is `key`, under `node`.
fn keyed<'a>(node: &'a UiNode, kind: &str, key: &str) -> Option<&'a UiNode> {
    if node.kind == kind && node.props.get("key") == Some(&Value::text(key)) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| keyed(child, kind, key))
}

/// Open the gallery as its window does on a store that holds nothing: the root is made, its
/// package's migrations run on it, and what they wrote is committed before the view is drawn.
fn window(ids: &str) -> Replica {
    let (mut replica, _packages) = Replica::new("ada", AutoCommit::new(), &[(GALLERY, ids)]);
    replica.host.root_of(APP).expect("the gallery's root");
    replica.commit();
    replica
}

#[test]
fn a_store_with_nothing_in_it_shows_every_group_filled() {
    let mut replica = window(IDS);
    let view = replica.view(APP);
    let mut first = Vec::new();
    words(view.tree(), &mut first);
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
        let shown = counted(page.tree(), kind);
        assert!(
            shown >= count,
            "`{view}`: {what}, {shown} of {kind} stand in the page"
        );
    }
    for (view, word) in [
        ("data", "Readme.md"),
        ("data", "Moved to Done"),
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
        let mut words_on_the_page = Vec::new();
        words(page.tree(), &mut words_on_the_page);
        assert!(
            shows(&words_on_the_page, word),
            "`{view}` shows its `{word}`: {words_on_the_page:?}"
        );
    }
    let _ = &mut replica;
}
