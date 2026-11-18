//! The gallery app (`apps/tl/gallery`) as a window opens it: a store with nothing in it shows the
//! gallery's components from the first frame, its sidebar listing every group.

use automerge::AutoCommit;
use tilolang_runtime::UiNode;

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
        tilolang_runtime::Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        words(child, out);
    }
}

#[test]
fn a_store_with_nothing_in_it_opens_the_gallery_on_its_components() {
    let (replica, _packages) = Replica::new("ada", AutoCommit::new(), &[(GALLERY, IDS)]);
    let view = replica.view(APP);
    let mut shown = Vec::new();
    words(view.tree(), &mut shown);
    // Its sidebar lists every group, and the page it opens on is the first one's.
    for group in [
        "Everyday parts",
        "Collections",
        "Media and places",
        "The shell",
        "Your own",
    ] {
        assert!(
            shown.iter().any(|word| word == group),
            "the sidebar's `{group}`: {shown:?}"
        );
    }
    assert!(shown.iter().any(|word| word == "Basics"), "{shown:?}");
    assert!(
        shown
            .iter()
            .any(|word| word.contains("Text, rich text and a quote")),
        "the basics' page stands from the first frame: {shown:?}"
    );
}
