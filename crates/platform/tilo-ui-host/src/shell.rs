//! The window's one shell, composed from its windows' shells.
//!
//! Each view of the window builds its own shell (`ui.NavShell`, `std` builds one for every
//! frame). The window shows them as the columns of one: the root's sidebar and overview, and the
//! content, header and kind of the window after it (workspace-model.md).
//! Composing them is the platform's, as is what holds for any shell of a window, whoever built
//! it:
//!
//! - a header never repeats what its column's content offers: an action the content shows as a
//!   button, or toggles with a control bound to the state it toggles;
//! - a header never repeats a title its content edits as its large title;
//! - a window offers search once: beside an overview, the next column's header has none.
//!
//! The window after the root's keeps its own keys in the composed tree, each carrying
//! [`DETAIL`], so what its controls report reaches its view.

use crate::adapter::{SLOT, kind};
use crate::{Event, UiNode, Value};

/// What the keys of the window after the root's carry in the composed tree.
pub const DETAIL: &str = "detail~";

/// The event of the composed shell that goes back from the window after the root's.
pub const BACK: &str = "back";

fn shell_kind() -> String {
    kind("NavShell")
}

/// The shell a view's tree is: its root, or the first shell under the fragments around it.
#[must_use]
pub fn shell_of(tree: &UiNode) -> Option<&UiNode> {
    if tree.kind == shell_kind() {
        return Some(tree);
    }
    if tree.kind == "fragment" {
        return tree.children.iter().find_map(shell_of);
    }
    None
}

/// A view's tree as a shell: its own, or one whose content it is (a view that composes no
/// shell of its own).
#[must_use]
pub fn as_shell(tree: &UiNode) -> UiNode {
    if let Some(shell) = shell_of(tree) {
        return shell.clone();
    }
    let mut shell = UiNode::empty(&shell_kind(), &format!("{}~shell", tree.key));
    shell
        .children
        .push(slot("content", &shell.key, vec![tree.clone()]));
    shell
}

fn slot(name: &str, owner: &str, children: Vec<UiNode>) -> UiNode {
    let mut node = UiNode::empty(SLOT, &format!("{owner}#{name}"));
    node.props
        .insert("name".to_owned(), Value::Text(name.to_owned()));
    node.children = children;
    node
}

/// The children of the named slot `name` of a shell node.
fn slot_children<'a>(shell: &'a UiNode, name: &str) -> &'a [UiNode] {
    shell
        .children
        .iter()
        .find(|child| {
            child.kind == SLOT
                && matches!(child.props.get("name"), Some(Value::Text(slot)) if slot == name)
        })
        .map_or(&[], |child| child.children.as_slice())
}

/// The shell with the named slot `name` holding `children` (none dropped when empty).
fn with_slot(mut shell: UiNode, name: &str, children: Vec<UiNode>) -> UiNode {
    let owner = shell.key.clone();
    shell.children.retain(|child| {
        !(child.kind == SLOT
            && matches!(child.props.get("name"), Some(Value::Text(slot)) if slot == name))
    });
    if !children.is_empty() {
        shell.children.push(slot(name, &owner, children));
    }
    shell
}

/// Whether a shell has an overview column.
#[must_use]
pub fn has_overview(shell: &UiNode) -> bool {
    !slot_children(shell, "overview").is_empty()
}

/// `node` without the rows a virtual list holds: the list reads those as it shows them.
#[must_use]
pub fn without_rows(node: &UiNode) -> UiNode {
    let virtual_list = crate::is_virtual(&node.kind);
    UiNode {
        kind: node.kind.clone(),
        key: node.key.clone(),
        props: node.props.clone(),
        binding: node.binding.clone(),
        events: node.events.clone(),
        anchor: node.anchor.clone(),
        children: if virtual_list {
            node.children
                .iter()
                .filter(|child| child.kind == SLOT)
                .map(without_rows)
                .collect()
        } else {
            node.children.iter().map(without_rows).collect()
        },
    }
}

/// The props a column's header takes from the window shown in it.
const HEADER_PROPS: &[&str] = &["title", "count", "content_kind"];

/// Whether `nodes` only stand in for content not chosen yet: empty states, or nothing.
fn placeholder(nodes: &[UiNode]) -> bool {
    nodes.iter().all(|node| {
        node.kind == kind("EmptyState") || (node.kind == "fragment" && placeholder(&node.children))
    })
}

/// The window's shell: the root's shell, with the window after it (its shell, keys carrying
/// [`DETAIL`]) as its content column. Beside an overview the next column is the content; in
/// place of the root's content it is pushed, with Back: a shell that can go back carries the
/// event [`BACK`], which the window answers itself. Beside a sidebar, a root whose content only
/// stands in for what the sidebar chooses (a chat's channels over "No channel open") has
/// nothing to go back to: the sidebar is how one goes elsewhere.
#[must_use]
pub fn compose(root: &UiNode, detail: Option<&UiNode>) -> UiNode {
    let overview = has_overview(root);
    let mut shell = root.clone();
    let chooses =
        !slot_children(root, "sidebar").is_empty() && placeholder(slot_children(root, "content"));
    if !overview && !chooses {
        // A window of a page or a document goes back from the window after it.
        shell.events.insert(BACK.to_owned(), Event::default());
        shell
            .props
            .insert("can_go_back".to_owned(), Value::Bool(detail.is_some()));
    }
    let Some(detail) = detail else {
        return shell;
    };
    for name in HEADER_PROPS {
        match detail.props.get(*name) {
            Some(value) => {
                shell.props.insert((*name).to_owned(), value.clone());
            }
            None => {
                shell.props.remove(*name);
            }
        }
    }
    let mut actions: Vec<UiNode> = slot_children(detail, "actions").to_vec();
    if overview {
        // The overview's header searches for the window.
        actions.retain(|node| node.kind != kind("SearchButton"));
    }
    let content = slot_children(detail, "content").to_vec();
    let shell = with_slot(shell, "actions", actions);
    with_slot(shell, "content", content)
}

/// Apply the header rules to the content column of the composed shell: its header repeats
/// neither an action its content offers nor a title its content edits.
#[must_use]
pub fn header_rules(mut shell: UiNode) -> UiNode {
    let content = slot_children(&shell, "content").to_vec();
    let mut shown: Vec<String> = Vec::new();
    let mut bound: Vec<(String, String)> = Vec::new();
    let mut titled = false;
    for node in &content {
        offered(node, &mut shown, &mut bound, &mut titled);
    }
    if titled {
        shell.props.remove("title");
    }
    let repeated = |node: &UiNode, event: &str| -> bool {
        let label = match node.props.get("label") {
            Some(Value::Text(label)) => label.as_str(),
            _ => "",
        };
        shown.iter().any(|shown| shown == label)
            || node
                .events
                .get(event)
                .and_then(|event| event.toggles.as_ref())
                .is_some_and(|toggled| bound.contains(toggled))
    };
    let actions: Vec<UiNode> = slot_children(&shell, "actions")
        .iter()
        .filter_map(|node| {
            if node.kind == kind("Button") {
                return (!repeated(node, "press")).then(|| node.clone());
            }
            if node.kind == kind("MenuButton") {
                let mut menu = node.clone();
                menu.children
                    .retain(|item| item.kind != kind("MenuItem") || !repeated(item, "choose"));
                let commands = menu
                    .children
                    .iter()
                    .any(|item| item.kind == kind("MenuItem"));
                return commands.then_some(menu);
            }
            Some(node.clone())
        })
        .collect();
    with_slot(shell, "actions", actions)
}

/// What `node`'s content offers: the labels of its buttons that apply an action, the fields
/// its controls are bound to, and whether it edits a title as its large title.
fn offered(
    node: &UiNode,
    shown: &mut Vec<String>,
    bound: &mut Vec<(String, String)>,
    titled: &mut bool,
) {
    if node.kind == kind("Button")
        && node.events.contains_key("press")
        && let Some(Value::Text(label)) = node.props.get("label")
    {
        shown.push(label.clone());
    }
    if let Some((target, field)) = node.binding.as_ref().and_then(|binding| binding.field()) {
        bound.push((target.to_owned(), field.to_owned()));
    }
    if node.kind == kind("TextInput")
        && matches!(
            node.props.get("title"),
            Some(Value::Bool(true) | Value::Int(1))
        )
    {
        *titled = true;
    }
    for child in &node.children {
        offered(child, shown, bound, titled);
    }
}

/// `node` with each key carrying `prefix`: the window after the root's in the composed tree.
#[must_use]
pub fn prefixed(node: &UiNode, prefix: &str) -> UiNode {
    if prefix.is_empty() {
        return node.clone();
    }
    UiNode {
        kind: node.kind.clone(),
        key: format!("{prefix}{}", node.key),
        props: node.props.clone(),
        binding: node.binding.clone(),
        events: node.events.clone(),
        anchor: node.anchor.clone(),
        children: node
            .children
            .iter()
            .map(|child| prefixed(child, prefix))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind_name: &str, key: &str, props: &[(&str, Value)]) -> UiNode {
        let mut node = UiNode::empty(&kind(kind_name), key);
        for (name, value) in props {
            node.props.insert((*name).to_owned(), value.clone());
        }
        node
    }

    fn button(key: &str, label: &str) -> UiNode {
        let mut button = node("Button", key, &[("label", Value::text(label))]);
        button.events.insert("press".to_owned(), Event::default());
        button
    }

    fn shell(key: &str, slots: Vec<(&str, Vec<UiNode>)>, props: &[(&str, Value)]) -> UiNode {
        let mut shell = node("NavShell", key, props);
        for (name, children) in slots {
            shell.children.push(slot(name, key, children));
        }
        shell
    }

    #[test]
    fn the_next_window_is_the_content_beside_an_overview_and_its_header_searches_not() {
        let root = shell(
            "root",
            vec![
                ("overview", vec![node("VirtualList", "list", &[])]),
                ("overview_actions", vec![node("SearchButton", "s", &[])]),
                ("content", vec![node("EmptyState", "none", &[])]),
            ],
            &[("overview_title", Value::text("All notes"))],
        );
        let detail = prefixed(
            &shell(
                "note",
                vec![
                    (
                        "actions",
                        vec![node("SearchButton", "s2", &[]), button("pin", "Pin")],
                    ),
                    ("content", vec![node("Scroll", "body", &[])]),
                ],
                &[
                    ("title", Value::text("Q3 plan")),
                    ("content_kind", Value::text("document")),
                ],
            ),
            DETAIL,
        );
        let composed = compose(&root, Some(&detail));
        assert_eq!(composed.key, "root", "the root's shell stays");
        assert_eq!(composed.props.get("title"), Some(&Value::text("Q3 plan")));
        assert_eq!(
            composed.props.get("overview_title"),
            Some(&Value::text("All notes"))
        );
        let content: Vec<&str> = slot_children(&composed, "content")
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(content, ["detail~body"]);
        let actions: Vec<&str> = slot_children(&composed, "actions")
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(actions, ["detail~pin"], "one search, the overview's");
        assert!(
            !composed.events.contains_key(BACK),
            "beside an overview, the shell goes back by itself"
        );
        // A page's window pushes the next one, with Back.
        let page = shell(
            "page",
            vec![("content", vec![node("Scroll", "c", &[])])],
            &[],
        );
        let pushed = compose(&page, Some(&detail));
        assert!(pushed.events.contains_key(BACK));
        assert_eq!(pushed.props.get("can_go_back"), Some(&Value::Bool(true)));
        let alone = compose(&page, None);
        assert_eq!(alone.props.get("can_go_back"), Some(&Value::Bool(false)));
        // Beside a sidebar over a placeholder, the sidebar chooses: no Back.
        let channels = shell(
            "channels",
            vec![
                ("sidebar", vec![node("NavList", "nav", &[])]),
                ("content", vec![node("EmptyState", "none", &[])]),
            ],
            &[],
        );
        let chosen = compose(&channels, Some(&detail));
        assert!(!chosen.events.contains_key(BACK));
        assert_eq!(chosen.props.get("can_go_back"), None);
    }

    #[test]
    fn a_header_repeats_neither_a_button_nor_a_title_its_content_has() {
        let mut title = node("TextInput", "t", &[("title", Value::Int(1))]);
        title.binding = None;
        let mut more = node("MenuButton", "more", &[("label", Value::text("More"))]);
        let mut item = node("MenuItem", "add", &[("label", Value::text("Increment"))]);
        item.events.insert("choose".to_owned(), Event::default());
        more.children.push(item);
        let page = shell(
            "page",
            vec![
                (
                    "actions",
                    vec![
                        node("SearchButton", "s", &[]),
                        button("dec", "Decrement"),
                        more,
                        button("share", "Share"),
                    ],
                ),
                (
                    "content",
                    vec![UiNode {
                        children: vec![
                            title,
                            button("c-dec", "Decrement"),
                            button("c-inc", "Increment"),
                        ],
                        ..UiNode::empty("stack", "body")
                    }],
                ),
            ],
            &[("title", Value::text("Counter"))],
        );
        let ruled = header_rules(page);
        let actions: Vec<&str> = slot_children(&ruled, "actions")
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(actions, ["s", "share"], "an emptied More menu goes too");
        assert!(
            !ruled.props.contains_key("title"),
            "the content edits its title"
        );
    }
}
