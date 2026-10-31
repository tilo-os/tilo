//! What a view attaches to the element it stands in (`ui.ContextMenu`, `ui.HoverToolbar`,
//! `ui.Swipe`: components whose description says they attach), taken to the element that
//! runs them.
//!
//! A row's view puts them wherever it likes among its content; they draw nothing there. A
//! list item (`ui.ListItem`) holds those of its row: each attachment anywhere in the row's
//! view, through its calls (fragments) and its layout (`stack`, `row`, `grid`, `scroll` and
//! the rest that only arrange children) moves into the item's `attached` slot, so the list
//! runs the row's menu, toolbar and swipe. A component that gives its children a meaning of
//! its own (a card, a section) is a place of its own: what it holds stays in it. This runs on
//! the tree as the app sent it, before the window reads it, so the reconciler and the rows see
//! one shape.

use crate::UiNode;
use crate::adapter::{SLOT, description_of, slot_nodes};

/// The kind of a list item.
const LIST_ITEM: &str = "ui.ListItem";
/// The slot of a list item that holds what its row attached.
const ATTACHED: &str = "attached";

/// The kinds that only arrange their children in a place: an attachment written among them
/// belongs to the element the place is in, so it is taken through them.
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

/// Whether `node` only arranges its children (`stack`, `row`, a view's call).
fn arranges(node: &UiNode) -> bool {
    LAYOUT.contains(&node.kind.as_str())
}

/// Whether `node` attaches to the element it stands in.
#[must_use]
pub fn attaches(node: &UiNode) -> bool {
    description_of(node).is_some_and(|description| description.semantics.attaches)
}

/// `node` with every list item holding what its row attached.
#[must_use]
pub fn hoist(node: &UiNode) -> UiNode {
    let mut out = node.clone();
    hoist_in(&mut out);
    out
}

/// Hoist in place, through the whole tree.
pub fn hoist_in(node: &mut UiNode) {
    if node.kind == LIST_ITEM {
        hoist_item(node);
    }
    for child in &mut node.children {
        hoist_in(child);
    }
}

/// Move what the row of the list item `item` attached into its `attached` slot.
fn hoist_item(item: &mut UiNode) {
    let Some(description) = description_of(item) else {
        return;
    };
    let Some(row_slot) = description.default_slot().map(|slot| slot.name.clone()) else {
        return;
    };
    // Only the item's default slot (its row) is searched; its named slots are its own.
    let in_row = slot_nodes(item, description, &row_slot)
        .iter()
        .any(|child| holds_attachment(child));
    if !in_row {
        return;
    }
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(item.children.len());
    for mut child in std::mem::take(&mut item.children) {
        if child.kind != SLOT && attaches(&child) {
            taken.push(child);
            continue;
        }
        if child.kind != SLOT {
            take_attachments(&mut child, &mut taken);
        }
        kept.push(child);
    }
    item.children = kept;
    let named = item.children.iter_mut().find(|child| {
        child.kind == SLOT
            && matches!(child.props.get("name"), Some(crate::Value::Text(name)) if name == ATTACHED)
    });
    if let Some(slot) = named {
        slot.children.extend(taken);
    } else {
        let mut slot = UiNode::empty(SLOT, &format!("{}/{ATTACHED}", item.key));
        slot.props
            .insert("name".to_owned(), crate::Value::text(ATTACHED));
        slot.children = taken;
        item.children.push(slot);
    }
}

/// Whether `node` is an attachment, or something that only arranges children (a view's call, a
/// loop's row, a stack) holding one anywhere in the arrangement.
fn holds_attachment(node: &UiNode) -> bool {
    attaches(node) || (arranges(node) && node.children.iter().any(holds_attachment))
}

/// Take the attachments anywhere in `node` (through what only arranges children) out of it, in
/// order.
fn take_attachments(node: &mut UiNode, out: &mut Vec<UiNode>) {
    if attaches(node) || !arranges(node) {
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
        let slot = hoisted
            .children
            .iter()
            .find(|c| c.kind == SLOT)
            .expect("the item has its attached slot");
        assert_eq!(slot.props.get("name"), Some(&Value::text(ATTACHED)));
        assert_eq!(
            kinds(slot),
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
        let slot = hoisted
            .children
            .iter()
            .find(|c| c.kind == SLOT)
            .expect("the item has its attached slot");
        let kinds: Vec<String> = slot.children.iter().map(|c| c.kind.clone()).collect();
        assert_eq!(
            kinds,
            ["ui.ContextMenu", "ui.HoverToolbar", "ui.Swipe"],
            "the row's attachments, wherever its layout puts them"
        );
        let drawn = &hoisted.children[0];
        let mut left = Vec::new();
        fn kinds_of(node: &UiNode, out: &mut Vec<String>) {
            out.push(node.kind.clone());
            for child in &node.children {
                kinds_of(child, out);
            }
        }
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
            !hoisted.children.iter().any(|c| c.kind == SLOT),
            "a card's menu is the card's, not the row's"
        );
    }

    #[test]
    fn an_item_whose_row_attaches_nothing_is_left_as_it_is() {
        let mut item = component("ListItem", "m1");
        item.children = vec![fragment("row", vec![text("said")])];
        assert_eq!(hoist(&item), item);
    }
}
