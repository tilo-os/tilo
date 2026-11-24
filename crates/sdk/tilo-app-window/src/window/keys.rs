//! Keys and focus in the window's tree: the level a key addresses, the control a press lands
//! on, and the field that takes the typing a person starts.

use super::{DETAIL, Level, UiNode, Value, Widget};

pub(super) fn rect_of(widget: &dyn Widget, key: &str) -> Option<ui::kurbo::Rect> {
    if widget.test_id() == Some(key) {
        let rect = widget.get_rect();
        return (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| rect_of(child, key))
}

pub(super) fn level_of(key: &str) -> (Level, String) {
    match key.strip_prefix(DETAIL) {
        Some(own) => (Level::Detail, own.to_owned()),
        None => (Level::Root, key.to_owned()),
    }
}

pub(super) fn field_after_enter(tree: &UiNode, key: &str) -> Option<String> {
    fn in_order<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
        out.push(node);
        for child in &node.children {
            in_order(child, out);
        }
    }
    let takes_typing = |node: &UiNode| {
        node.binding.is_some()
            && crate::adapter::description_of(node).is_some_and(|description| {
                description.bindings.iter().any(|binding| {
                    matches!(
                        binding.ty,
                        ui::component::Type::Text | ui::component::Type::RichText
                    )
                })
            })
    };
    let mut nodes = Vec::new();
    in_order(tree, &mut nodes);
    let at = nodes.iter().position(|node| node.key == key)?;
    let field = nodes.get(at)?;
    if !one_line(field) || field.events.contains_key("submit") {
        return None;
    }
    let level = level_of(key).0;
    nodes
        .get(at.saturating_add(1)..)?
        .iter()
        .find(|node| takes_typing(node) && level_of(&node.key).0 == level)
        .map(|node| node.key.clone())
}

pub(super) enum Press {
    Lead(String),
    Commit,
    Run(String, String),
    /// Run the action at this address, as the palette runs it: by its address, with the resource the
    /// window shows as its target and no other argument.
    RunAction(tilo_actions::ActionAddress),
}

/// The node key and event to fire for the control that answers a press, matched the way the toolkit
/// matches every shortcut ([`ui::platform::Shortcut::same_press`]): one key, in one spelling, with
/// the modifiers it names, and a command modifier that is the platform's own. So an app's `key:
/// "Ctrl+Enter"` is Command+Enter on a Mac and `key: "e"` is the unshifted E on every layout (a
/// `Button`'s `press`, a `MenuItem`'s `choose`, as the action's `key:` reaches them through
/// `std.header`). `None` when no control declares it. The control `focused` — the one the keyboard
/// is in — is where the answer starts: the actions of the item the person is on lead, then those of
/// what it is in, out to the window's. With no control to start from, the content after the
/// overview's list leads (the resource the person selected), then the whole shell.
pub(super) fn shortcut_action_where(
    node: &UiNode,
    focused: Option<&str>,
    pressed: &ui::platform::Shortcut,
) -> Option<(String, String)> {
    if let Some(path) = focused.and_then(|key| path_to(node, key)) {
        // Out from the control the keyboard is in: what it declares itself, then what else is
        // beside the way on at each level (the item's own controls, the header of the page it is
        // on), then what the item is in.
        for (at, stands) in path.iter().enumerate().rev() {
            if let Some(found) = declared_action(stands, pressed) {
                return Some(found);
            }
            let on_the_way = path.get(at.saturating_add(1)).map(|next| next.key.as_str());
            for child in &stands.children {
                if Some(child.key.as_str()) == on_the_way {
                    continue;
                }
                if let Some(found) = declared_within(child, pressed) {
                    return Some(found);
                }
            }
        }
    }
    for child in crate::shell::part_children(node, "content") {
        if let Some(found) = declared_within(child, pressed) {
            return Some(found);
        }
    }
    declared_within(node, pressed)
}

fn declared_action(node: &UiNode, pressed: &ui::platform::Shortcut) -> Option<(String, String)> {
    if !declares(node, pressed) {
        return None;
    }
    ["press", "choose"]
        .into_iter()
        .find(|event| node.events.contains_key(*event))
        .map(|event| (node.key.clone(), event.to_owned()))
}

fn declared_within(node: &UiNode, pressed: &ui::platform::Shortcut) -> Option<(String, String)> {
    declared_action(node, pressed).or_else(|| {
        node.children
            .iter()
            .find_map(|child| declared_within(child, pressed))
    })
}

pub(super) fn declares(node: &UiNode, pressed: &ui::platform::Shortcut) -> bool {
    let one = matches!(
        node.props.get("key"),
        Some(Value::Text(key))
            if ui::platform::Shortcut::from(key.as_str()).same_press(pressed)
    );
    one || match node.props.get("keys") {
        Some(Value::List(keys)) => keys.iter().any(|key| match key {
            Value::Text(key) => ui::platform::Shortcut::from(key.as_str()).same_press(pressed),
            _ => false,
        }),
        _ => false,
    }
}

fn path_to<'a>(node: &'a UiNode, key: &str) -> Option<Vec<&'a UiNode>> {
    if node.key == key {
        return Some(vec![node]);
    }
    node.children.iter().find_map(|child| {
        let mut path = path_to(child, key)?;
        path.insert(0, node);
        Some(path)
    })
}

pub(super) fn text_field(node: &UiNode) -> bool {
    node.kind == crate::adapter::kind("TextInput") || node.kind == crate::adapter::kind("RichInput")
}

fn one_line(node: &UiNode) -> bool {
    node.kind == crate::adapter::kind("TextInput")
        && matches!(
            node.props.get("lines"),
            None | Some(Value::Unit | Value::Opt(None))
        )
}

pub(super) fn field_takes_a_press(node: &UiNode, key: &str) -> bool {
    node_at(node, key).is_some_and(|field| one_line(field) && field.events.contains_key("submit"))
}

pub(super) fn node_at<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if node.key == key {
        return Some(node);
    }
    node.children.iter().find_map(|child| node_at(child, key))
}

pub(super) fn takes_typing(widget: &dyn Widget) -> bool {
    (widget.is_focused() && widget.ime_content().is_some())
        || widget.children().into_iter().any(takes_typing)
}

/// The node key of the text field in `node`'s tree that declares the press `pressed`: the entry at
/// the end of a list, where the app's key puts the person instead of making an empty item. It
/// answers before any control: a header or bar button that names the same key leads the person to
/// the entry the app wrote for it.
pub(super) fn focus_field(node: &UiNode, pressed: &ui::platform::Shortcut) -> Option<String> {
    if text_field(node) && declares(node, pressed) {
        return Some(node.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| focus_field(child, pressed))
}

pub(super) fn shortcut_declared(node: &UiNode, key: &str) -> Option<ui::platform::Shortcut> {
    let control = node.kind == crate::adapter::kind("Button")
        || node.kind == crate::adapter::kind("MenuItem");
    if control
        && node.key == key
        && let Some(Value::Text(declared)) = node.props.get("key")
    {
        return Some(ui::platform::Shortcut::from(declared.as_str()));
    }
    node.children
        .iter()
        .find_map(|child| shortcut_declared(child, key))
}
