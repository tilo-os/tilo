//! Picking parts of a window from its retained tree: what a person means
//! when they point at "this" to report a problem with it.
//!
//! Answers come from the widgets' own accessibility nodes, the tree
//! assistive technology reads, never from pixels: the deepest node under a
//! point that a person could name, the chain of named nodes around it (so a
//! picker can widen from a word to its row), or the outermost named nodes
//! inside a dragged rectangle. [`capture`] then reads a picked node's texts,
//! each marked with whether it is the app's own fixed copy
//! ([`AccessNode::interface_name`](crate::a11y::AccessNode::interface_name))
//! or something a person or a resource put there.
//!
//! Paths are the inspection's widget paths (`Root[0]/Column[0]/Text[2]`),
//! with `#i/j` for a widget's synthetic children (a virtual list's rows).
//! Hosts run this wherever the widget tree lives: in the page for the
//! browser desktop, in the app's own process for a native window.

use std::collections::HashMap;

use vello::kurbo::{Point, Rect};

use crate::a11y::{AccessNode, Role};
use crate::widget::Widget;

/// A node a person can pick.
#[derive(Clone, Debug, PartialEq)]
pub struct PickTarget {
    /// The widget path, `#i/j` addressing a synthetic child.
    pub path: String,
    pub role: Role,
    /// Its accessible name, when it has one.
    pub name: Option<String>,
    /// Whether that name is the app's own fixed, localized copy.
    pub interface: bool,
    /// Where it is, in the window's logical coordinates.
    pub rect: Rect,
}

impl PickTarget {
    /// The role as assistive technology names it (`Button`, `ListItem`).
    #[must_use]
    pub fn role_name(&self) -> String {
        role_name(self.role)
    }
}

/// The role `role` as assistive technology names it.
#[must_use]
pub fn role_name(role: Role) -> String {
    match serde_json::to_value(role) {
        Ok(serde_json::Value::String(role)) => role,
        Ok(serde_json::Value::Object(map)) => map
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "Generic".into()),
        _ => "Generic".into(),
    }
}

/// One text under a picked node.
#[derive(Clone, Debug, PartialEq)]
pub struct PickedText {
    pub text: String,
    /// The app's own fixed copy, rather than content.
    pub interface: bool,
    /// Where it is drawn, when its node has bounds of its own.
    pub rect: Option<Rect>,
}

/// The texts under a picked node, in reading order, its own name first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PickCapture {
    /// The node's role; `None` for a widget that names no semantics of
    /// its own.
    pub role: Option<Role>,
    pub label: Option<PickedText>,
    pub texts: Vec<PickedText>,
}

/// Deepest a walk goes; deeper trees are not a person's interface.
const MAX_DEPTH: usize = 256;
/// Most texts read under one pick.
const MAX_TEXTS: usize = 64;

fn nameable(node: &AccessNode) -> bool {
    !node.state.hidden
        && (node.semantic_key.is_some()
            || node
                .name
                .as_deref()
                .is_some_and(|name| !name.trim().is_empty())
            || node
                .value
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()))
}

fn target(path: String, node: &AccessNode, rect: Rect) -> PickTarget {
    PickTarget {
        path,
        role: node.role,
        name: node.name.clone().filter(|name| !name.trim().is_empty()),
        interface: node.interface_name,
        rect,
    }
}

fn usable(rect: Rect) -> bool {
    rect.width() > 0.0 && rect.height() > 0.0 && rect.x0.is_finite() && rect.y0.is_finite()
}

/// The path of each child of `widget`, as the inspection numbers them.
fn child_paths<'a>(widget: &'a dyn Widget, path: &str) -> Vec<(&'a dyn Widget, String)> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    widget
        .children()
        .into_iter()
        .map(|child| {
            let name = child.name();
            let index = counts.entry(name).or_insert(0);
            let at = *index;
            *index = index.saturating_add(1);
            let path = if path.is_empty() {
                format!("{name}[{at}]")
            } else {
                format!("{path}/{name}[{at}]")
            };
            (child, path)
        })
        .collect()
}

/// A widget's accessibility node with its synthetic children placed, read
/// inside its component's scope.
fn access(widget: &dyn Widget) -> Option<AccessNode> {
    let _entry = crate::bound_dispatch::enter_widget(widget).ok()?;
    widget
        .accessibility()
        .map(|node| crate::a11y::with_synthetic_bounds(node, widget))
}

/// The named nodes under `point`, outermost first, ending with the deepest
/// one: the chain a picker widens along. Of two nodes at one depth the
/// later, painted over the earlier, wins. Empty where nothing nameable is.
#[must_use]
pub fn targets_at(root: &dyn Widget, point: Point) -> Vec<PickTarget> {
    let mut best: Option<(usize, Vec<PickTarget>)> = None;
    let mut chain = Vec::new();
    let root_path = format!("{}[0]", root.name());
    visit_at(root, root_path, point, 0, &mut chain, &mut best);
    best.map(|(_, chain)| chain).unwrap_or_default()
}

fn visit_at(
    widget: &dyn Widget,
    path: String,
    point: Point,
    depth: usize,
    chain: &mut Vec<PickTarget>,
    best: &mut Option<(usize, Vec<PickTarget>)>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let rect = widget.get_rect();
    let node = access(widget);
    if node.as_ref().is_some_and(|node| node.state.hidden) {
        return;
    }
    let mut pushed = false;
    if let Some(node) = &node {
        let bounds = node.bounds.map_or(rect, crate::a11y::AccessBounds::to_rect);
        if nameable(node) && usable(bounds) && bounds.contains(point) {
            chain.push(target(path.clone(), node, bounds));
            pushed = true;
            if best
                .as_ref()
                .is_none_or(|(deepest, _)| chain.len() >= *deepest)
            {
                *best = Some((chain.len(), chain.clone()));
            }
        }
        visit_synthetic_at(node, &path, point, chain, best);
    }
    let clipped = widget
        .child_clip_rect()
        .is_some_and(|clip| !clip.contains(point));
    if !clipped {
        for (child, child_path) in child_paths(widget, &path) {
            visit_at(
                child,
                child_path,
                point,
                depth.saturating_add(1),
                chain,
                best,
            );
        }
    }
    if pushed {
        chain.pop();
    }
}

fn visit_synthetic_at(
    node: &AccessNode,
    path: &str,
    point: Point,
    chain: &mut Vec<PickTarget>,
    best: &mut Option<(usize, Vec<PickTarget>)>,
) {
    fn walk(
        node: &AccessNode,
        path: &str,
        point: Point,
        chain: &mut Vec<PickTarget>,
        best: &mut Option<(usize, Vec<PickTarget>)>,
        depth: usize,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        for (index, child) in node.children.iter().enumerate() {
            if child.state.hidden {
                continue;
            }
            let child_path = if path.contains('#') {
                format!("{path}/{index}")
            } else {
                format!("{path}#{index}")
            };
            let bounds = child.bounds.map(crate::a11y::AccessBounds::to_rect);
            let inside = bounds.is_some_and(|bounds| usable(bounds) && bounds.contains(point));
            if inside && nameable(child) {
                let Some(bounds) = bounds else { continue };
                chain.push(target(child_path.clone(), child, bounds));
                if best
                    .as_ref()
                    .is_none_or(|(deepest, _)| chain.len() >= *deepest)
                {
                    *best = Some((chain.len(), chain.clone()));
                }
                walk(
                    child,
                    &child_path,
                    point,
                    chain,
                    best,
                    depth.saturating_add(1),
                );
                chain.pop();
            } else if bounds.is_none() || inside {
                walk(
                    child,
                    &child_path,
                    point,
                    chain,
                    best,
                    depth.saturating_add(1),
                );
            }
        }
    }
    walk(node, path, point, chain, best, 0);
}

/// The outermost named nodes wholly inside `area`, in reading order: a
/// dragged rectangle around three rows picks the rows, around a whole list
/// the list. At most `limit`.
#[must_use]
pub fn targets_within(root: &dyn Widget, area: Rect, limit: usize) -> Vec<PickTarget> {
    let mut found = Vec::new();
    let root_path = format!("{}[0]", root.name());
    visit_within(root, root_path, area, 0, limit, &mut found);
    found
}

fn visit_within(
    widget: &dyn Widget,
    path: String,
    area: Rect,
    depth: usize,
    limit: usize,
    found: &mut Vec<PickTarget>,
) {
    if depth > MAX_DEPTH || found.len() >= limit {
        return;
    }
    let rect = widget.get_rect();
    if usable(rect) && rect.intersect(area).area() <= 0.0 {
        return;
    }
    let node = access(widget);
    if node.as_ref().is_some_and(|node| node.state.hidden) {
        return;
    }
    if let Some(node) = &node {
        let bounds = node.bounds.map_or(rect, crate::a11y::AccessBounds::to_rect);
        if nameable(node) && usable(bounds) && encloses(area, bounds) {
            found.push(target(path, node, bounds));
            return;
        }
        if within_synthetic(node, &path, area, limit, found) {
            return;
        }
    }
    let clip = widget.child_clip_rect();
    let area = clip.map_or(area, |clip| clip.intersect(area));
    if area.area() <= 0.0 {
        return;
    }
    for (child, child_path) in child_paths(widget, &path) {
        visit_within(
            child,
            child_path,
            area,
            depth.saturating_add(1),
            limit,
            found,
        );
    }
}

/// Whether the synthetic children of `node` gave any target.
fn within_synthetic(
    node: &AccessNode,
    path: &str,
    area: Rect,
    limit: usize,
    found: &mut Vec<PickTarget>,
) -> bool {
    let before = found.len();
    for (index, child) in node.children.iter().enumerate() {
        if found.len() >= limit {
            break;
        }
        if child.state.hidden {
            continue;
        }
        let child_path = if path.contains('#') {
            format!("{path}/{index}")
        } else {
            format!("{path}#{index}")
        };
        match child.bounds.map(crate::a11y::AccessBounds::to_rect) {
            Some(bounds) if nameable(child) && usable(bounds) && encloses(area, bounds) => {
                found.push(target(child_path, child, bounds));
            }
            _ => {
                within_synthetic(child, &child_path, area, limit, found);
            }
        }
    }
    found.len() > before
}

fn encloses(outer: Rect, inner: Rect) -> bool {
    inner.x0 >= outer.x0 && inner.y0 >= outer.y0 && inner.x1 <= outer.x1 && inner.y1 <= outer.y1
}

/// The texts under the node at `path`, its own name first: every name and
/// value below it in reading order, without repeating one its parent
/// already said. `None` when the tree no longer has that node.
#[must_use]
pub fn capture(root: &dyn Widget, path: &str) -> Option<PickCapture> {
    let (widget_path, synthetic) = match path.split_once('#') {
        Some((widget, synthetic)) => (widget, Some(synthetic)),
        None => (path, None),
    };
    let widget = find(root, &format!("{}[0]", root.name()), widget_path, 0)?;
    let node = access(widget);
    let mut capture = PickCapture::default();
    if let Some(indices) = synthetic {
        let mut node = node?;
        for index in indices.split('/') {
            let index: usize = index.parse().ok()?;
            node = node.children.get(index)?.clone();
        }
        capture.role = Some(node.role);
        capture.label = label(&node, node.bounds.map(crate::a11y::AccessBounds::to_rect));
        read_synthetic(&node, &mut capture.texts);
    } else {
        if let Some(node) = &node {
            let rect = node
                .bounds
                .map_or_else(|| widget.get_rect(), crate::a11y::AccessBounds::to_rect);
            capture.role = Some(node.role);
            capture.label = label(node, Some(rect));
            if let Some(value) = value(node, Some(rect)) {
                capture.texts.push(value);
            }
            read_synthetic(node, &mut capture.texts);
        }
        for child in widget.children() {
            read_widget(child, &mut capture.texts, 0);
        }
    }
    let mut seen: Vec<String> = capture
        .label
        .iter()
        .map(|label| label.text.clone())
        .collect();
    capture.texts.retain(|text| {
        let fresh = !seen.contains(&text.text);
        if fresh {
            seen.push(text.text.clone());
        }
        fresh
    });
    capture.texts.truncate(MAX_TEXTS);
    Some(capture)
}

fn find<'a>(
    widget: &'a dyn Widget,
    path: &str,
    wanted: &str,
    depth: usize,
) -> Option<&'a dyn Widget> {
    if path == wanted {
        return Some(widget);
    }
    if depth > MAX_DEPTH || !wanted.starts_with(path) {
        return None;
    }
    child_paths(widget, path)
        .into_iter()
        .find_map(|(child, child_path)| find(child, &child_path, wanted, depth.saturating_add(1)))
}

fn label(node: &AccessNode, rect: Option<Rect>) -> Option<PickedText> {
    node.name
        .as_ref()
        .filter(|name| !name.trim().is_empty())
        .map(|name| PickedText {
            text: name.clone(),
            interface: node.interface_name,
            rect,
        })
}

/// A field's contents are always content; a protected field has none.
fn value(node: &AccessNode, rect: Option<Rect>) -> Option<PickedText> {
    if node.state.protected {
        return None;
    }
    node.value
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| PickedText {
            text: value.clone(),
            interface: false,
            rect,
        })
}

fn read_node(node: &AccessNode, rect: Option<Rect>, texts: &mut Vec<PickedText>) {
    if let Some(name) = label(node, rect) {
        texts.push(name);
    }
    if let Some(value) = value(node, rect) {
        texts.push(value);
    }
}

fn read_synthetic(node: &AccessNode, texts: &mut Vec<PickedText>) {
    for child in &node.children {
        if texts.len() >= MAX_TEXTS {
            return;
        }
        if child.state.hidden {
            continue;
        }
        read_node(
            child,
            child.bounds.map(crate::a11y::AccessBounds::to_rect),
            texts,
        );
        read_synthetic(child, texts);
    }
}

fn read_widget(widget: &dyn Widget, texts: &mut Vec<PickedText>, depth: usize) {
    if depth > MAX_DEPTH || texts.len() >= MAX_TEXTS {
        return;
    }
    let node = access(widget);
    if let Some(node) = &node {
        if node.state.hidden {
            return;
        }
        let rect = node
            .bounds
            .map_or_else(|| widget.get_rect(), crate::a11y::AccessBounds::to_rect);
        read_node(node, usable(rect).then_some(rect), texts);
        read_synthetic(node, texts);
    }
    for child in widget.children() {
        read_widget(child, texts, depth.saturating_add(1));
    }
}

/// Most areas [`sensitive_areas`] lists before it covers the whole part.
const MAX_SENSITIVE: usize = 512;

/// Where a picture of the node at `path` (whose bounds are `whole`) must
/// be masked: every text that is not the app's own interface copy, every
/// image and every protected field under it. Unlike [`capture`] nothing
/// is left out for length: past [`MAX_SENSITIVE`] areas, or where the node
/// is gone, the whole part is masked.
#[must_use]
pub fn sensitive_areas(root: &dyn Widget, path: &str, whole: Rect) -> Vec<Rect> {
    let (widget_path, synthetic) = match path.split_once('#') {
        Some((widget, synthetic)) => (widget, Some(synthetic)),
        None => (path, None),
    };
    let Some(widget) = find(root, &format!("{}[0]", root.name()), widget_path, 0) else {
        return vec![whole];
    };
    let mut areas = Vec::new();
    if let Some(indices) = synthetic {
        let Some(mut node) = access(widget) else {
            return vec![whole];
        };
        for index in indices.split('/') {
            let Some(child) = index
                .parse::<usize>()
                .ok()
                .and_then(|index| node.children.get(index).cloned())
            else {
                return vec![whole];
            };
            node = child;
        }
        sensitive_node(&node, None, whole, &mut areas);
    } else {
        sensitive_widget(widget, whole, &mut areas, 0);
    }
    if areas.len() >= MAX_SENSITIVE {
        return vec![whole];
    }
    areas
}

fn sensitive_node(node: &AccessNode, rect: Option<Rect>, whole: Rect, areas: &mut Vec<Rect>) {
    if areas.len() >= MAX_SENSITIVE || node.state.hidden {
        return;
    }
    let rect = node
        .bounds
        .map(crate::a11y::AccessBounds::to_rect)
        .or(rect)
        .unwrap_or(whole);
    let content = (node
        .name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
        && !node.interface_name)
        || node
            .value
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || node.state.protected
        || node.role == Role::Image;
    if content {
        areas.push(rect);
    }
    for child in &node.children {
        sensitive_node(child, None, rect, areas);
    }
}

fn sensitive_widget(widget: &dyn Widget, whole: Rect, areas: &mut Vec<Rect>, depth: usize) {
    if depth > MAX_DEPTH || areas.len() >= MAX_SENSITIVE {
        return;
    }
    let rect = widget.get_rect();
    if let Some(node) = access(widget) {
        if node.state.hidden {
            return;
        }
        sensitive_node(&node, usable(rect).then_some(rect), whole, areas);
    }
    for child in widget.children() {
        sensitive_widget(child, whole, areas, depth.saturating_add(1));
    }
}

/// What the person last did through controls, newest last: the action
/// and resource of each, in which component instance, and whether it ran.
/// Only names travel; arguments never do. A report about picked parts
/// carries the last few.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentInvocation {
    /// Unix milliseconds.
    pub at: i64,
    pub instance: u64,
    pub resource: String,
    pub action: String,
    pub done: bool,
}

/// How many invocations [`recent_invocations`] keeps.
pub const RECENT_INVOCATIONS: usize = 20;

thread_local! {
    static RECENT: std::cell::RefCell<std::collections::VecDeque<RecentInvocation>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

/// Note an invocation the person made through a control.
pub(crate) fn note_invocation(address: &crate::bound_dispatch::BoundActionAddress, done: bool) {
    let invocation = RecentInvocation {
        at: i64::try_from(crate::time::unix_now_ms()).unwrap_or(i64::MAX),
        instance: address.instance_id,
        resource: address.address.resource.clone(),
        action: address.address.action.clone(),
        done,
    };
    RECENT.with_borrow_mut(|recent| {
        if recent.len() == RECENT_INVOCATIONS {
            recent.pop_front();
        }
        recent.push_back(invocation);
    });
}

/// The invocations noted on this thread, oldest first.
#[must_use]
pub fn recent_invocations() -> Vec<RecentInvocation> {
    RECENT.with_borrow(|recent| recent.iter().cloned().collect())
}

#[cfg(test)]
#[path = "pick_tests.rs"]
mod tests;
