//! What a frame shows an inspector, a screen reader and an automated check: the widget and semantic
//! tree, the named targets a caller addresses, and the accessibility issues a driven frame reports.
//! A host builds its [`InspectionSnapshot`] and [`AccessibilityAudit`] through these; the spec's
//! `targets`, `inspect`, `audit` and `expect` outputs are read through them.

use std::collections::{HashMap, HashSet};

use vello::kurbo::Rect;

use crate::a11y::{AccessNode, Role};
use crate::widget::Widget;

/// One node in a developer inspection snapshot; `semantics` is the [`AccessNode`] the exported
/// AccessKit tree was built from, and `children` include virtualized rows and cells that own no
/// retained widget.
#[derive(Debug, serde::Serialize)]
pub struct InspectionNode {
    pub path: String,
    pub widget: String,
    pub test_id: Option<String>,
    /// Whether the retained widget takes part in keyboard focus traversal; kept in the inspection
    /// format so an omitted semantic node cannot hide a control from the accessibility audit.
    pub focusable: bool,
    /// `[x, y, width, height]` in logical window coordinates.
    pub rect: [f64; 4],
    /// Absolute logical clip applied to children, if the widget clips them.
    pub child_clip: Option<[f64; 4]>,
    /// The words this widget shows as its page title, when it is the page's heading: the
    /// same words a shell's bar must not show beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    /// Effective policy and authoring origins, when supplied by the widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<crate::widget::LayoutInfo>,
    /// The key the widget's draws carried in the last frame, while a host keys them
    /// ([`crate::scene_parts::key_widgets`]): a page shown as HTML finds the widget's element by it and
    /// makes it the semantic node's home, so the drawn and the accessible tree are one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_key: Option<String>,
    pub semantics: Option<AccessNode>,
    pub children: Vec<InspectionNode>,
}

impl InspectionNode {
    /// This node and every node under it, in tree order.
    #[must_use]
    pub fn nodes(&self) -> Vec<&Self> {
        let mut out = Vec::new();
        let mut next = vec![self];
        while let Some(node) = next.pop() {
            out.push(node);
            next.extend(node.children.iter().rev());
        }
        out
    }
}

/// A coherent post-layout snapshot of the retained and semantic trees.
#[derive(Debug, serde::Serialize)]
pub struct InspectionSnapshot {
    pub viewport: [u32; 2],
    /// The retained owner of the runner's current keyboard focus; other mounted apps may hold
    /// editor focus of their own. Absence serializes as null: omission means legacy unknown
    /// ownership.
    pub keyboard_focus_owner: Option<String>,
    pub root: InspectionNode,
}

/// One deterministic accessibility-contract violation found in a laid-out retained or synthetic
/// semantic node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccessibilityIssue {
    pub path: String,
    pub code: String,
    pub message: String,
}

/// Current machine-readable accessibility-audit report format.
pub const ACCESSIBILITY_AUDIT_VERSION: u32 = 1;

/// Result of auditing the same semantic tree exported to assistive technology.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccessibilityAudit {
    pub version: u32,
    pub issues: Vec<AccessibilityIssue>,
}

/// Current machine-readable Tilo-native contract report format.
pub const NATIVE_APP_AUDIT_VERSION: u32 = 2;

/// One reason an app cannot yet be certified as fully action-native.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NativeAppIssue {
    pub control: String,
    pub code: String,
    pub message: String,
}

/// Runtime proof over the app's actual laid-out widget and resource trees.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NativeAppAudit {
    pub version: u32,
    pub accessibility: AccessibilityAudit,
    pub issues: Vec<NativeAppIssue>,
}

impl NativeAppAudit {
    #[must_use]
    pub fn is_certified(&self) -> bool {
        self.accessibility.is_clean() && self.issues.is_empty()
    }
}

impl Default for AccessibilityAudit {
    fn default() -> Self {
        Self {
            version: ACCESSIBILITY_AUDIT_VERSION,
            issues: Vec::new(),
        }
    }
}

impl AccessibilityAudit {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }
}

pub(crate) mod patch;
pub use patch::{InspectionPatchStats, write_inspection_patch};

/// What `widget` shows, as a driven frame's inspection reads it: a skipped pass is checked against
/// it ([`crate::component_inputs`]).
#[cfg(debug_assertions)]
pub(crate) fn widget_inspection_json(widget: &dyn Widget) -> String {
    serde_json::to_string(&collect_inspection(widget, "", &mut HashMap::new(), None))
        .unwrap_or_default()
}

/// The node a semantic `target` names: the one whose action target or semantic key is `target` — the
/// latter is the name the accessibility tree keys a node by (a list row an agent chooses).
pub(crate) fn find_semantic_target<'a>(
    node: &'a AccessNode,
    target: &str,
) -> Option<&'a AccessNode> {
    if node.action_target.as_deref() == Some(target) || node.semantic_key.as_deref() == Some(target)
    {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_semantic_target(child, target))
}

pub(crate) fn compare_text(
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

pub(crate) fn compare_contains(
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

pub(crate) fn compare_bool(
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

/// The editable text's selection, as the anchor and focus the widget published for it: absent when
/// nothing published a selection.
pub(crate) fn compare_text_selection(
    mismatches: &mut Vec<String>,
    field: &str,
    expected: Option<(usize, usize)>,
    actual: Option<crate::a11y::AccessibleTextSelection>,
) {
    let Some((anchor, focus)) = expected else {
        return;
    };
    let actual = actual.map(|selection| (selection.anchor, selection.focus));
    if actual != Some((anchor, focus)) {
        mismatches.push(format!(
            "{field} expected {anchor}..{focus}, got {}",
            actual.map_or_else(
                || "none".to_string(),
                |(anchor, focus)| format!("{anchor}..{focus}")
            )
        ));
    }
}

pub(crate) fn compare_optional_bool(
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

pub(super) use crate::a11y::role_name;

pub(crate) fn collect_targets(
    w: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut HashMap<String, Rect>,
) {
    collect_targets_called(w, parent_path, None, sibling_counts, out);
}

/// [`collect_targets`] of a widget its parent calls `called` ([`Widget::child_path_name`]).
fn collect_targets_called(
    w: &dyn Widget,
    parent_path: &str,
    called: Option<String>,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut HashMap<String, Rect>,
) {
    let Ok(_entry) = crate::widget::bound_nodes::enter_widget(w) else {
        return;
    };
    if w.accessibility_hidden() {
        return;
    }
    let (name, path, my_index) = crate::widget::widget_path(w, parent_path, called, sibling_counts);
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
        // A control whose widget was never laid out has no place on the window: what names it
        // then is only the accessibility box synthesized around it, which reads at the origin
        // and lands a press on the window's chrome. Nothing that has no place is a target.
        if w.get_rect().area() > 0.0 {
            out.insert(id.to_string(), rect);
        }
    }
    // Affordances a widget paints into its own rect instead of composing children of their own:
    // a strip's labels, a window's tabs, a launchpad card. They are named here where they stand,
    // so a tool that reads the shell's targets finds the chip it sees; one whose rect is empty is
    // not on screen and is not named. A name a widget already answers to keeps its own place.
    for (name, rect) in w.test_rects() {
        if rect.area() <= 0.0 {
            continue;
        }
        out.entry(format!("{path}/{name}")).or_insert(rect);
        out.entry(name).or_insert(rect);
    }
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for (index, child) in w.children().into_iter().enumerate() {
        collect_targets_called(
            child,
            &path,
            w.child_path_name(index),
            &mut child_counts,
            out,
        );
    }
}

fn control_bounds(node: &crate::a11y::AccessNode, id: &str) -> Option<crate::a11y::AccessBounds> {
    if node.action_target.as_deref() == Some(id) && node.bounds.is_some() {
        return node.bounds;
    }
    node.children
        .iter()
        .find_map(|child| control_bounds(child, id))
}

pub(crate) struct RetainedNamedTarget {
    pub(crate) key: (usize, String),
    pub(crate) path: String,
    pub(crate) bare: Option<String>,
    pub(crate) id: Option<String>,
}

pub(crate) fn collect_named_targets(
    widget: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut Vec<RetainedNamedTarget>,
) {
    collect_named_targets_called(widget, parent_path, None, sibling_counts, out);
}

/// [`collect_named_targets`] of a widget its parent calls `called` ([`Widget::child_path_name`]).
fn collect_named_targets_called(
    widget: &dyn Widget,
    parent_path: &str,
    called: Option<String>,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut Vec<RetainedNamedTarget>,
) {
    let Ok(_entry) = crate::widget::bound_nodes::enter_widget(widget) else {
        return;
    };
    let (name, path, my_index) =
        crate::widget::widget_path(widget, parent_path, called, sibling_counts);
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
    // Every child a walk of the retained tree must see, in the order it lists them: those the
    // widget routes input to, then the ones it hosts without routing — a subtree it does not own
    // and answers the person's events for itself, like the chrome's search card.
    let mut child_counts = HashMap::new();
    for (index, child) in widget.children().into_iter().enumerate() {
        collect_named_targets_called(
            child,
            &path,
            widget.child_path_name(index),
            &mut child_counts,
            out,
        );
    }
    widget.for_each_hosted_child(&mut |child| {
        collect_named_targets_called(child, &path, None, &mut child_counts, out);
    });
}

pub(crate) fn collect_inspection(
    widget: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    slot: Option<(&dyn Widget, usize)>,
) -> InspectionNode {
    collect_inspection_called(widget, parent_path, None, sibling_counts, slot)
}

/// [`collect_inspection`] of a widget its parent calls `called` ([`Widget::child_path_name`]).
fn collect_inspection_called(
    widget: &dyn Widget,
    parent_path: &str,
    called: Option<String>,
    sibling_counts: &mut HashMap<String, usize>,
    slot: Option<(&dyn Widget, usize)>,
) -> InspectionNode {
    let Ok(_entry) = crate::widget::bound_nodes::enter_widget(widget) else {
        return InspectionNode {
            path: parent_path.into(),
            widget: "ClosedComponent".into(),
            test_id: None,
            focusable: false,
            rect: [0.0; 4],
            child_clip: None,
            page_title: None,
            layout: None,
            paint_key: None,
            semantics: None,
            children: vec![],
        };
    };
    let (name, path, _) = crate::widget::widget_path(widget, parent_path, called, sibling_counts);
    let rect = widget.get_rect();
    let mut semantics = widget.accessibility();
    if let Some(node) = &mut semantics {
        crate::a11y_walk::with_slot_words(node, slot.as_ref());
    }
    let children_of = widget.children().len();
    let inherited = (semantics.is_none() && children_of == 1)
        .then_some(slot)
        .flatten();
    let mut child_counts = HashMap::new();
    let mut children = Vec::new();
    for (index, child) in widget.children().into_iter().enumerate() {
        // As the accessibility walk: a widget with no node still names the child it holds
        // the words for.
        let slot = if semantics.is_some() || widget.accessibility_child_name(index).is_some() {
            Some((widget, index))
        } else {
            inherited
        };
        children.push(collect_inspection_called(
            child,
            &path,
            widget.child_path_name(index),
            &mut child_counts,
            slot,
        ));
    }
    widget.for_each_hosted_child(&mut |child| {
        children.push(collect_inspection(child, &path, &mut child_counts, None));
    });
    InspectionNode {
        path,
        widget: name,
        test_id: widget.test_id().map(str::to_owned),
        focusable: widget.is_focusable(),
        rect: [rect.x0, rect.y0, rect.width(), rect.height()],
        child_clip: widget
            .child_clip_rect()
            .map(|r| [r.x0, r.y0, r.width(), r.height()]),
        page_title: widget.own_page_title(),
        layout: widget.layout_info(),
        paint_key: crate::scene_parts::painted_key(widget),
        semantics: semantics.map(|access| crate::a11y::with_synthetic_bounds(access, widget)),
        children,
    }
}

pub(crate) fn audit_inspection_node(node: &InspectionNode, issues: &mut Vec<AccessibilityIssue>) {
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

/// Every semantic control a binding may stand behind: the effect-capable roles, and a focusable list
/// item, which is pressed as a whole (a list row bound through `ListRow::on_view`).
pub(crate) fn effect_control_ids(node: &InspectionNode) -> HashSet<String> {
    effect_control_ids_of(node, &|_| true, true)
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

struct EffectFilter<'a> {
    include: &'a dyn Fn(&AccessNode) -> bool,
    pressable_items: bool,
}

pub(crate) fn inspection_has_semantics(node: &InspectionNode) -> bool {
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
            && matches!(node.role, Role::ListItem | Role::TreeItem | Role::MenuItem)
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
        .filter(|child| child.is_keyed())
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

/// Whether `node` is what `expected` says of it: every mismatched field in one diagnostic.
pub(crate) fn compare_expectation(
    node: &AccessNode,
    expected: &tilo_headless_schema::SemanticExpectation,
) -> Result<(), String> {
    let mut mismatches = Vec::new();
    let actual_role = role_name(node.role);
    if let Some(role) = expected.role.as_deref()
        && role != actual_role
    {
        mismatches.push(format!("role expected {role:?}, got {actual_role:?}"));
    }
    compare_text(
        &mut mismatches,
        "name",
        expected.name.as_deref(),
        node.name.as_deref(),
    );
    compare_contains(
        &mut mismatches,
        "name",
        expected.name_contains.as_deref(),
        node.name.as_deref(),
    );
    compare_text(
        &mut mismatches,
        "value",
        expected.value.as_deref(),
        node.value.as_deref(),
    );
    compare_contains(
        &mut mismatches,
        "value",
        expected.value_contains.as_deref(),
        node.value.as_deref(),
    );
    compare_text_selection(
        &mut mismatches,
        "text selection",
        expected.text_selection,
        node.text_selection,
    );
    compare_bool(
        &mut mismatches,
        "focused",
        expected.focused,
        node.state.focused,
    );
    compare_bool(
        &mut mismatches,
        "selected",
        expected.selected,
        node.state.selected,
    );
    compare_optional_bool(
        &mut mismatches,
        "checked",
        expected.checked,
        node.state.checked,
    );
    compare_optional_bool(
        &mut mismatches,
        "expanded",
        expected.expanded,
        node.state.expanded,
    );
    compare_bool(
        &mut mismatches,
        "enabled",
        expected.enabled,
        !node.state.disabled,
    );
    compare_bool(&mut mismatches, "busy", expected.busy, node.state.busy);
    compare_bool(
        &mut mismatches,
        "invalid",
        expected.invalid,
        node.state.invalid,
    );
    if mismatches.is_empty() {
        Ok(())
    } else {
        let semantic = expected
            .semantic_target
            .as_deref()
            .map_or_else(String::new, |target| format!(" semantic target {target:?}"));
        Err(format!(
            "expectation failed for widget {:?}{semantic}: {}",
            expected.target,
            mismatches.join("; ")
        ))
    }
}
