//! What a view attaches to the element it stands in (`ui.ContextMenu`, `ui.HoverToolbar`,
//! `ui.Swipe`: components whose description says they attach), taken to the element that
//! runs them.
//!
//! A row's view puts them wherever it likes among its content; they draw nothing there. A
//! list item (`ui.ListItem`) holds those of its row: each attachment at the top of the row's
//! view (its calls and loops seen through) moves into the item's `attached` slot, so the list
//! runs the row's menu, toolbar and swipe. This runs on the tree as the app sent it, before the
//! window reads it, so the reconciler and the rows see one shape.

use crate::UiNode;
use crate::adapter::{SLOT, description_of, slot_nodes};

/// The kind of a list item.
const LIST_ITEM: &str = "ui.ListItem";
/// The slot of a list item that holds what its row attached.
const ATTACHED: &str = "attached";

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

/// Whether `node` is an attachment, or a fragment (a view's call, a loop's row) holding one at
/// its top.
fn holds_attachment(node: &UiNode) -> bool {
    attaches(node) || (node.kind == "fragment" && node.children.iter().any(holds_attachment))
}

/// Take the attachments at the top of `node` (through fragments) out of it, in order.
fn take_attachments(node: &mut UiNode, out: &mut Vec<UiNode>) {
    if attaches(node) {
        return;
    }
    if node.kind != "fragment" {
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

    #[test]
    fn an_item_whose_row_attaches_nothing_is_left_as_it_is() {
        let mut item = component("ListItem", "m1");
        item.children = vec![fragment("row", vec![text("said")])];
        assert_eq!(hoist(&item), item);
    }
}
