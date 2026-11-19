//! What a view shows, as an agent reads it: the semantic tree a person's screen reader is
//! given, written as an outline of labelled nodes, each with its words, its value, its state
//! and where it stands.
//!
//! This is the same tree assistive technology reads ([`AccessNode`]) and the same widgets the
//! window draws: an outline can never show what the view does not, and a view that changes
//! shows its change in the next outline. A control's `action` names what an agent acts on
//! through the action API, so reading an outline and acting on what it shows are one path.
//!
//! The outline is for one view — an app's window content — never for pixels: a widget whose
//! accessibility node names it is one line; a widget that names none is passed through to
//! its children, which flatten up to the nearest line that carries its own words.

use serde::Serialize;

use crate::a11y::AccessNode;
use crate::a11y_tree::HostedByIndex;
use crate::a11y_tree::walk::{A11yVisitor, Visit, walk_synthetic_children};
use crate::widget::Widget;

/// The most nodes one outline carries: enough for a whole window's page, with the rest
/// counted rather than shown, so an answer stays in an agent's context.
pub const MAX_NODES: usize = 2_000;

/// The most characters one outline's text carries.
pub const MAX_TEXT_CHARS: usize = 16_000;

/// The most characters of one label or value an outline keeps.
const MAX_WORDS: usize = 240;

/// A view's semantic outline: the tree, and the same tree as text.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Outline {
    /// Every line, in reading order, one node each: `<role> "<name>"<value><state><action>`,
    /// indented by depth.
    pub text: String,
    /// The same outline as a tree, for a program that reads it rather than a model.
    pub tree: Vec<OutlineNode>,
    /// How many nodes the text shows.
    pub nodes: usize,
    /// How many nodes it left out because it reached [`MAX_NODES`].
    pub omitted: usize,
}

/// One line of an outline: what a widget shows, and what a person could do with it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineNode {
    /// The role, as semantic declarations name it (`button`, `list_item`).
    pub role: String,
    /// The words that name it, in the person's language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What it holds: a field's contents, a paragraph's words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// What its role says too little about, in the app's words ("Message").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// What is said after its name: an empty state's hint, a dialog's situation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Its state in words: `selected`, `checked`, `expanded`, `disabled`, `focused`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<&'static str>,
    /// `[x, y, width, height]`, logical pixels of the view.
    pub rect: [f64; 4],
    /// What a control does, as a stable identity an agent acts by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// The key the control is activated by, as assistive technology operates it: its
    /// stable identity within the widget that publishes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// The item of its app's collection it shows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<OutlineNode>,
}

impl OutlineNode {
    /// This node and every node under it, in tree order.
    fn count(&self) -> usize {
        1_usize.saturating_add(self.children.iter().map(Self::count).sum::<usize>())
    }
}

/// The outline of what `view` shows: its semantic tree, in reading order.
///
/// It is read by the same walk the screen reader's tree is built by
/// ([`crate::a11y_tree::walk`]): a child widget a list states as one of its own items is that
/// item's line, never a second line beside it, and what a person's screen reader hides the
/// outline leaves out.
#[must_use]
pub fn outline(view: &dyn Widget) -> Outline {
    let mut walk = Walk {
        nodes: 0,
        omitted: 0,
        hidden: 0,
    };
    let mut tree = Vec::new();
    crate::a11y_tree::walk::walk::<true, _>(&mut walk, view, None, &mut tree);
    let mut text = String::new();
    for node in &tree {
        node_text(node, 0, &mut text);
    }
    if walk.omitted > 0 {
        let _ =
            std::fmt::Write::write_fmt(&mut text, format_args!("… {} more nodes", walk.omitted));
    }
    Outline {
        text,
        nodes: tree.iter().map(OutlineNode::count).sum(),
        tree,
        omitted: walk.omitted,
    }
}

struct Walk {
    nodes: usize,
    omitted: usize,
    /// How many hidden nodes the walk is under: nothing under one is a line.
    hidden: usize,
}

/// A widget's line opened by the walk: the line, when the widget says anything of its own,
/// and the nodes it publishes without a widget of their own.
struct Opened {
    line: Option<OutlineNode>,
    hidden: bool,
    synthetic: Vec<AccessNode>,
}

impl Walk {
    /// Whether one more line fits: one that does not is counted as left out.
    fn fits(&mut self) -> bool {
        if self.nodes >= MAX_NODES {
            self.omitted = self.omitted.saturating_add(1);
            return false;
        }
        true
    }

    /// `line` and the lines under it, put in `parent`: a line that says nothing passes its
    /// children up in its place.
    fn place(
        &mut self,
        line: Option<OutlineNode>,
        children: Vec<OutlineNode>,
        parent: &mut Vec<OutlineNode>,
    ) {
        match line {
            Some(mut line) if line_counts(&line) || !children.is_empty() => {
                self.nodes = self.nodes.saturating_add(1);
                line.children = children;
                parent.push(line);
            }
            _ => parent.extend(children),
        }
    }
}

impl A11yVisitor for Walk {
    type Scope = Vec<OutlineNode>;
    type Pending = Opened;
    type SyntheticPending = (Option<OutlineNode>, bool);

    fn open(
        &mut self,
        w: &dyn Widget,
        parent_child: Option<(&dyn Widget, usize)>,
        _parent: &mut Vec<OutlineNode>,
    ) -> Visit<Opened, Vec<OutlineNode>> {
        if self.hidden > 0 || w.accessibility_hidden() {
            return Visit::Skip;
        }
        let Some(mut access) = w.accessibility() else {
            return Visit::Collapse;
        };
        crate::a11y_tree::walk::with_slot_words(&mut access, parent_child.as_ref());
        let mut access = crate::a11y::with_synthetic_bounds(access, w);
        let synthetic = std::mem::take(&mut access.children);
        let hidden = access.state.hidden;
        if hidden {
            self.hidden = self.hidden.saturating_add(1);
        }
        let line = (!hidden && self.fits()).then(|| node_line(&access, w.get_rect()));
        Visit::Node(
            Opened {
                line,
                hidden,
                synthetic,
            },
            Vec::new(),
        )
    }

    fn close<'w>(
        &mut self,
        w: &'w dyn Widget,
        opened: Opened,
        mut scope: Vec<OutlineNode>,
        hosted: HostedByIndex<'w>,
        _represented: Vec<&'w dyn Widget>,
        parent: &mut Vec<OutlineNode>,
    ) {
        walk_synthetic_children::<true, Self>(self, w, &opened.synthetic, hosted, &mut scope);
        if opened.hidden {
            self.hidden = self.hidden.saturating_sub(1);
        }
        self.place(opened.line, scope, parent);
    }

    fn open_synthetic(
        &mut self,
        _owner: &dyn Widget,
        _path: &[usize],
        node: &AccessNode,
        _parent: &mut Vec<OutlineNode>,
    ) -> ((Option<OutlineNode>, bool), Vec<OutlineNode>) {
        let hidden = self.hidden > 0 || node.state.hidden;
        if hidden {
            self.hidden = self.hidden.saturating_add(1);
            return ((None, true), Vec::new());
        }
        let rect = node
            .bounds
            .map_or(vello::kurbo::Rect::ZERO, crate::a11y::AccessBounds::to_rect);
        let line = self.fits().then(|| node_line(node, rect));
        ((line, false), Vec::new())
    }

    fn close_synthetic(
        &mut self,
        _owner: &dyn Widget,
        _path: &[usize],
        _node: &AccessNode,
        (line, hidden): (Option<OutlineNode>, bool),
        scope: Vec<OutlineNode>,
        parent: &mut Vec<OutlineNode>,
    ) {
        if hidden {
            self.hidden = self.hidden.saturating_sub(1);
            return;
        }
        // A node a widget publishes is a line even when it says nothing yet: it holds a place.
        match line {
            Some(mut line) => {
                self.nodes = self.nodes.saturating_add(1);
                line.children = scope;
                parent.push(line);
            }
            None => parent.extend(scope),
        }
    }
}

/// Whether `node` says anything a reader would see: words, a value, a kind, or children.
fn line_counts(node: &OutlineNode) -> bool {
    node.name.is_some()
        || node.value.is_some()
        || node.kind.is_some()
        || node.description.is_some()
        || !node.state.is_empty()
        || node.action.is_some()
        || node.control.is_some()
        || node.item.is_some()
        || !node.children.is_empty()
}

fn node_line(node: &AccessNode, rect: vello::kurbo::Rect) -> OutlineNode {
    let bounds = node.bounds.map_or(rect, crate::a11y::AccessBounds::to_rect);
    OutlineNode {
        role: crate::a11y::role_name(node.role).to_owned(),
        name: words(node.name.as_deref()),
        value: if node.state.protected {
            None
        } else {
            words(node.value.as_deref())
        },
        kind: words(node.role_description.as_deref()),
        description: words(node.description.as_deref()),
        state: states(node),
        rect: [bounds.x0, bounds.y0, bounds.width(), bounds.height()],
        action: node
            .action
            .clone()
            .or_else(|| node.action_target.clone())
            .filter(|action| !action.trim().is_empty()),
        control: node.semantic_key.clone(),
        item: node.item.clone(),
        children: Vec::new(),
    }
}

/// The states a reader sees, in words.
fn states(node: &AccessNode) -> Vec<&'static str> {
    let state = &node.state;
    let mut words = Vec::new();
    if state.selected {
        words.push("selected");
    }
    match state.checked {
        Some(true) => words.push("checked"),
        Some(false) if state.mixed => words.push("mixed"),
        _ => {}
    }
    match state.expanded {
        Some(true) => words.push("expanded"),
        Some(false) => words.push("collapsed"),
        None => {}
    }
    if state.disabled {
        words.push("disabled");
    } else if state.read_only {
        words.push("read only");
    }
    if state.focused {
        words.push("focused");
    }
    if state.active_descendant {
        words.push("on this option");
    }
    if state.required {
        words.push("required");
    }
    if state.invalid {
        words.push("invalid");
    }
    if state.busy {
        words.push("busy");
    }
    if state.protected {
        words.push("protected");
    }
    words
}

/// `text` made one line of outline words: its line breaks closed into spaces and its length
/// bounded, with nothing in it that reads as outline syntax.
fn words(text: Option<&str>) -> Option<String> {
    let text = text?;
    let mut said = String::with_capacity(text.len().min(MAX_WORDS));
    for word in text.split_whitespace() {
        if said.chars().count() >= MAX_WORDS {
            said.push('…');
            break;
        }
        if !said.is_empty() {
            said.push(' ');
        }
        said.push_str(word);
    }
    (!said.is_empty()).then_some(said)
}

/// Write `node` at `depth` as one line and its children's lines under it.
fn node_text(node: &OutlineNode, depth: usize, out: &mut String) {
    use std::fmt::Write as _;
    if out.len() >= MAX_TEXT_CHARS {
        return;
    }
    for _ in 0..depth {
        out.push_str("  ");
    }
    let _ = write!(out, "{}", node.role);
    if let Some(name) = &node.name {
        let _ = write!(out, " \"{name}\"");
    }
    if let Some(value) = &node.value {
        let _ = write!(out, " = \"{value}\"");
    }
    if let Some(kind) = &node.kind {
        let _ = write!(out, " ({kind})");
    }
    if let Some(description) = &node.description {
        let _ = write!(out, " — \"{description}\"");
    }
    if !node.state.is_empty() {
        let _ = write!(out, " [{}]", node.state.join(", "));
    }
    if let Some(item) = &node.item {
        let _ = write!(out, " <{item}>");
    }
    if let Some(control) = &node.control {
        let _ = write!(out, " #{control}");
    }
    if let Some(action) = &node.action {
        let _ = write!(out, " {{{action}}}");
    }
    out.push('\n');
    let child_depth = depth.saturating_add(1);
    let mut wrote = 0_usize;
    for child in &node.children {
        if out.len() >= MAX_TEXT_CHARS {
            break;
        }
        node_text(child, child_depth, out);
        wrote = wrote.saturating_add(1);
    }
    if wrote < node.children.len() {
        for _ in 0..child_depth {
            out.push_str("  ");
        }
        let _ = writeln!(out, "… {} more", node.children.len().saturating_sub(wrote));
    }
}
