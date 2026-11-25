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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    fn component(name: &str, key: &str) -> UiNode {
        UiNode::empty(&crate::adapter::kind(name), key)
    }

    fn fragment(key: &str, children: Vec<UiNode>) -> UiNode {
        let mut node = UiNode::empty("fragment", key);
        node.children = children;
        node
    }

    fn text(key: &str) -> UiNode {
        let mut node = component("Text", key);
        node.props.insert("value".to_owned(), Value::text(key));
        node
    }

    fn kinds_of(node: &UiNode, out: &mut Vec<String>) {
        out.push(node.kind.clone());
        for child in &node.children {
            kinds_of(child, out);
        }
    }

    #[test]
    fn a_rows_attachments_move_into_its_item_in_order() {
        let mut item = component("ListItem", "m1");
        item.children = vec![fragment(
            "row",
            vec![
                text("said"),
                component("HoverToolbar", "bar"),
                fragment("inner", vec![component("ContextMenu", "menu")]),
                component("Swipe", "swipe"),
            ],
        )];
        let hoisted = hoist(&item);
        let row = &hoisted.children[0];
        let kinds = |node: &UiNode| -> Vec<String> {
            node.children.iter().map(|c| c.kind.clone()).collect()
        };
        assert_eq!(
            kinds(row),
            ["ui.Text", "fragment"],
            "the row keeps what it draws"
        );
        assert!(row.children[1].children.is_empty());
        let part = hoisted
            .children
            .iter()
            .find(|c| c.kind == PART)
            .expect("the item has its attached part");
        assert_eq!(part.props.get("name"), Some(&Value::text(ATTACHED)));
        assert_eq!(
            kinds(part),
            ["ui.HoverToolbar", "ui.ContextMenu", "ui.Swipe"],
            "in the order the view wrote them"
        );
    }

    // USER REQUIREMENT: a row's view puts its menu and toolbar in the layout it draws, as a
    // message's does (`stack { row { name } toolbar }`), and the list still runs them.
    #[test]
    fn an_attachment_inside_the_rows_layout_moves_into_its_item() {
        let mut item = component("ListItem", "m1");
        let mut stack = UiNode::empty("stack", "content");
        stack.children = vec![
            fragment("voice", vec![component("ContextMenu", "menu")]),
            fragment("head", vec![text("Grace Hopper")]),
            fragment("bar", vec![component("HoverToolbar", "toolbar")]),
            {
                let mut inner = UiNode::empty("row", "actions");
                inner.children = vec![
                    component("Button", "reply"),
                    fragment("more", vec![component("Swipe", "swipe")]),
                ];
                inner
            },
        ];
        item.children = vec![fragment("row", vec![stack])];
        let hoisted = hoist(&item);
        let part = hoisted
            .children
            .iter()
            .find(|c| c.kind == PART)
            .expect("the item has its attached part");
        let kinds: Vec<String> = part.children.iter().map(|c| c.kind.clone()).collect();
        assert_eq!(
            kinds,
            ["ui.ContextMenu", "ui.HoverToolbar", "ui.Swipe"],
            "the row's attachments, wherever its layout puts them"
        );
        let drawn = &hoisted.children[0];
        let mut left = Vec::new();
        kinds_of(drawn, &mut left);
        for gone in ["ui.ContextMenu", "ui.HoverToolbar", "ui.Swipe"] {
            assert!(
                !left.iter().any(|kind| kind == gone),
                "{gone} draws: {left:?}"
            );
        }
        assert!(
            left.iter().any(|kind| kind == "ui.Text")
                && left.iter().any(|kind| kind == "ui.Button"),
            "what the row draws stays: {left:?}"
        );
    }

    #[test]
    fn what_a_component_of_its_own_holds_stays_in_it() {
        let mut card = component("Card", "card");
        card.children = vec![fragment(
            "in",
            vec![component("ContextMenu", "the card's own menu")],
        )];
        let mut item = component("ListItem", "m1");
        item.children = vec![fragment("row", vec![card])];
        let hoisted = hoist(&item);
        assert!(
            !hoisted.children.iter().any(|c| c.kind == PART),
            "a card's menu is the card's, not the row's"
        );
    }

    #[test]
    fn an_item_whose_row_attaches_nothing_is_left_as_it_is() {
        let mut item = component("ListItem", "m1");
        item.children = vec![fragment("row", vec![text("said")])];
        assert_eq!(hoist(&item), item);
    }

    // USER REQUIREMENT: a menu may stand on any layout, not only in a list item's row: the layout
    // takes it, wherever a fragment between them held it.
    #[test]
    fn a_menu_a_layout_holds_stands_at_the_layouts_own_children() {
        let mut stack = UiNode::empty("stack", "content");
        stack.children = vec![fragment(
            "if the page has a menu",
            vec![component("ContextMenu", "menu")],
        )];
        let mut layers = UiNode::empty("layers", "shell");
        layers.children = vec![component("Text", "drawn"), stack];
        let mut tree = layers;
        raise_menus(&mut tree);
        assert_eq!(
            kinds_of_kinds(&tree),
            ["ui.Text", "stack"],
            "the layout keeps its shape, the fragment gone with the menu it held"
        );
        // The innermost layout keeps the menu the view placed in it, and the layout beside takes
        // nothing of it.
        let inside = &tree.children[1];
        assert_eq!(
            kinds_of_kinds(inside),
            [MENU],
            "the stack takes its own menu"
        );
        assert!(
            tree.children[0].children.is_empty(),
            "the text beside it takes nothing"
        );
    }

    #[test]
    fn a_menu_reached_through_a_call_is_the_layout_beside_its_call() {
        // What a view's `if let` around a menu is: a fragment, drawing nothing, holding the menu
        // and no one else.
        let mut call = UiNode::empty("layers", "page");
        call.children = vec![
            component("WebView", "page"),
            fragment("menu?", vec![component("ContextMenu", "page menu")]),
        ];
        let mut tree = call.clone();
        raise_menus(&mut tree);
        assert_eq!(
            kinds_of_kinds(&tree),
            ["ui.WebView", "ui.ContextMenu"],
            "the empty fragment goes with the menu it held"
        );
    }

    fn kinds_of_kinds(node: &UiNode) -> Vec<String> {
        node.children.iter().map(|c| c.kind.clone()).collect()
    }
}
