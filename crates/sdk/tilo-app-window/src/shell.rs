//! The window's one shell, composed from its views' shells. Each view builds its own shell
//! (`ui.NavShell`, `std` builds one for every frame). The window shows them as the columns of one:
//! the root's sidebar and overview, and the content, header and kind of the window after it
//! (workspace-model.md). Composing them is the platform's, as is what holds for any shell of a
//! window, whoever built it:
//! - a header never repeats what its column's content offers: an action the content shows as a
//!   button, or toggles with a control bound to the state it toggles;
//! - a header never repeats a title its content edits as its large title;
//! - a window offers search once: beside an overview, the next column's header has none.
//!
//! The window after the root's keeps its own keys in the composed tree, each carrying [`DETAIL`],
//! so what its controls report reaches its view.

use crate::adapter::{PART, kind};
use crate::{Event, UiNode, Value};

/// What the keys of the window after the root's carry in the composed tree.
pub const DETAIL: &str = "detail~";

/// The event of the composed shell that goes back from the window after the root's.
pub const BACK: &str = "back";

/// The prop of a view's shell that says the window opened it as a page of its own: it stands
/// over what the window showed, a list beside it or not, and carries Back.
pub const PAGE: &str = tilo_render_protocol::patch::PAGE;

/// Whether `tree` is a view opened as a page of its own ([`PAGE`]).
#[must_use]
pub fn is_page(tree: &UiNode) -> bool {
    fn marked(node: &UiNode) -> bool {
        node.props.get(PAGE) == Some(&Value::Bool(true)) || node.children.iter().any(marked)
    }
    marked(tree)
}

/// The prop of a composed shell whose window has nothing at all to list, so it shows the app's one
/// state where its columns are ([`nothing_listed`]).
pub const NOTHING_LISTED: &str = "nothing_listed";

fn shell_kind() -> String {
    kind("NavShell")
}

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
        .push(part("content", &shell.key, vec![content]));
    shell
}

fn row(tree: &UiNode) -> bool {
    row_like(&tree.kind)
}

/// Whether a node of `kind` is a row of a list: what a list shows of an item, whose own
/// commands answer while that row is selected rather than for the window.
pub(crate) fn row_like(node_kind: &str) -> bool {
    [
        kind("ListRow"),
        kind("TreeViewRow"),
        kind("SwipeRow"),
        kind("ListItem"),
    ]
    .iter()
    .any(|row| row.as_str() == node_kind)
}

/// A row as a window's content: a page from its top, at the readable measure, scrolling under the
/// header. A view that is not a row keeps the column: a body that fills (a `layers`, a canvas)
/// fills it.
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

fn part(name: &str, owner: &str, children: Vec<UiNode>) -> UiNode {
    let mut node = UiNode::empty(PART, &format!("{owner}#{name}"));
    node.props
        .insert("name".to_owned(), Value::Text(name.to_owned()));
    node.children = children;
    node
}

pub(crate) fn part_children<'a>(shell: &'a UiNode, name: &str) -> &'a [UiNode] {
    shell
        .children
        .iter()
        .find(|child| {
            child.kind == PART
                && matches!(child.props.get("name"), Some(Value::Text(part)) if part == name)
        })
        .map_or(&[], |child| child.children.as_slice())
}

/// The content the shell's views give: their named `content` part where one stands, else the
/// children they gave without a part's name, which are the shell's default part.
fn content_children(shell: &UiNode) -> Vec<UiNode> {
    let named = part_children(shell, "content");
    if named.is_empty() {
        shell
            .children
            .iter()
            .filter(|child| child.kind != PART)
            .cloned()
            .collect()
    } else {
        named.to_vec()
    }
}

fn with_part(mut shell: UiNode, name: &str, children: Vec<UiNode>) -> UiNode {
    let owner = shell.key.clone();
    shell.children.retain(|child| {
        !(child.kind == PART
            && matches!(child.props.get("name"), Some(Value::Text(part)) if part == name))
    });
    if !children.is_empty() {
        shell.children.push(part(name, &owner, children));
    }
    shell
}

#[must_use]
pub fn has_overview(shell: &UiNode) -> bool {
    !part_children(shell, "overview").is_empty()
}

/// The prop naming the title of the view a list is under, and the one naming how many things that
/// title counts: what [`ui::widget::VirtualListProps`] reads, which the shell hands the lists of
/// its content column (`view_title`, `view_count`).
const VIEW_TITLE: &str = "view_title";
const VIEW_COUNT: &str = "view_count";

/// What the window `shell` shows, as its headers name it: its content column's title (the page, the
/// record, the place the window is at), else its overview's, else its sidebar's, and nothing where
/// none of them names one. A desktop names its tab, its switcher entry and its assistive-technology
/// node by this.
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

/// The tree with each titled view's title handed to the lists it holds: a list whose rows all stand
/// in one group then heads no group of them when that heading says what the view's title already
/// says (the same words, counting the same rows left to do), so the title over the list says it
/// once. A view's title is the one its content header reads (`title:` and `count:`), and it reaches
/// the lists of that column under whatever the app puts between them. A shell's other columns say
/// their own things and hand nothing: an overview lists under its own title.
#[must_use]
pub fn titled_lists(mut node: UiNode) -> UiNode {
    hand_title(&mut node, None);
    node
}

pub(crate) type Title = (String, Option<i64>);

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

/// The title [`titled_lists`] hands the node at `path` (keys from `tree`'s children down), as
/// `tree`'s nodes over it hand it down: what a subtree normalized on its own is handed
/// ([`hand_title`]).
pub(crate) fn title_at(tree: &UiNode, path: &[String]) -> Option<Title> {
    let mut node = tree;
    let mut title: Option<Title> = None;
    for key in path {
        let shell = is_shell(node);
        let here = if shell { content_title(node) } else { title };
        let child = node.children.iter().find(|child| child.key == *key)?;
        title = (!shell || is_content_part(child)).then_some(here).flatten();
        node = child;
    }
    title
}

fn is_shell(node: &UiNode) -> bool {
    crate::adapter::component_name(&node.kind) == Some("NavShell")
}

fn is_content_part(child: &UiNode) -> bool {
    child.kind == PART
        && matches!(child.props.get("name"), Some(Value::Text(name)) if name == "content")
}

/// Hand `title` down the tree: into a list that stands in it, and into what the tree holds.
pub(crate) fn hand_title(node: &mut UiNode, title: Option<&Title>) {
    let shell = is_shell(node);
    let here = if shell {
        content_title(node)
    } else {
        title.cloned()
    };
    if let Some((words, count)) = &here
        && crate::adapter::component_name(&node.kind) == Some("VirtualList")
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
        let content = !shell || is_content_part(child);
        if content {
            hand_title(child, here.as_ref());
        } else {
            hand_title(child, None);
        }
    }
}

#[must_use]
pub fn offers_switch(shell: &UiNode) -> bool {
    ["actions", "overview_actions"].into_iter().any(|name| {
        part_children(shell, name)
            .iter()
            .any(|node| node.kind == kind("ViewTabs"))
    })
}

const HEADER_PROPS: &[&str] = &["title", "count", "content_kind"];

/// Whether `nodes` only stand in for content not chosen yet: empty states, or nothing.
fn placeholder(nodes: &[UiNode]) -> bool {
    nodes.iter().all(|node| {
        node.kind == kind("EmptyState") || (node.kind == "fragment" && placeholder(&node.children))
    })
}

/// The window's shell: the root's shell, with the window after it (its shell, keys carrying
/// [`DETAIL`]) as its content column. Beside an overview the next column is the content; in place
/// of the root's content it is pushed, with Back: a shell that can go back carries the event
/// [`BACK`], which the window answers itself. Beside a sidebar, a root whose content only stands in
/// for what the sidebar chooses (a chat's channels over "No channel open") has nothing to go back
/// to: the sidebar is how one goes elsewhere. A window with nothing at all to list shows the app's
/// own state for it once, alone ([`nothing_listed`]).
#[must_use]
pub fn compose(root: &UiNode, detail: Option<&UiNode>) -> UiNode {
    nothing_listed(at_window(root, detail))
}

fn only_states(node: &UiNode) -> bool {
    let wrapper = node.kind == PART
        || [
            "fragment",
            "stack",
            "column",
            "row",
            "grid",
            "page",
            "primary",
            "secondary",
            "spacer",
        ]
        .contains(&node.kind.as_str())
        || node.kind == kind("Scroll");
    node.kind == kind("EmptyState") || (wrapper && node.children.iter().all(only_states))
}

fn first_state(nodes: &[UiNode]) -> Option<&UiNode> {
    fn state(node: &UiNode) -> Option<&UiNode> {
        if node.kind == kind("EmptyState") {
            return Some(node);
        }
        node.children.iter().find_map(state)
    }
    nodes.iter().find_map(state)
}

/// The window with nothing at all to list: one empty state spanning it, where the overview, the
/// content and the inspector stand, and no column drawing a second one beside it. The app says what
/// it has none of where that list would stand (a chat's places over "No channels yet"), and what is
/// where the thing it chooses would show ("No channel open"). While neither holds a list nor a
/// thing (no channel at all, a first run), the window shows the app's words once
/// ([`ui::widget::NavShell::nothing_listed`]), on the screen a window opens on, so a narrow window
/// fronts the same state whichever column stood in front before. The app's words are all it adds:
/// the state's title, its icon and the line under them are the ones it declared. Its bar keeps the
/// actions its columns offered, so the way to make the first thing is there at every width, and the
/// columns come back with the first one.
///
/// A sidebar whose header carries the app's own controls (a notebook's File menu, the marks of the
/// accounts a mailbox reads) is chrome, not a state: folding the window around its state would hide
/// the only way to the commands that fill it. Such a window keeps its columns, each state where its
/// column stands.
#[must_use]
pub fn nothing_listed(mut shell: UiNode) -> UiNode {
    let sidebar_kept = !part_children(&shell, "sidebar").is_empty()
        && !part_children(&shell, "sidebar_actions").is_empty();
    if sidebar_kept {
        return shell;
    }
    // The places' state first, since it says what the list would hold; then the overview's, the
    // list's own; then the content's, which says what would stand there. A view gives its
    // content without a part's name (the shell's default part), so the shell's unnamed children
    // are its content when no named content part stands beside them.
    let mut state = None;
    for name in ["sidebar", "overview", "content"] {
        let children: Vec<UiNode> = if name == "content" {
            content_children(&shell)
        } else {
            part_children(&shell, name).to_vec()
        };
        if !children.iter().all(only_states) {
            return shell;
        }
        if state.is_none() {
            state = first_state(&children).cloned();
        }
    }
    let Some(state) = state else {
        return shell;
    };
    // The bar keeps what the columns offered: the way to make the first thing is there, where the
    // column that held it is gone.
    let offered_actions = part_children(&shell, "actions");
    let bar = if offered_actions.is_empty() {
        part_children(&shell, "overview_actions").to_vec()
    } else {
        offered_actions.to_vec()
    };
    // The state stands where the content stood: a content given without a name goes with it,
    // so the one column never holds the app's state and what it replaced alike.
    shell.children.retain(|child| child.kind == PART);
    shell = with_part(shell, "sidebar", Vec::new());
    shell = with_part(shell, "overview", Vec::new());
    shell = with_part(shell, "actions", bar);
    shell = with_part(shell, "content", vec![state]);
    shell
        .props
        .insert(NOTHING_LISTED.to_owned(), Value::Bool(true));
    // The window opens on its one state: no column keeps a front for a column that is gone.
    shell.props.remove("pane");
    shell
}

/// Whether the window's body holds only an empty state: nothing is chosen or open there, so a
/// one-column window has nothing to show in the body and opens on its sidebar instead.
#[must_use]
pub fn shows_nothing(shell: &UiNode) -> bool {
    let content = part_children(shell, "content");
    !content.is_empty() && placeholder(content)
}

/// The window's primary action as its bar's control: the words it reads by and what pressing it
/// runs.
fn primary_action(shell: &UiNode) -> Option<(String, Event)> {
    ["actions", "overview_actions"]
        .into_iter()
        .flat_map(|part| part_children(shell, part))
        .find_map(|node| {
            let primary = node.kind == kind("Button")
                && node.props.get("role") == Some(&Value::text("primary"));
            let Some(Value::Text(label)) = node.props.get("label") else {
                return None;
            };
            let press = node.events.get("press")?;
            (primary && press.invokes.is_some()).then(|| (label.clone(), press.clone()))
        })
}

/// Let the empty states of `columns` offer the window's primary action: a state that says
/// nothing is there yet is also where the first one is made. A state that names its own action
/// keeps it. At a size that stands the columns beside the body the window offers the action in
/// its bar too; a one-column window's front column has no bar control of the body's, so its
/// state is where the way to fill it stands, with `message` naming it under the state's words.
pub fn offer_primary(shell: &mut UiNode, columns: &[&str], message: Option<&str>) {
    let Some((label, press)) = primary_action(shell) else {
        return;
    };
    for column in columns {
        let Some(part) = shell.children.iter_mut().find(|child| {
            child.kind == PART
                && matches!(
                    child.props.get("name"),
                    Some(Value::Text(name)) if name == column
                )
        }) else {
            continue;
        };
        for node in &mut part.children {
            offer_state(node, &label, &press, message);
        }
    }
}

/// Put `label`, `press` and `message` in the empty state `node` holds, where it names no action
/// yet: the line under the state's words says what the control beside it starts.
fn offer_state(node: &mut UiNode, label: &str, press: &Event, message: Option<&str>) {
    if node.kind == kind("EmptyState") && !node.props.contains_key("action") {
        node.props.insert("action".to_owned(), Value::text(label));
        if let Some(message) = message
            && !node.props.contains_key("message")
        {
            node.props
                .insert("message".to_owned(), Value::text(message));
        }
        node.events.insert("act".to_owned(), press.clone());
        return;
    }
    for child in &mut node.children {
        offer_state(child, label, press, message);
    }
}

fn at_window(root: &UiNode, detail: Option<&UiNode>) -> UiNode {
    let overview = has_overview(root);
    let mut shell = root.clone();
    let chooses =
        !part_children(root, "sidebar").is_empty() && placeholder(part_children(root, "content"));
    // A view opened as a page of its own stands over what the window showed, a list beside it or
    // not, and Back returns to it.
    let page = detail.is_some_and(is_page);
    if (!overview && !chooses) || page {
        // A window of a page or a document goes back from the window after it.
        shell.events.insert(BACK.to_owned(), Event::default());
        shell
            .props
            .insert("can_go_back".to_owned(), Value::Bool(detail.is_some()));
    }
    let Some(detail) = detail else {
        return shell;
    };
    // A window after the root's that shows one thing with views of its own (a list it draws, or the
    // switch between the views its type declares) moves the window there: the window's shell is
    // its, with the root's sidebar (the places to go back by), and the root's list and its controls
    // step aside. The window is built anew where it moves to, so its bar is the one what it shows
    // offers, never a merge of the two.
    if has_overview(detail) || offers_switch(detail) {
        let mut moved = detail.clone();
        moved = with_part(moved, "sidebar", part_children(root, "sidebar").to_vec());
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
        if page {
            // The page moved the window where it stands, and Back returns from it all the same.
            moved.events.insert(BACK.to_owned(), Event::default());
            moved
                .props
                .insert("can_go_back".to_owned(), Value::Bool(true));
        } else if root.events.contains_key(BACK) {
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
    let mut actions: Vec<UiNode> = part_children(detail, "actions").to_vec();
    if has_overview(&shell) {
        // The overview's header searches for the window.
        actions.retain(|node| node.kind != kind("SearchButton"));
    }
    let content = part_children(detail, "content").to_vec();
    let shell = with_part(shell, "actions", actions);
    with_part(shell, "content", content)
}

/// Apply the header rules to the composed shell: its bar repeats no action its content or its
/// sidebar offers (an empty sidebar's "New channel"), whichever part the bar's controls stand in. A
/// title the content shows as its own heading is the shell's business, not the window's: the shell
/// leaves it to the heading and takes it back once that scrolls away.
#[must_use]
pub fn header_rules(mut shell: UiNode) -> UiNode {
    let mut shown: Vec<String> = Vec::new();
    let mut bound: Vec<(String, String)> = Vec::new();
    for part in ["content", "sidebar"] {
        for node in part_children(&shell, part) {
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
            // The commands the menu holds, wherever the view's calls and loops put them (a loop of
            // commands stands in a fragment), and those it is given as actions of the resource it
            // offers.
            let commands = crate::adapter::description_of(node).is_some_and(|description| {
                crate::adapter::part_components(node, description, "items")
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
    for part in ["actions", "overview_actions"] {
        let children = part_children(&shell, part);
        let actions: Vec<UiNode> = children.iter().filter_map(&kept).collect();
        shell = with_part(shell, part, actions);
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

    fn shell(key: &str, parts: Vec<(&str, Vec<UiNode>)>, props: &[(&str, Value)]) -> UiNode {
        let mut shell = node("NavShell", key, props);
        for (name, children) in parts {
            shell.children.push(part(name, key, children));
        }
        shell
    }

    fn find(node: &UiNode, key: &str) -> Option<UiNode> {
        if node.key == key {
            return Some(node.clone());
        }
        node.children.iter().find_map(|child| find(child, key))
    }

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
            !part_children(&handed, "content")[0]
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
        let content: Vec<&str> = part_children(&composed, "content")
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(content, ["detail~body"]);
        let actions: Vec<&str> = part_children(&composed, "actions")
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

    /// A view opened as a page of its own (`open(r, view: v)`) carries Back even beside an
    /// overview: it stands over what the window showed, and Back returns to it.
    #[test]
    fn a_page_of_its_own_goes_back_beside_an_overview() {
        let root = shell(
            "root",
            vec![
                ("overview", vec![node("VirtualList", "list", &[])]),
                ("content", vec![node("EmptyState", "none", &[])]),
            ],
            &[("overview_title", Value::text("All notes"))],
        );
        let page = prefixed(
            &shell(
                "note",
                vec![("content", vec![node("Scroll", "body", &[])])],
                &[("page", Value::Bool(true))],
            ),
            DETAIL,
        );
        let composed = compose(&root, Some(&page));
        assert!(
            composed.events.contains_key(BACK),
            "the page carries Back beside the overview"
        );
        assert_eq!(composed.props.get("can_go_back"), Some(&Value::Bool(true)));
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
        let overview: Vec<&str> = part_children(&composed, "overview")
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
        let actions: Vec<&str> = part_children(&composed, "overview_actions")
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
        let actions: Vec<&str> = part_children(&ruled, "actions")
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
            part_children(&ruled, "actions").is_empty(),
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
        let actions: Vec<&str> = part_children(&ruled, "overview_actions")
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
            part_children(&header_rules(listing_nothing), "actions").is_empty(),
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
            part_children(&ruled, "actions").len(),
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

    fn empty_places() -> (UiNode, UiNode) {
        let empty = |key: &str, title: &str| {
            node(
                "EmptyState",
                key,
                &[
                    ("title", Value::text(title)),
                    ("icon", Value::text("hash")),
                    (
                        "message",
                        Value::text("Channels are where your workspace talks."),
                    ),
                ],
            )
        };
        let places = empty("none", "No channels yet");
        let unopened = empty("unopened", "No channel open");
        (places, unopened)
    }

    #[test]
    fn a_window_with_nothing_at_all_to_list_shows_one_state_alone() {
        let (places, unopened) = empty_places();
        let bare = shell(
            "channels",
            vec![
                ("actions", vec![button("new", "New channel")]),
                ("sidebar", vec![places]),
                ("content", vec![unopened]),
            ],
            &[("sidebar_title", Value::text("Channels"))],
        );
        let composed = compose(&bare, None);
        assert_eq!(
            composed.props.get("nothing_listed"),
            Some(&Value::Bool(true)),
            "the window says it lists nothing"
        );
        assert!(
            part_children(&composed, "sidebar").is_empty(),
            "the places draw no second state"
        );
        assert!(
            find(&composed, "unopened").is_none(),
            "nor what stood where a thing would"
        );
        let content = part_children(&composed, "content");
        assert_eq!(
            content.len(),
            1,
            "one state stands where the columns do: {content:?}"
        );
        assert_eq!(
            content[0].props.get("title"),
            Some(&Value::text("No channels yet")),
            "the words the app declares for what it has none of, once"
        );
        assert_eq!(
            content[0].props.get("message"),
            Some(&Value::text("Channels are where your workspace talks.")),
            "with the line it says the thing is for"
        );
        let actions: Vec<&str> = part_children(&composed, "actions")
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(actions, ["new"], "the bar keeps the way to make the first");
        // The window opens on that one state: no column keeps the front of one that is gone.
        assert!(!composed.props.contains_key("pane"));
    }

    #[test]
    fn a_window_whose_sidebar_holds_the_app_s_commands_keeps_its_columns() {
        // An empty notebook: its places say they list nothing, and the sidebar's header keeps the
        // File menu that imports what would fill it. Folding the window around one state would
        // take that menu, the sidebar's name and the way in away, so the columns stand.
        let (places, unopened) = empty_places();
        let menu = node("MenuButton", "file", &[("label", Value::text("File"))]);
        let notebook = shell(
            "notes",
            vec![
                ("actions", vec![button("new", "New note")]),
                ("sidebar_actions", vec![menu]),
                ("sidebar", vec![places]),
                ("content", vec![unopened.clone()]),
            ],
            &[("sidebar_title", Value::text("Notes"))],
        );
        let composed = compose(&notebook, None);
        assert!(
            !composed.props.contains_key("nothing_listed"),
            "the window keeps its columns"
        );
        assert_eq!(
            part_children(&composed, "sidebar")
                .iter()
                .map(|node| node.key.as_str())
                .collect::<Vec<_>>(),
            ["none"],
            "the places' state stands where the places do"
        );
        assert!(
            find(&composed, "unopened").is_some(),
            "and what stands where a note would stays there"
        );
        assert_eq!(
            part_children(&composed, "sidebar_actions")
                .iter()
                .map(|node| node.key.as_str())
                .collect::<Vec<_>>(),
            ["file"],
            "the File menu stays in the sidebar's header"
        );
        assert_eq!(
            composed.props.get("sidebar_title"),
            Some(&Value::text("Notes")),
            "the sidebar keeps its name, which names the window"
        );
        assert_eq!(
            shown_title(&composed),
            Some("Notes"),
            "and the window is named by it while no note is open"
        );
        assert_eq!(
            part_children(&composed, "actions")
                .iter()
                .map(|node| node.key.as_str())
                .collect::<Vec<_>>(),
            ["new"],
            "the bar still offers the way to start the first note"
        );

        // A header's controls have no row to stand in without the column: the window folds as it
        // did, and its bar keeps what its content offered.
        let headless = shell(
            "notes",
            vec![
                ("actions", vec![button("new", "New note")]),
                ("sidebar_actions", vec![node("MenuButton", "file", &[])]),
                ("content", vec![unopened]),
            ],
            &[],
        );
        let folded = compose(&headless, None);
        assert_eq!(
            folded.props.get("nothing_listed"),
            Some(&Value::Bool(true)),
            "with no places column, one state stands for the window"
        );
    }

    #[test]
    fn a_window_that_lists_something_keeps_its_columns() {
        let (places, unopened) = empty_places();
        // The places hold a place to choose: the window lists it, and what stands where a thing
        // would shows beside it.
        let listing = shell(
            "channels",
            vec![
                ("actions", vec![button("new", "New channel")]),
                ("sidebar", vec![node("NavList", "nav", &[])]),
                ("content", vec![unopened]),
            ],
            &[],
        );
        let composed = compose(&listing, None);
        assert!(!composed.props.contains_key("nothing_listed"));
        assert!(!part_children(&composed, "sidebar").is_empty());
        assert_eq!(part_children(&composed, "content").len(), 1);

        // Nothing to choose from and something chosen: a conversation open in a window whose places
        // say they hold none keeps both.
        let opened = shell(
            "channels",
            vec![
                ("sidebar", vec![places.clone()]),
                (
                    "content",
                    vec![node("Text", "said", &[("text", Value::text("# design"))])],
                ),
            ],
            &[],
        );
        assert!(!compose(&opened, None).props.contains_key("nothing_listed"));

        // The window after the root's brings something to show: both columns stand.
        let root = shell(
            "channels",
            vec![
                ("actions", vec![button("new", "New channel")]),
                ("sidebar", vec![places]),
                ("content", vec![node("EmptyState", "unopened", &[])]),
            ],
            &[],
        );
        let detail = prefixed(
            &shell(
                "conversation",
                vec![(
                    "content",
                    vec![node("Text", "said", &[("text", Value::text("hello"))])],
                )],
                &[("title", Value::text("# design"))],
            ),
            DETAIL,
        );
        let composed = compose(&root, Some(&detail));
        assert!(!composed.props.contains_key("nothing_listed"));
        assert_eq!(
            composed.props.get("title"),
            Some(&Value::text("# design")),
            "the window shows the conversation"
        );
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
        let content = part_children(&shelled, "content");
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
        let filling = node("layers", "shell", &[]);
        let filled = as_shell(&filling);
        let content = part_children(&filled, "content");
        assert_eq!(
            content.first().map(|node| node.key.as_str()),
            Some("shell"),
            "a body that fills the window keeps the column"
        );
    }

    #[test]
    fn a_window_with_nothing_listed_gives_its_content_once() {
        // An app gives its content without a part's name, and its sidebar holds only the empty
        // state of the place: the window folds to that one state, and its content column holds
        // it once, never the state beside what it replaced.
        let mut window = node("NavShell", "window", &[("title", Value::text("Disks"))]);
        window.children.push(part(
            "sidebar",
            "window",
            vec![node("EmptyState", "none", &[])],
        ));
        window.children.push(node("EmptyState", "content", &[]));
        let folded = nothing_listed(window);
        assert!(
            folded.props.contains_key(NOTHING_LISTED),
            "the window folds to its one state"
        );
        let content = content_children(&folded);
        assert_eq!(content.len(), 1, "one content view, never two: {content:?}");
        assert_eq!(
            content[0].key, "none",
            "the sidebar's state stands in content"
        );
    }
}
