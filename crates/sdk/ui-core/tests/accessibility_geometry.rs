//! The accessibility tree as the browser desktop and inspection tools read
//! it (the headless host's inspection snapshot): each control is reported
//! where it is drawn, and what is drawn is there. A node without geometry
//! of its own would be placed over its owner's whole rect, so a pointer sent
//! to its centre misses; an open list whose options are missing cannot be
//! chosen from.
//!
//! Linked into the `it` umbrella test binary (see `tests/it.rs`), which also
//! carries the test-wide lint allowances.

use tilo_ui::State;
use tilo_ui::a11y::{AccessNode, Action, Role};
use tilo_ui::headless::{FrameHarness, InspectionNode};
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Dropdown, EmptyState};

fn find<'a>(node: &'a AccessNode, matches: &dyn Fn(&AccessNode) -> bool) -> Option<&'a AccessNode> {
    if matches(node) {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, matches))
}

fn semantic<'a>(
    node: &'a InspectionNode,
    matches: &dyn Fn(&AccessNode) -> bool,
) -> Option<&'a AccessNode> {
    node.semantics
        .as_ref()
        .and_then(|access| find(access, matches))
        .or_else(|| {
            node.children
                .iter()
                .find_map(|child| semantic(child, matches))
        })
}

fn all<'a>(node: &'a AccessNode, role: Role, out: &mut Vec<&'a AccessNode>) {
    if node.role == role {
        out.push(node);
    }
    for child in &node.children {
        all(child, role, out);
    }
}

#[test]
fn an_empty_states_action_reports_the_rect_it_is_drawn_in() {
    let mut harness = FrameHarness::for_root(
        State::new(),
        EmptyState::new("No snippets yet").with_action("New snippet"),
        (900, 700),
        Theme::dark(),
    );
    harness.relayout();
    let drawn = harness.root::<EmptyState>().action_rect();
    assert!(drawn.area() > 0.0, "the action is drawn");
    let snapshot = harness.inspection();
    let button = semantic(&snapshot.root, &|node| {
        node.role == Role::Button && node.name.as_deref() == Some("New snippet")
    })
    .expect("the action is in the tree");
    assert_eq!(
        button.bounds.map(tilo_ui::a11y::AccessBounds::to_rect),
        Some(drawn),
        "the action reports where it is drawn, not the whole state"
    );
}

#[test]
fn an_open_dropdowns_options_are_in_the_tree_where_they_are_drawn_and_choosable() {
    let options = vec![
        "Plain text".to_owned(),
        "Shell".to_owned(),
        "Rust".to_owned(),
    ];
    let mut harness = FrameHarness::for_root(
        State::new(),
        Dropdown::new(options, "language").with_placeholder("Language"),
        (600, 500),
        Theme::dark(),
    );
    harness.relayout();
    let closed = harness.inspection();
    assert!(
        semantic(&closed.root, &|node| node.role == Role::ListBox).is_none(),
        "a closed dropdown offers no options"
    );

    let combo = harness
        .a11y_node_named("Language")
        .expect("the combobox is in the tree");
    harness.perform_a11y_action(combo, &Action::Expand).unwrap();
    harness.relayout();
    assert!(harness.root::<Dropdown>().is_open());
    let open = harness.inspection();
    let list = semantic(&open.root, &|node| node.role == Role::ListBox)
        .expect("the open list is in the tree");
    let list_rect = list.bounds.expect("the list reports where it is").to_rect();
    let mut found = Vec::new();
    all(list, Role::ListBoxOption, &mut found);
    let names: Vec<_> = found
        .iter()
        .filter_map(|node| node.name.as_deref())
        .collect();
    assert_eq!(names, ["Plain text", "Shell", "Rust"]);
    for option in &found {
        let bounds = option
            .bounds
            .expect("a shown option reports where it is")
            .to_rect();
        assert!(
            bounds.area() > 0.0 && list_rect.contains(bounds.center()),
            "{:?} is drawn inside the list: {bounds:?} in {list_rect:?}",
            option.name
        );
    }

    let shell = harness
        .a11y_node_named("Shell")
        .expect("the option is reachable by name");
    harness
        .perform_a11y_action(shell, &Action::Activate)
        .unwrap();
    harness.relayout();
    let dropdown = harness.root::<Dropdown>();
    assert_eq!(
        dropdown.selected(),
        "Shell",
        "activating an option chooses it"
    );
    assert!(!dropdown.is_open(), "and closes the list");
}

/// The label of the option the focused combobox names as its active
/// descendant, after checking the tree's focus stayed on the combobox.
fn active_option(harness: &FrameHarness) -> Option<String> {
    use tilo_ui::accesskit::Role as AkRole;
    let (_, combo) = harness.a11y_focus().expect("something holds focus");
    assert_eq!(
        combo.role(),
        AkRole::ComboBox,
        "focus stays on the combobox"
    );
    let active = combo.active_descendant()?;
    let nodes = harness.a11y_nodes();
    let (_, option) = nodes
        .iter()
        .find(|(id, _)| *id == active)
        .expect("the active descendant is in the tree");
    assert_eq!(option.role(), AkRole::ListBoxOption);
    Some(option.label().expect("an option has a name").to_owned())
}

#[test]
fn an_open_dropdown_keeps_focus_and_names_its_highlighted_option() {
    let options = vec![
        "Plain text".to_owned(),
        "Shell".to_owned(),
        "Rust".to_owned(),
    ];
    let mut harness = FrameHarness::for_root(
        State::new(),
        Dropdown::new(options.clone(), "language").with_placeholder("Language"),
        (600, 500),
        Theme::dark(),
    );
    harness.relayout();
    harness.tab(false);
    harness.key("Enter");
    harness.relayout();
    assert!(harness.root::<Dropdown>().is_open());

    // The tree's focus stays on the combobox, which names the option the
    // keyboard is on as its active descendant, as a platform list box does.
    let first = active_option(&harness).expect("the open combobox names its highlighted option");
    harness.key("ArrowDown");
    harness.relayout();
    let second = active_option(&harness).expect("and still does after moving");
    assert_ne!(
        first, second,
        "moving the highlight moves the active descendant"
    );
    assert_eq!(
        Some(&second),
        options.get(harness.root::<Dropdown>().highlight_index()),
        "the active descendant is the highlighted option"
    );

    // The built-in screen reader reads the option as the highlight moves.
    let combo = harness
        .focused_access_node()
        .expect("the combobox is focused");
    let spoken = tilo_ui::screen_reader::describe_state(&combo);
    assert!(spoken.starts_with(&second), "{spoken:?}");

    harness.key("Escape");
    harness.relayout();
    assert_eq!(
        active_option(&harness),
        None,
        "a closed list names no option"
    );
}
