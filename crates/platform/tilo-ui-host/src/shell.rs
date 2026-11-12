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

/// The kind of the node a list's rows are: the toolkit's virtual list, whose groups a title can
/// say the whole of.
fn list_kind() -> String {
    kind("VirtualList")
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
/// shell of its own, a row standing in a page).
#[must_use]
pub fn as_shell(tree: &UiNode) -> UiNode {
    if let Some(shell) = shell_of(tree) {
        return shell.clone();
    }
    let mut shell = UiNode::empty(&shell_kind(), &format!("{}~shell", tree.key));
    let content = if row(tree) {
        framed(tree, &shell.key)
    } else {
        tree.clone()
    };
    shell
        .children
        .push(slot("content", &shell.key, vec![content]));
    shell
}

/// Whether `tree` is one row of a list: a row view's body, or a view of one item drawn as a row.
fn row(tree: &UiNode) -> bool {
    [
        kind("ListRow"),
        kind("TreeViewRow"),
        kind("SwipeRow"),
        kind("ListItem"),
    ]
    .contains(&tree.kind)
}

/// A row as a window's content: a page from its top, at the readable measure, scrolling under
/// the header. A row is a line: a window that shows one — a result, a notice — shows it where a
/// page holds it. Given the column itself instead, the row is stretched over the whole of it,
/// and its line floats at the column's middle with no padding around it. A view that is not a
/// row keeps the column: a body that fills (a `layers`, a canvas) fills it.
fn framed(tree: &UiNode, owner: &str) -> UiNode {
    let mut stack = UiNode::empty("stack", &format!("{owner}#stack"));
    stack
        .props
        .insert("gap".to_owned(), Value::Text("section".to_owned()));
    stack.children.push(tree.clone());
    let mut page = UiNode::empty("page", &format!("{owner}#page"));
    page.children.push(stack);
    let mut scroll = UiNode::empty(&kind("Scroll"), &format!("{owner}#scroll"));
    scroll.children.push(page);
    scroll
}

fn slot(name: &str, owner: &str, children: Vec<UiNode>) -> UiNode {
    let mut node = UiNode::empty(SLOT, &format!("{owner}#{name}"));
    node.props
        .insert("name".to_owned(), Value::Text(name.to_owned()));
    node.children = children;
    node
}

/// The children of the named slot `name` of a shell node.
pub(crate) fn slot_children<'a>(shell: &'a UiNode, name: &str) -> &'a [UiNode] {
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

/// The prop naming the title of the view a list stands under, and the one naming how many
/// things that title counts: what [`ui::widget::VirtualListProps`] reads, which the shell hands
/// the lists of its content column (`view_title`, `view_count`).
const VIEW_TITLE: &str = "view_title";
const VIEW_COUNT: &str = "view_count";

/// What the window `shell` shows, as the shell's own headers name it: its content column's title
/// — the page, the record, the place the window is at — else its overview's, else its sidebar's,
/// and nothing where it names none of them. A window is named by what it shows, so a desktop
/// names its tab, its switcher entry and its assistive-technology node by this.
#[must_use]
pub fn shown_title(shell: &UiNode) -> Option<&str> {
    ["title", "overview_title", "sidebar_title"]
        .iter()
        .find_map(|prop| match shell.props.get(*prop) {
            Some(Value::Text(title)) => {
                let title = title.trim();
                (!title.is_empty()).then_some(title)
            }
            _ => None,
        })
}

/// The tree with each titled view's title handed to the lists it holds: a list whose rows all
/// stand in one group then heads no group of them when that heading says what the view's title
/// already says — the same words, counting the same rows left to do — since the title over the
/// list says it once.
///
/// A view's title is the one its shell's content header reads (`title:` and `count:`), and it
/// reaches the lists of that column under whatever the app puts between them. A shell's other
/// columns say their own things and hand nothing: an overview lists under its own title.
#[must_use]
pub fn titled_lists(mut node: UiNode) -> UiNode {
    hand_title(&mut node, None);
    node
}

/// What a view's title says: the words its header reads, and how many things it counts beside
/// them.
type Title = (String, Option<i64>);

/// The title the shell `node` reads over its content column.
fn content_title(node: &UiNode) -> Option<Title> {
    let words = match node.props.get("title") {
        Some(Value::Text(title)) if !title.trim().is_empty() => title.clone(),
        _ => return None,
    };
    let count = match node.props.get("count") {
        Some(Value::Int(count)) => Some(*count),
        _ => None,
    };
    Some((words, count))
}

/// Hand `title` down the tree: into a list that stands in it, and into what the tree holds.
fn hand_title(node: &mut UiNode, title: Option<&Title>) {
    let shell = node.kind == shell_kind();
    let here = if shell {
        content_title(node)
    } else {
        title.cloned()
    };
    if let Some((words, count)) = &here
        && node.kind == list_kind()
    {
        node.props
            .insert(VIEW_TITLE.to_owned(), Value::Text(words.clone()));
        match count {
            Some(count) => node.props.insert(VIEW_COUNT.to_owned(), Value::Int(*count)),
            None => node.props.remove(VIEW_COUNT),
        };
    }
    for child in &mut node.children {
        // A shell gives its title to the column it heads, and none to the others.
        let content = !shell
            || (child.kind == SLOT
                && matches!(child.props.get("name"), Some(Value::Text(name)) if name == "content"));
        if content {
            hand_title(child, here.as_ref());
        } else {
            hand_title(child, None);
        }
    }
}

/// Whether `shell` offers the switch between the views the thing it shows declares: the tabs
/// the host puts beside its title.
#[must_use]
pub fn offers_switch(shell: &UiNode) -> bool {
    ["actions", "overview_actions"].into_iter().any(|name| {
        slot_children(shell, name)
            .iter()
            .any(|node| node.kind == kind("ViewTabs"))
    })
}

/// `node` without the rows a virtual list holds: the list reads those as it shows them.
#[must_use]
pub fn without_rows(node: &UiNode) -> UiNode {
    let virtual_list = tilo_render_protocol::patch::is_virtual_list(&node.kind);
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
    // A window after the root's that shows one thing with views of its own — a list it draws,
    // or the switch between the views its type declares — moves the window there: the window's
    // shell is its, with the root's sidebar (the places to go back by), and the root's list and
    // its controls step aside. The window is built anew where it moves to, so its bar is the one
    // what it shows offers, never a merge of the two.
    if has_overview(detail) || offers_switch(detail) {
        let mut moved = detail.clone();
        moved = with_slot(moved, "sidebar", slot_children(root, "sidebar").to_vec());
        for name in ["sidebar_title", "can_go_back", "back", "pane"] {
            match root.props.get(name) {
                Some(value) => {
                    moved.props.insert((*name).to_owned(), value.clone());
                }
                None => {
                    if matches!(name, "sidebar_title") {
                        moved.props.remove(name);
                    }
                }
            }
        }
        if root.events.contains_key(BACK) {
            moved.events.insert(BACK.to_owned(), Event::default());
        }
        return header_rules(moved);
    }
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
    if has_overview(&shell) {
        // The overview's header searches for the window.
        actions.retain(|node| node.kind != kind("SearchButton"));
    }
    let content = slot_children(detail, "content").to_vec();
    let shell = with_slot(shell, "actions", actions);
    with_slot(shell, "content", content)
}

/// Apply the header rules to the composed shell: its bar repeats no action its content or its
/// sidebar offers (an empty sidebar's "New channel"), whichever slot the bar's controls stand in. A title the content shows as its own heading is
/// the shell's business, not the host's: the shell leaves it to the heading and takes it back
/// once that scrolls away.
#[must_use]
pub fn header_rules(mut shell: UiNode) -> UiNode {
    let mut shown: Vec<String> = Vec::new();
    let mut bound: Vec<(String, String)> = Vec::new();
    for slot in ["content", "sidebar"] {
        for node in slot_children(&shell, slot) {
            offered(node, &mut shown, &mut bound);
        }
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
    let kept = |node: &UiNode| -> Option<UiNode> {
        if node.kind == kind("Button") {
            return (!repeated(node, "press")).then(|| node.clone());
        }
        if node.kind == kind("MenuButton") {
            let mut menu = node.clone();
            menu.children
                .retain(|item| item.kind != kind("MenuItem") || !repeated(item, "choose"));
            // The commands the menu holds, wherever the view's calls and loops put them
            // (a loop of commands stands in a fragment), and those it is given as actions
            // of the resource it offers.
            let commands = crate::adapter::description_of(node).is_some_and(|description| {
                crate::adapter::slot_parts(node, description, "items")
                    .iter()
                    .any(|item| item.kind == kind("MenuItem") && !repeated(item, "choose"))
            }) || matches!(
                node.props.get("actions"),
                Some(Value::List(items)) if !items.is_empty()
            );
            return commands.then_some(menu);
        }
        Some(node.clone())
    };
    for slot in ["actions", "overview_actions"] {
        let children = slot_children(&shell, slot);
        let actions: Vec<UiNode> = children.iter().filter_map(&kept).collect();
        shell = with_slot(shell, slot, actions);
    }
    shell
}

/// What `node`'s content offers: the labels of the actions it draws itself and the fields its
/// controls are bound to.
fn offered(node: &UiNode, shown: &mut Vec<String>, bound: &mut Vec<(String, String)>) {
    if node.kind == kind("Button")
        && node.events.contains_key("press")
        && let Some(Value::Text(label)) = node.props.get("label")
    {
        shown.push(label.clone());
    }
    // An empty state's one call to action is drawn by the widget itself, so its label is the
    // state's, not a button's.
    if node.kind == kind("EmptyState")
        && let Some(Value::Text(label)) = node.props.get("action")
    {
        shown.push(label.clone());
    }
    if let Some((target, field)) = node.binding.as_ref().and_then(|binding| binding.field()) {
        bound.push((target.to_owned(), field.to_owned()));
    }
    for child in &node.children {
        offered(child, shown, bound);
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

    /// The node keyed `key` in the tree under `node`.
    fn find(node: &UiNode, key: &str) -> Option<UiNode> {
        if node.key == key {
            return Some(node.clone());
        }
        node.children.iter().find_map(|child| find(child, key))
    }

    /// A node holding others, as an app's view does around its list.
    fn stack(key: &str, children: Vec<UiNode>) -> UiNode {
        let mut stack = UiNode::empty("stack", key);
        stack.children = children;
        stack
    }

    #[test]
    fn a_window_s_titles_reach_the_lists_of_its_content_column() {
        let titled = shell(
            "today",
            vec![
                ("overview", vec![node("VirtualList", "places", &[])]),
                (
                    "content",
                    vec![stack("column", vec![node("VirtualList", "rows", &[])])],
                ),
            ],
            &[("title", Value::text("Today")), ("count", Value::Int(2))],
        );
        let handed = titled_lists(titled);
        let list =
            |key: &str| -> UiNode { find(&handed, key).expect("the list stands where it was") };
        let rows = list("rows");
        assert_eq!(
            rows.props.get("view_title"),
            Some(&Value::text("Today")),
            "the content list hears the title over it"
        );
        assert_eq!(rows.props.get("view_count"), Some(&Value::Int(2)));
        assert!(
            !list("places").props.contains_key("view_title"),
            "the overview lists under its own title"
        );
    }

    #[test]
    fn a_window_with_no_title_hands_none_to_its_lists() {
        let untitled = shell(
            "places",
            vec![("content", vec![node("VirtualList", "rows", &[])])],
            &[],
        );
        let handed = titled_lists(untitled);
        assert!(
            !slot_children(&handed, "content")[0]
                .props
                .contains_key("view_title"),
            "no title stands over the list: its headings stand as they say"
        );
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
    fn a_window_after_the_roots_with_a_list_of_its_own_moves_the_window_there() {
        let root = shell(
            "home",
            vec![
                ("overview", vec![node("VirtualList", "inbox", &[])]),
                (
                    "overview_actions",
                    vec![
                        node("SearchButton", "search", &[]),
                        button("new-task", "New task"),
                    ],
                ),
                ("content", vec![node("EmptyState", "nothing", &[])]),
            ],
            &[
                ("overview_title", Value::text("Inbox")),
                ("overview_count", Value::Int(4)),
            ],
        );
        let project = prefixed(
            &shell(
                "project",
                vec![
                    ("overview", vec![node("VirtualList", "tasks", &[])]),
                    (
                        "overview_actions",
                        vec![
                            node("SearchButton", "search-p", &[]),
                            button("add", "New task"),
                            node("ViewTabs", "views", &[]),
                        ],
                    ),
                    ("content", vec![node("EmptyState", "none", &[])]),
                ],
                &[
                    ("overview_title", Value::text("Launch")),
                    ("overview_count", Value::Int(2)),
                    ("content_kind", Value::text("document")),
                ],
            ),
            DETAIL,
        );
        let composed = compose(&root, Some(&project));
        let overview: Vec<&str> = slot_children(&composed, "overview")
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(
            overview,
            ["detail~tasks"],
            "the window moved to the project"
        );
        assert_eq!(
            composed.props.get("overview_title"),
            Some(&Value::text("Launch"))
        );
        assert_eq!(composed.props.get("overview_count"), Some(&Value::Int(2)));
        let actions: Vec<&str> = slot_children(&composed, "overview_actions")
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(
            actions,
            ["detail~search-p", "detail~add", "detail~views"],
            "the project's own bar, its switch with it"
        );
    }

    #[test]
    fn a_header_repeats_no_button_its_content_has() {
        let build = || {
            let mut title = node("TextInput", "t", &[("title", Value::Int(1))]);
            title.binding = None;
            let mut more = node("MenuButton", "more", &[("label", Value::text("More"))]);
            let mut item = node("MenuItem", "add", &[("label", Value::text("Increment"))]);
            item.events.insert("choose".to_owned(), Event::default());
            more.children.push(item);
            shell(
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
            )
        };
        let ruled = header_rules(build());
        let actions: Vec<&str> = slot_children(&ruled, "actions")
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(actions, ["s", "share"], "an emptied More menu goes too");
        assert_eq!(
            ruled.props.get("title"),
            Some(&Value::text("Counter")),
            "the bar keeps the title: the shell hides it while the page's heading shows it"
        );
    }

    #[test]
    fn a_bar_steps_aside_for_the_call_to_action_its_empty_content_offers() {
        // No channels yet: the empty state is where the first one is made, so the bar does not
        // offer the same action twice. Once the state goes, the bar keeps it.
        let empty = |key: &str| node("EmptyState", key, &[("action", Value::text("New channel"))]);
        let bare = shell(
            "channels",
            vec![
                ("actions", vec![button("new", "New channel")]),
                ("content", vec![empty("none")]),
            ],
            &[],
        );
        let ruled = header_rules(bare);
        assert!(
            slot_children(&ruled, "actions").is_empty(),
            "the empty state's own action is the only one"
        );

        // The same rule where the window's controls stand over an overview's column.
        let over = shell(
            "channels",
            vec![
                (
                    "overview_actions",
                    vec![button("new", "New channel"), button("find", "Find")],
                ),
                ("content", vec![empty("none")]),
            ],
            &[],
        );
        let ruled = header_rules(over);
        let actions: Vec<&str> = slot_children(&ruled, "overview_actions")
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(
            actions,
            ["find"],
            "an overview bar keeps only what it alone offers"
        );

        // The same where the sidebar lists nothing and its empty state makes the first one.
        let listing_nothing = shell(
            "channels",
            vec![
                ("actions", vec![button("new", "New channel")]),
                ("sidebar", vec![empty("none")]),
                ("content", vec![node("EmptyState", "unopened", &[])]),
            ],
            &[],
        );
        assert!(
            slot_children(&header_rules(listing_nothing), "actions").is_empty(),
            "the sidebar's empty state holds the one New channel"
        );

        let filled = shell(
            "channels",
            vec![
                ("actions", vec![button("new", "New channel")]),
                ("content", vec![button("channel", "# general")]),
            ],
            &[],
        );
        let ruled = header_rules(filled);
        assert_eq!(
            slot_children(&ruled, "actions").len(),
            1,
            "the bar keeps its primary once the content offers nothing"
        );
    }

    /// The window is named by what it shows: the page its content column heads, else the place its
    /// overview does, else its sidebar's, and by nothing where the shell names none of them.
    #[test]
    fn the_window_is_named_by_the_title_its_shell_heads_what_it_shows_with() {
        let content = shell(
            "tasks",
            vec![("content", vec![])],
            &[
                ("sidebar_title", Value::text("Tasks")),
                ("title", Value::text("Today")),
            ],
        );
        assert_eq!(shown_title(&content), Some("Today"));

        let overview = shell(
            "notes",
            vec![("overview", vec![])],
            &[
                ("sidebar_title", Value::text("Notes")),
                ("overview_title", Value::text("Q3 plan")),
            ],
        );
        assert_eq!(
            shown_title(&overview),
            Some("Q3 plan"),
            "the column beside the sidebar names what is open in it"
        );

        let sidebar = shell(
            "files",
            vec![("sidebar", vec![])],
            &[("sidebar_title", Value::text("Files"))],
        );
        assert_eq!(shown_title(&sidebar), Some("Files"));

        let unnamed = shell("blank", vec![], &[("title", Value::text("   "))]);
        assert_eq!(shown_title(&unnamed), None, "a blank heading names nothing");
    }

    #[test]
    fn a_row_view_with_no_shell_of_its_own_is_drawn_in_a_page() {
        let row = node(
            "ListRow",
            "summary",
            &[("title", Value::text("Take the bins out"))],
        );
        let shelled = as_shell(&row);
        assert_eq!(shelled.kind, kind("NavShell"), "the window's shell");
        let content = slot_children(&shelled, "content");
        let [scroll] = content else {
            panic!("the shell's content, not {:?}", content.len());
        };
        assert_eq!(
            scroll.kind,
            kind("Scroll"),
            "a page scrolls under the header"
        );
        let [page] = scroll.children.as_slice() else {
            panic!("the scroll holds the page");
        };
        assert_eq!(page.kind, "page", "the page pads it and holds its measure");
        let [stack] = page.children.as_slice() else {
            panic!("the page holds the stack");
        };
        assert_eq!(stack.kind, "stack");
        assert_eq!(
            stack.children.first().map(|node| node.key.as_str()),
            Some("summary"),
            "the view's own tree, whole"
        );
        let filling = node("layers", "desk", &[]);
        let filled = as_shell(&filling);
        let content = slot_children(&filled, "content");
        assert_eq!(
            content.first().map(|node| node.key.as_str()),
            Some("desk"),
            "a body that fills the window keeps the column"
        );
    }
}
