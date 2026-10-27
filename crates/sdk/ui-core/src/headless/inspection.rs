//! What a frame shows an inspector, a screen reader and an automated check:
//! the widget and semantic tree, the named targets a caller addresses, and
//! the accessibility issues a driven frame reports. [`FrameHarness`] builds
//! its [`InspectionSnapshot`](super::InspectionSnapshot) and
//! [`AccessibilityAudit`](super::AccessibilityAudit) through these; the
//! spec's `targets`, `inspect`, `audit` and `expect` outputs are read
//! through them.

use std::collections::{HashMap, HashSet};

use vello::kurbo::Rect;

use crate::a11y::{AccessNode, Role};
use crate::widget::Widget;

use super::{AccessibilityIssue, InspectionNode};

pub(super) fn find_semantic_target<'a>(
    node: &'a AccessNode,
    target: &str,
) -> Option<&'a AccessNode> {
    if node.action_target.as_deref() == Some(target) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_semantic_target(child, target))
}

pub(super) fn compare_text(
    mismatches: &mut Vec<String>,
    field: &str,
    expected: Option<&str>,
    actual: Option<&str>,
) {
    if let Some(expected) = expected
        && actual != Some(expected)
    {
        mismatches.push(format!("{field} expected {expected:?}, got {actual:?}"));
    }
}

pub(super) fn compare_contains(
    mismatches: &mut Vec<String>,
    field: &str,
    expected: Option<&str>,
    actual: Option<&str>,
) {
    if let Some(expected) = expected
        && !actual.is_some_and(|actual| actual.contains(expected))
    {
        mismatches.push(format!(
            "{field} expected to contain {expected:?}, got {actual:?}"
        ));
    }
}

pub(super) fn compare_bool(
    mismatches: &mut Vec<String>,
    field: &str,
    expected: Option<bool>,
    actual: bool,
) {
    if let Some(expected) = expected
        && expected != actual
    {
        mismatches.push(format!("{field} expected {expected}, got {actual}"));
    }
}

pub(super) fn compare_optional_bool(
    mismatches: &mut Vec<String>,
    field: &str,
    expected: Option<bool>,
    actual: Option<bool>,
) {
    if let Some(expected) = expected
        && actual != Some(expected)
    {
        mismatches.push(format!("{field} expected {expected}, got {actual:?}"));
    }
}

impl From<crate::a11y::Role> for String {
    fn from(role: crate::a11y::Role) -> Self {
        role_name(role).to_owned()
    }
}

pub(super) fn role_name(role: crate::a11y::Role) -> &'static str {
    use crate::a11y::Role;
    match role {
        Role::Window => "window",
        Role::Dialog => "dialog",
        Role::Group => "group",
        Role::Generic => "generic",
        Role::Toolbar => "toolbar",
        Role::TabBar => "tab_bar",
        Role::Tab => "tab",
        Role::TabPanel => "tab_panel",
        Role::List => "list",
        Role::ListItem => "list_item",
        Role::Table => "table",
        Role::Row => "row",
        Role::Cell => "cell",
        Role::Tree => "tree",
        Role::TreeItem => "tree_item",
        Role::Menu => "menu",
        Role::MenuItem => "menu_item",
        Role::ListBox => "list_box",
        Role::ListBoxOption => "list_box_option",
        Role::Button => "button",
        Role::ToggleButton => "toggle_button",
        Role::Checkbox => "checkbox",
        Role::RadioButton => "radio_button",
        Role::Switch => "switch",
        Role::Slider => "slider",
        Role::Separator => "separator",
        Role::SpinButton => "spin_button",
        Role::ComboBox => "combo_box",
        Role::TextInput => "text_input",
        Role::MultilineTextInput => "multiline_text_input",
        Role::Link => "link",
        Role::ProgressBar => "progress_bar",
        Role::Spinner => "spinner",
        Role::Tooltip => "tooltip",
        Role::Image => "image",
        Role::Label => "label",
        Role::StaticText => "static_text",
        Role::Heading { .. } => "heading",
        Role::Paragraph => "paragraph",
        Role::Document => "document",
        Role::Terminal => "terminal",
        Role::Canvas => "canvas",
        Role::Unknown => "unknown",
    }
}

pub(super) fn collect_targets(
    w: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut HashMap<String, Rect>,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(w) else {
        return;
    };
    let (name, path, my_index) = next_widget_path(w, parent_path, sibling_counts);
    out.insert(path.clone(), w.get_rect());
    if my_index == 0 {
        let bare = if parent_path.is_empty() {
            name
        } else {
            format!("{parent_path}/{name}")
        };
        out.entry(bare).or_insert_with(|| w.get_rect());
    }
    if let Some(id) = w.test_id() {
        // A widget addressed by a control it holds (an empty state's button
        // inside its panel), or whose own node names narrower bounds, is
        // operated where that control is, as assistive technology would.
        let rect = w
            .accessibility()
            .and_then(|node| {
                let node = crate::a11y::with_synthetic_bounds(node, w);
                control_bounds(&node, id).or(node.bounds)
            })
            .map(crate::a11y::AccessBounds::to_rect)
            .filter(|bounds| bounds.width() > 0.0 && bounds.height() > 0.0)
            .unwrap_or_else(|| w.get_rect());
        out.insert(id.to_string(), rect);
    }
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for child in w.children() {
        collect_targets(child, &path, &mut child_counts, out);
    }
}

/// Where the control `id` names sits in `node`'s tree, if one of its nodes
/// is that control.
fn control_bounds(node: &crate::a11y::AccessNode, id: &str) -> Option<crate::a11y::AccessBounds> {
    if node.action_target.as_deref() == Some(id) && node.bounds.is_some() {
        return node.bounds;
    }
    node.children
        .iter()
        .find_map(|child| control_bounds(child, id))
}

pub(super) struct RetainedNamedTarget {
    pub(super) key: (usize, String),
    pub(super) path: String,
    pub(super) bare: Option<String>,
    pub(super) id: Option<String>,
}

pub(super) fn collect_named_targets(
    widget: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut Vec<RetainedNamedTarget>,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    let (name, path, my_index) = next_widget_path(widget, parent_path, sibling_counts);
    // Include even unregistered wrappers: otherwise their same-name inline
    // child could be mistaken for them by a registration-only uniqueness check.
    let bare = (my_index == 0).then(|| {
        if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}/{name}")
        }
    });
    out.push(RetainedNamedTarget {
        key: (std::ptr::from_ref(widget).cast::<()>() as usize, name),
        path: path.clone(),
        bare,
        id: widget.test_id().map(str::to_owned),
    });
    let mut child_counts = HashMap::new();
    for child in widget.children() {
        collect_named_targets(child, &path, &mut child_counts, out);
    }
}

pub(super) fn collect_inspection(
    widget: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
) -> InspectionNode {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return InspectionNode {
            path: parent_path.into(),
            widget: "ClosedComponent".into(),
            test_id: None,
            focusable: false,
            rect: [0.0; 4],
            child_clip: None,
            layout: None,
            semantics: None,
            children: vec![],
        };
    };
    let (name, path, _) = next_widget_path(widget, parent_path, sibling_counts);
    let rect = widget.get_rect();
    let mut child_counts = HashMap::new();
    let children = widget
        .children()
        .into_iter()
        .map(|child| collect_inspection(child, &path, &mut child_counts))
        .collect();
    InspectionNode {
        path,
        widget: name,
        test_id: widget.test_id().map(str::to_owned),
        focusable: widget.is_focusable(),
        rect: [rect.x0, rect.y0, rect.width(), rect.height()],
        child_clip: widget
            .child_clip_rect()
            .map(|r| [r.x0, r.y0, r.width(), r.height()]),
        layout: widget.layout_info(),
        semantics: widget
            .accessibility()
            .map(|access| crate::a11y::with_synthetic_bounds(access, widget)),
        children,
    }
}

pub(super) fn audit_inspection_node(node: &InspectionNode, issues: &mut Vec<AccessibilityIssue>) {
    if node.focusable && node.semantics.is_none() {
        push_audit_issue(
            issues,
            &node.path,
            "missing-semantics",
            "a keyboard-focusable widget must expose accessibility semantics",
        );
    }
    if let Some(semantics) = &node.semantics {
        audit_access_node(semantics, &node.path, issues);
    }
    for child in &node.children {
        audit_inspection_node(child, issues);
    }
}

/// Every semantic control a binding may stand behind: the effect-capable
/// roles, and a focusable list item, which is pressed as a whole (a list
/// row bound through `ListRow::on_view`).
pub(super) fn effect_control_ids(node: &InspectionNode) -> HashSet<String> {
    effect_control_ids_of(node, &|_| true, true)
}

pub(super) fn effect_control_ids_where(
    node: &InspectionNode,
    include: &dyn Fn(&AccessNode) -> bool,
) -> HashSet<String> {
    effect_control_ids_of(node, include, false)
}

fn effect_control_ids_of(
    node: &InspectionNode,
    include: &dyn Fn(&AccessNode) -> bool,
    pressable_items: bool,
) -> HashSet<String> {
    let mut controls = HashSet::new();
    if let Some(semantics) = &node.semantics {
        collect_effect_access_nodes(
            semantics,
            node.test_id.as_deref(),
            &node.path,
            &EffectFilter {
                include,
                pressable_items,
            },
            &mut controls,
        );
    }
    for child in &node.children {
        controls.extend(effect_control_ids_of(child, include, pressable_items));
    }
    controls
}

/// Which semantic nodes count as controls.
struct EffectFilter<'a> {
    include: &'a dyn Fn(&AccessNode) -> bool,
    /// Count a focusable list or tree item too.
    pressable_items: bool,
}

pub(super) fn inspection_has_semantics(node: &InspectionNode) -> bool {
    node.semantics.is_some() || node.children.iter().any(inspection_has_semantics)
}

fn collect_effect_access_nodes(
    node: &AccessNode,
    retained_id: Option<&str>,
    path: &str,
    filter: &EffectFilter<'_>,
    controls: &mut HashSet<String>,
) {
    let effect = (filter.include)(node)
        && !(node.state.read_only
            && matches!(node.role, Role::TextInput | Role::MultilineTextInput))
        && (matches!(
            node.role,
            Role::Button
                | Role::Tab
                | Role::ToggleButton
                | Role::Checkbox
                | Role::RadioButton
                | Role::Switch
                | Role::ComboBox
                | Role::Slider
                | Role::SpinButton
                | Role::TextInput
                | Role::MultilineTextInput
        ) || (filter.pressable_items
            && matches!(node.role, Role::ListItem | Role::TreeItem)
            && node.state.focusable));
    if effect {
        let identity = node
            .action_target
            .clone()
            .or_else(|| retained_id.map(str::to_owned))
            .or_else(|| node.semantic_key.clone())
            .unwrap_or_else(|| format!("{path}/<unaddressable-{:?}>", node.role));
        controls.insert(identity);
    }
    for (index, child) in node.children.iter().enumerate() {
        collect_effect_access_nodes(
            child,
            None,
            &format!("{path}/semantic[{index}]"),
            filter,
            controls,
        );
    }
}

pub(super) fn audit_access_node(
    node: &AccessNode,
    path: &str,
    issues: &mut Vec<AccessibilityIssue>,
) {
    audit_access_node_contract(node, path, issues);
    audit_access_children(node, path, issues);
}

fn audit_access_node_contract(node: &AccessNode, path: &str, issues: &mut Vec<AccessibilityIssue>) {
    let has_name = node
        .name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty());
    if semantic_role_requires_name(node.role) && !has_name {
        push_audit_issue(
            issues,
            path,
            "missing-name",
            format!(
                "{} requires a non-empty accessible name",
                role_name(node.role)
            ),
        );
    }
    if node.state.focusable && !has_accessible_content_name(node) {
        push_audit_issue(
            issues,
            path,
            "unnamed-focus-target",
            "a keyboard-focusable semantic node requires a non-empty accessible name",
        );
    }
    if node.role == Role::Unknown {
        push_audit_issue(
            issues,
            path,
            "unknown-role",
            "replace Role::Unknown with the semantic role exposed to assistive technology",
        );
    }
    if node.state.protected && node.value.is_some() {
        push_audit_issue(
            issues,
            path,
            "protected-value-exposed",
            "a protected control must not expose its value",
        );
    }
    match (node.pos_in_set, node.set_size) {
        (Some(position), Some(size)) if size == 0 || position == 0 || position > size => {
            push_audit_issue(
                issues,
                path,
                "invalid-set-position",
                format!("pos_in_set {position} must be within 1..={size}"),
            );
        }
        (Some(_), None) | (None, Some(_)) => push_audit_issue(
            issues,
            path,
            "incomplete-set-position",
            "pos_in_set and set_size must be supplied together",
        ),
        _ => {}
    }
    if node.level == Some(0) {
        push_audit_issue(
            issues,
            path,
            "invalid-level",
            "semantic hierarchy levels are one-based",
        );
    }
    if node.action_target.as_deref() == Some("") {
        push_audit_issue(
            issues,
            path,
            "empty-action-target",
            "synthetic action targets must be non-empty stable model identities",
        );
    }
}

fn audit_access_children(node: &AccessNode, path: &str, issues: &mut Vec<AccessibilityIssue>) {
    let keyed = node
        .children
        .iter()
        .filter(|child| child.key.is_some())
        .count();
    if keyed > 0 && keyed < node.children.len() {
        push_audit_issue(
            issues,
            path,
            "mixed-synthetic-keys",
            "synthetic siblings must be either all keyed or all position-addressed",
        );
    }
    let mut keys = HashSet::new();
    let mut targets = HashSet::new();
    for (index, child) in node.children.iter().enumerate() {
        let child_path = semantic_child_path(path, child, index);
        if let Some(key) = child.key
            && !keys.insert(key)
        {
            push_audit_issue(
                issues,
                &child_path,
                "duplicate-synthetic-key",
                format!("synthetic sibling key {key} is not unique"),
            );
        }
        if let Some(target) = child.action_target.as_deref()
            && !target.is_empty()
            && !targets.insert(target)
        {
            push_audit_issue(
                issues,
                &child_path,
                "duplicate-action-target",
                format!("synthetic sibling action target {target:?} is not unique"),
            );
        }
        audit_access_node(child, &child_path, issues);
    }
}

fn has_accessible_content_name(node: &AccessNode) -> bool {
    node.name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
        || node.children.iter().any(has_accessible_content_name)
}

fn semantic_role_requires_name(role: Role) -> bool {
    matches!(
        role,
        Role::Tab
            | Role::MenuItem
            | Role::Button
            | Role::ToggleButton
            | Role::Checkbox
            | Role::RadioButton
            | Role::Switch
            | Role::Slider
            | Role::SpinButton
            | Role::ComboBox
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::Link
            | Role::Image
    )
}

fn semantic_child_path(parent: &str, child: &AccessNode, index: usize) -> String {
    if let Some(target) = child.action_target.as_deref() {
        format!("{parent}/semantic[{target:?}]")
    } else if let Some(key) = child.key {
        format!("{parent}/semantic[key={key}]")
    } else {
        format!("{parent}/semantic[{index}]")
    }
}

fn push_audit_issue(
    issues: &mut Vec<AccessibilityIssue>,
    path: &str,
    code: &str,
    message: impl Into<String>,
) {
    issues.push(AccessibilityIssue {
        path: path.to_owned(),
        code: code.to_owned(),
        message: message.into(),
    });
}

fn next_widget_path(
    widget: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
) -> (String, String, usize) {
    let name = widget.name().to_string();
    let sibling_key = format!("{parent_path}::{name}");
    let index = sibling_counts.entry(sibling_key).or_insert(0);
    let current = *index;
    // Bounded by siblings under one parent; saturating makes that explicit.
    *index = index.saturating_add(1);
    let path = if parent_path.is_empty() {
        format!("{name}[{current}]")
    } else {
        format!("{parent_path}/{name}[{current}]")
    };
    (name, path, current)
}
