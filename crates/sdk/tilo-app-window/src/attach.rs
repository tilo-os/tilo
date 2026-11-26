//! What a view attaches to the element it is in (`ui.ContextMenu`, `ui.HoverToolbar`, `ui.Swipe`:
//! components whose description says they attach), taken to the element that runs them. A list item
//! (`ui.ListItem`) takes the attachments anywhere in its row's view, through its calls (fragments)
//! and the layouts that only arrange children, into its `attached` part, so the list runs the row's
//! menu, toolbar and swipe; a card or a section is a place of its own and keeps what it holds. A
//! menu a layout's view placed belongs to the layout, which the window stands in a menu area
//! ([`crate::reconcile`]). This runs on the tree as the app sent it, before the window reads it, so
//! the reconciler and the rows see one shape.

use crate::UiNode;
use crate::adapter::{PART, description_of, part_nodes};

const LIST_ITEM: &str = "ui.ListItem";
const ATTACHED: &str = "attached";

/// The menu a view places on the element it is in.
pub(crate) const MENU: &str = "ui.ContextMenu";

/// The kinds that only arrange their children: an attachment among them belongs to the element the
/// place is in, so it is taken through them.
const LAYOUT: &[&str] = &[
    "stack",
    "column",
    "row",
    "grid",
    "split",
    "page",
    "layers",
    "scroll",
    "primary",
    "secondary",
    "fragment",
];

/// Whether a node of `kind` only arranges its children: a layout, whose view's menu the window
/// stands it in ([`crate::reconcile::Reconciler`]).
#[must_use]
pub(crate) fn arranges(kind: &str) -> bool {
    LAYOUT.contains(&kind)
}

fn arranges_node(node: &UiNode) -> bool {
    arranges(&node.kind)
}

/// Whether the node is the menu a view attaches: it draws nothing where the view puts it.
#[must_use]
pub(crate) fn is_menu(node: &UiNode) -> bool {
    node.kind == MENU
}

/// The menu `node`'s view placed on it: its `ui.ContextMenu` child, which the window stands around
/// the node's layout rather than in the layout.
#[must_use]
pub(crate) fn menu_of(node: &UiNode) -> Option<&UiNode> {
    node.children.iter().find(|child| is_menu(child))
}

#[must_use]
pub fn attaches(node: &UiNode) -> bool {
    description_of(node).is_some_and(|description| description.semantics.attaches)
}

#[must_use]
pub fn hoist(node: &UiNode) -> UiNode {
    let mut out = node.clone();
    hoist_in(&mut out);
    out
}

pub fn hoist_in(node: &mut UiNode) {
    if node.kind == LIST_ITEM {
        hoist_item(node);
    }
    for child in &mut node.children {
        hoist_in(child);
    }
}

/// The menu a layout's view placed stands at the layout's own children, wherever a fragment (which
/// draws nothing) held it: a view's condition around its menu is no second place, so the layout the
/// view gave the menu to takes it, and the window stands it around that layout.
pub fn raise_menus(node: &mut UiNode) {
    for child in &mut node.children {
        raise_menus(child);
    }
    if !arranges_node(node) {
        return;
    }
    let mut kept = Vec::with_capacity(node.children.len());
    let mut taken = Vec::new();
    for mut child in std::mem::take(&mut node.children) {
        if is_menu(&child) {
            taken.push(child);
            continue;
        }
        if child.kind == "fragment" {
            raise_through_fragments(&mut child, &mut taken);
            if child.children.is_empty() {
                continue;
            }
        }
        kept.push(child);
    }
    kept.append(&mut taken);
    node.children = kept;
}

/// Take the menus `fragment` holds, through the fragments in it and no further, into `out`: a
/// layout under it keeps the menu the view placed on it.
fn raise_through_fragments(fragment: &mut UiNode, out: &mut Vec<UiNode>) {
    let mut kept = Vec::with_capacity(fragment.children.len());
    for mut child in std::mem::take(&mut fragment.children) {
        if is_menu(&child) {
            out.push(child);
            continue;
        }
        if child.kind == "fragment" {
            raise_through_fragments(&mut child, out);
            if child.children.is_empty() {
                continue;
            }
        }
        kept.push(child);
    }
    fragment.children = kept;
}

fn hoist_item(item: &mut UiNode) {
    let Some(description) = description_of(item) else {
        return;
    };
    let Some(row_part) = description.default_part().map(|part| part.name.clone()) else {
        return;
    };
    // Only the item's row part is searched, not its named parts.
    let in_row = part_nodes(item, description, &row_part)
        .iter()
        .any(|child| holds_attachment(child));
    if !in_row {
        return;
    }
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(item.children.len());
    for mut child in std::mem::take(&mut item.children) {
        if child.kind != PART && attaches(&child) {
            taken.push(child);
            continue;
        }
        if child.kind != PART {
            take_attachments(&mut child, &mut taken);
        }
        kept.push(child);
    }
    item.children = kept;
    let named = item.children.iter_mut().find(|child| {
        child.kind == PART
            && matches!(child.props.get("name"), Some(crate::Value::Text(name)) if name == ATTACHED)
    });
    if let Some(part) = named {
        part.children.extend(taken);
    } else {
        let mut part = UiNode::empty(PART, &format!("{}/{ATTACHED}", item.key));
        part.props
            .insert("name".to_owned(), crate::Value::text(ATTACHED));
        part.children = taken;
        item.children.push(part);
    }
}

fn holds_attachment(node: &UiNode) -> bool {
    attaches(node) || (arranges_node(node) && node.children.iter().any(holds_attachment))
}

fn take_attachments(node: &mut UiNode, out: &mut Vec<UiNode>) {
    if attaches(node) || !arranges_node(node) {
        return;
    }
    let mut kept = Vec::with_capacity(node.children.len());
    for mut child in std::mem::take(&mut node.children) {
        if attaches(&child) {
            out.push(child);
        } else {
            take_attachments(&mut child, out);
            kept.push(child);
        }
    }
    node.children = kept;
}
