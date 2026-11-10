//! The page a window draws for the item its list or tree shows: the fields of that page belong
//! to the item, so what the person types into them lands in it.

use std::collections::BTreeSet;

use tilo_render_protocol::patch::{Lane, ResourceRef};

use super::{AppWindow, UiNode, Value, lane_of};

impl AppWindow {
    /// The resource a field of the window's own view belongs to: a field of the page the
    /// window draws beside its list or tree for the item that list selects belongs to that
    /// item (as a page after the window's own does), so what the person types into it lands in
    /// the item even when the lane has already moved on to the next one. Every other field is
    /// the window's own resource.
    pub(super) fn field_target(&self, field: &str) -> Option<ResourceRef> {
        self.field_target_for(field, None)
    }

    /// [`Self::field_target`], for what the person typed after press `after`: the item that
    /// press showed.
    pub(super) fn field_target_for(&self, field: &str, after: Option<u64>) -> Option<ResourceRef> {
        let (lane, _) = lane_of(field);
        if lane == Lane::Root
            && self.page_field(field)
            && let Some(item) = after
                .and_then(|press| self.shows_by_press.get(&press))
                .cloned()
                .or_else(|| self.shown_in_place.clone())
                .or_else(|| self.built.as_ref().and_then(chosen_item_of))
        {
            return Some(item);
        }
        self.target(lane)
    }

    /// Whether `field` is a control of the page the window draws for the item its own view
    /// shows (a tree beside what it shows), rather than of the window's own list or sidebar.
    pub(super) fn page_field(&self, field: &str) -> bool {
        let (lane, local) = lane_of(field);
        if lane != Lane::Root {
            return false;
        }
        let Some(built) = &self.built else {
            return false;
        };
        content_nodes(built).iter().any(|node| {
            let mut keys = BTreeSet::new();
            bound_keys(node, &mut keys);
            keys.contains(&local)
        })
    }

    /// Show the field `key` as the app has it again: what was typed into it went elsewhere.
    pub(super) fn show_bound_again(&mut self, key: &str) {
        let value = self.app_bound.get(key).cloned();
        match &value {
            Some(Value::Text(text)) => {
                self.texts.insert(key.to_owned(), text.clone());
            }
            Some(_) | None => {
                self.texts.remove(key);
            }
        }
        match value {
            Some(value) => {
                self.bound.borrow_mut().insert(key.to_owned(), value);
            }
            None => {
                self.bound.borrow_mut().remove(key);
            }
        }
        self.refresh_bound();
    }
}

/// The nodes a window shell's content slot holds, the page beside its sidebar: the children of
/// the first slot named `content` under `tree`.
pub(super) fn content_nodes<'a>(tree: &'a UiNode) -> Vec<&'a UiNode> {
    fn walk<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) -> bool {
        if node.kind == crate::adapter::SLOT
            && matches!(node.props.get("name"), Some(Value::Text(name)) if name == "content")
        {
            out.extend(node.children.iter());
            return true;
        }
        node.children.iter().any(|child| walk(child, out))
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

/// The item the window's own page stands on: the resource a list of the content's tree says it
/// shows, where the window draws its page in its own view (a tree beside what it shows).
pub(super) fn chosen_item_of(tree: &UiNode) -> Option<ResourceRef> {
    let mut chosen = None;
    chosen_item(tree, &mut chosen);
    chosen.cloned()
}

pub(super) fn chosen_item<'a>(node: &'a UiNode, out: &mut Option<&'a ResourceRef>) -> bool {
    if let Some(Value::List(items)) = node.props.get("selection")
        && let Some(Value::Ref(chosen)) = items.first()
    {
        *out = Some(chosen);
        return true;
    }
    node.children.iter().any(|child| chosen_item(child, out))
}

/// The node of the composed tree `tree` whose own key is `key`.
pub(super) fn node_of_key<'a>(tree: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if tree.key == key {
        return Some(tree);
    }
    tree.children
        .iter()
        .find_map(|child| node_of_key(child, key))
}

/// The keys of the controls `node` holds that take what the person enters, as the keys the
/// window's bound values are kept under.
pub(super) fn bound_keys(node: &UiNode, out: &mut BTreeSet<String>) {
    if node.binding.is_some() {
        out.insert(node.key.clone());
    }
    for child in &node.children {
        bound_keys(child, out);
    }
}
