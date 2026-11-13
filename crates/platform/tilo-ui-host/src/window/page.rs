//! The page a window draws for the item its list or tree shows: the fields of that page belong to
//! the item, so what the person types into them lands in it, and the window is named by it.

use std::collections::BTreeSet;

use tilo_render_protocol::patch::{self as wire, Lane, ResourceRef};

use super::{AppWindow, UiNode, Value, lane_of};

impl AppWindow {
    /// What the window is named by: the item its own view shows as its page, by the name its row
    /// gives it, else what its shell's headers name. A desktop names the window's tab, its switcher
    /// entry and its assistive-technology node by this.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.page_title
            .as_deref()
            .or_else(|| self.built.as_ref().and_then(crate::shell::shown_title))
    }

    /// The name the row of the item the view shows as its page gives it: its `title`, or a tree
    /// node's `label`.
    pub(super) fn shown_item_title(&self) -> Option<String> {
        let shown = self.built.as_ref().and_then(shown_item_of)?;
        let rows = self.rows.borrow();
        rows.values().find_map(|list| {
            let list = list.borrow();
            list.rendered()
                .iter()
                .find_map(|key| list.row(key).and_then(|row| item_title(row, &shown)))
        })
    }

    /// A lane's page came to the front of a narrow window, showing `shows`' item in place where the
    /// window draws its own page: what the person types into it is that item's. Whether the press
    /// takes the keyboard into the page.
    pub(super) fn fronted(
        &mut self,
        brought: wire::Front,
        shows: Option<(u64, ResourceRef)>,
    ) -> bool {
        if let Some((press, shows)) = shows {
            if press > 0 {
                self.shows_by_press.insert(press, shows.clone());
                // A press made long ago has no typing left on its way.
                self.shows_by_press
                    .retain(|press, _| press.saturating_add(64) > self.fired);
            }
            self.shown_in_place = Some(shows);
        }
        // A person's press takes the keyboard into the page it shows; what a scenario or a search
        // opened leaves the keyboard where the person had it.
        if self.detail.is_none() && brought == wire::Front::Input {
            self.opened = Some(self.fired);
            return true;
        }
        false
    }

    /// The resource a field of the window's own view belongs to: a field of the page the window
    /// draws for the item its list or tree selects belongs to that item, so what the person types
    /// into it lands in the item even when the lane has moved on to the next one. Every other field
    /// is the window's own resource.
    pub(super) fn field_target(&self, field: &str) -> Option<ResourceRef> {
        self.field_target_for(field, None)
    }

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

    /// Whether `field` is a control of the page the window draws for the item its view shows (a
    /// tree beside what it shows), rather than of the window's list or sidebar. A view with no item
    /// beside it is the window's own page: its fields are the window's resource, and what the app
    /// sends for them is what they show.
    pub(super) fn page_field(&self, field: &str) -> bool {
        let (lane, local) = lane_of(field);
        if lane != Lane::Root {
            return false;
        }
        let Some(built) = &self.built else {
            return false;
        };
        if self.shown_in_place.is_none() && chosen_item_of(built).is_none() {
            return false;
        }
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

pub(super) fn content_nodes(tree: &UiNode) -> Vec<&UiNode> {
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

/// The item the window's page is for: the resource a list of the content's tree says it shows,
/// where the window draws its page in its own view (a tree beside what it shows).
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

pub(super) fn shown_item_of(tree: &UiNode) -> Option<String> {
    fn open(node: &UiNode) -> Option<String> {
        if node.kind == crate::adapter::kind("VirtualList") {
            match node.props.get("open") {
                Some(Value::Ref(item)) => return Some(item.id.clone()),
                Some(Value::Text(item)) if !item.is_empty() => return Some(item.clone()),
                _ => {}
            }
        }
        node.children.iter().find_map(open)
    }
    open(tree).or_else(|| chosen_item_of(tree).map(|item| item.id))
}

/// What the node of `tree` for item `item` names it: the first `title` of its row, else a tree
/// node's `label`. What the row opens into and its menu (its slots) name other things.
pub(super) fn item_title(tree: &UiNode, item: &str) -> Option<String> {
    fn named(node: &UiNode) -> Option<String> {
        if node.kind == crate::adapter::SLOT {
            return None;
        }
        ["title", "label"]
            .iter()
            .find_map(|prop| match node.props.get(*prop) {
                Some(Value::Text(text)) if !text.trim().is_empty() => Some(text.trim().to_owned()),
                _ => None,
            })
            .or_else(|| node.children.iter().find_map(named))
    }
    if matches!(tree.props.get("item"), Some(Value::Ref(at)) if at.id == item) {
        return named(tree);
    }
    tree.children
        .iter()
        .find_map(|child| item_title(child, item))
}

pub(super) fn node_of_key<'a>(tree: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if tree.key == key {
        return Some(tree);
    }
    tree.children
        .iter()
        .find_map(|child| node_of_key(child, key))
}

pub(super) fn bound_keys(node: &UiNode, out: &mut BTreeSet<String>) {
    if node.binding.is_some() {
        out.insert(node.key.clone());
    }
    for child in &node.children {
        bound_keys(child, out);
    }
}
