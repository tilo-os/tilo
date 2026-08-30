//! `TreeView` — hierarchical list. One row per visible node, indented by
//! depth; branch nodes carry an expand/collapse glyph. Selection and focus
//! are tracked separately so arrow-key navigation can move the focus ring
//! without committing a selection until the user explicitly chooses.
//!
//! Expand/select/focus state lives on the widget; `on_select` is how the
//! caller persists a choice into its own (e.g. CRDT-backed) storage.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

pub type NodeId = u64;

pub struct TreeNode<T> {
    pub id: NodeId,
    pub label: String,
    pub data: T,
    pub children: Vec<TreeNode<T>>,
}

impl<T> TreeNode<T> {
    pub fn leaf(id: NodeId, label: impl Into<String>, data: T) -> Self {
        Self {
            id,
            label: label.into(),
            data,
            children: Vec::new(),
        }
    }

    /// A branch with zero children still renders the disclosure glyph,
    /// so callers can populate it (e.g. an empty folder) later.
    pub fn branch(
        id: NodeId,
        label: impl Into<String>,
        data: T,
        children: Vec<TreeNode<T>>,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            data,
            children,
        }
    }

    fn is_branch(&self) -> bool {
        !self.children.is_empty()
    }
}

pub struct TreeView<T> {
    root: TreeNode<T>,
    expanded: HashSet<NodeId>,
    selected: Option<NodeId>,
    focused: Option<NodeId>,
    on_select: Option<Box<dyn FnMut(NodeId)>>,
    /// Full-row rects; the left `DISCLOSURE_WIDTH` sub-rect toggles
    /// expand instead of selecting.
    row_rects: Vec<(NodeId, Rect)>,
    rect: Rect,
    is_focused: bool,
    last_cursor: Option<Point>,
}

/// Matches `theme.tokens.spacing.s4` (16.0); also the per-depth indent unit.
const DISCLOSURE_WIDTH: f64 = 16.0;
const ROW_HEIGHT: f64 = 22.0;

impl<T> TreeView<T> {
    pub fn new(root: TreeNode<T>) -> Self {
        Self {
            root,
            expanded: HashSet::new(),
            selected: None,
            focused: None,
            on_select: None,
            row_rects: Vec::new(),
            rect: Rect::ZERO,
            is_focused: false,
            last_cursor: None,
        }
    }

    pub fn on_select<F: FnMut(NodeId) + 'static>(mut self, f: F) -> Self {
        self.on_select = Some(Box::new(f));
        self
    }

    pub fn is_expanded(&self, id: NodeId) -> bool {
        self.expanded.contains(&id)
    }

    pub fn toggle_expand(&mut self, id: NodeId) {
        if !Self::find_node(&self.root, id).is_some_and(TreeNode::is_branch) {
            return;
        }
        if !self.expanded.insert(id) {
            self.expanded.remove(&id);
        }
    }

    /// Does not require `id` to be visible: collapsing a parent doesn't
    /// clear the selection.
    pub fn select(&mut self, id: NodeId) {
        self.selected = Some(id);
        if let Some(cb) = self.on_select.as_mut() {
            cb(id);
        }
    }

    pub fn selected(&self) -> Option<NodeId> {
        self.selected
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    /// `(id, label, depth)`, depth 0 = root; a node is visible only if
    /// every ancestor is in the expanded set.
    pub fn visible_rows(&self) -> Vec<(NodeId, &str, usize)> {
        let mut out = Vec::new();
        Self::collect_visible(&self.root, 0, &self.expanded, &mut out);
        out
    }

    fn collect_visible<'a>(
        node: &'a TreeNode<T>,
        depth: usize,
        expanded: &HashSet<NodeId>,
        out: &mut Vec<(NodeId, &'a str, usize)>,
    ) {
        out.push((node.id, node.label.as_str(), depth));
        if expanded.contains(&node.id) {
            for child in &node.children {
                Self::collect_visible(child, depth + 1, expanded, out);
            }
        }
    }

    fn find_node(node: &TreeNode<T>, id: NodeId) -> Option<&TreeNode<T>> {
        if node.id == id {
            return Some(node);
        }
        for child in &node.children {
            if let Some(found) = Self::find_node(child, id) {
                return Some(found);
            }
        }
        None
    }

    fn visible_index_of(&self, id: NodeId) -> Option<usize> {
        self.visible_rows()
            .iter()
            .position(|(nid, _, _)| *nid == id)
    }
}

impl<T: 'static> Widget for TreeView<T> {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let rows = self.visible_rows().len().max(1);
        let height = (rows as f64) * ROW_HEIGHT;
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::auto(),
                height: Dimension::length(height as f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let rows = self.visible_rows();
        self.row_rects = rows
            .iter()
            .enumerate()
            .map(|(i, (id, _, _))| {
                let y0 = self.rect.y0 + (i as f64) * ROW_HEIGHT;
                let y1 = y0 + ROW_HEIGHT;
                (*id, Rect::new(self.rect.x0, y0, self.rect.x1, y1))
            })
            .collect();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let text_size = tokens.typography.scale.sm;
        let indent_px = f64::from(tokens.spacing.s4);
        let accent = tokens.colors.accent[ACCENT_PRIMARY_IDX];

        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        let rows = self.visible_rows();
        // Avoids a per-row tree walk inside the loop below.
        let mut is_branch_map: HashMap<NodeId, bool> = HashMap::new();
        for (id, _, _) in &rows {
            if let Some(n) = Self::find_node(&self.root, *id) {
                is_branch_map.insert(*id, n.is_branch());
            }
        }

        for (i, (id, label, depth)) in rows.iter().enumerate() {
            let y0 = self.rect.y0 + (i as f64) * ROW_HEIGHT;
            let y1 = y0 + ROW_HEIGHT;
            let row_rect = Rect::new(self.rect.x0, y0, self.rect.x1, y1);

            if self.selected == Some(*id) {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    accent.with_alpha_factor(0.3),
                    None,
                    &row_rect,
                );
            }

            if self.is_focused && self.focused == Some(*id) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &row_rect,
                );
            }

            let depth_px = (*depth as f64) * indent_px;
            let glyph_x = self.rect.x0 + depth_px;
            let label_x = glyph_x + DISCLOSURE_WIDTH;
            let text_y = y0 + (ROW_HEIGHT - f64::from(text_size)) / 2.0;

            if is_branch_map.get(id).copied().unwrap_or(false) {
                let glyph = if self.expanded.contains(id) {
                    "\u{25BC}"
                } else {
                    "\u{25B6}"
                };
                text_renderer.draw_text(
                    scene,
                    glyph,
                    text_size,
                    tokens.colors.text_dim,
                    transform * Affine::translate((glyph_x, text_y)),
                    None,
                );
            }

            text_renderer.draw_text(
                scene,
                label,
                text_size,
                tokens.colors.text_primary,
                transform * Affine::translate((label_x, text_y)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Some(Point::new(
                    position.x / scale_factor,
                    position.y / scale_factor,
                ));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(pt) = self.last_cursor else { return };
                let rows = self.visible_rows();
                // Snapshot before mutating via select/toggle_expand below:
                // `rows` borrows `self.root` and would conflict otherwise.
                let snapshots: Vec<(NodeId, usize)> =
                    rows.iter().map(|(id, _, depth)| (*id, *depth)).collect();
                for (i, (id, depth)) in snapshots.iter().enumerate() {
                    let Some((_, row_rect)) = self.row_rects.get(i) else {
                        continue;
                    };
                    if !row_rect.contains(pt) {
                        continue;
                    }
                    let glyph_x0 = row_rect.x0 + (*depth as f64) * DISCLOSURE_WIDTH;
                    let glyph_x1 = glyph_x0 + DISCLOSURE_WIDTH;
                    let is_branch =
                        Self::find_node(&self.root, *id).is_some_and(TreeNode::is_branch);
                    if is_branch && pt.x >= glyph_x0 && pt.x < glyph_x1 {
                        self.toggle_expand(*id);
                    } else {
                        self.select(*id);
                    }
                    self.focused = Some(*id);
                    *focus_target = Some(node_id);
                    *handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match logical_key {
                Key::Named(NamedKey::ArrowRight) => {
                    if let Some(fid) = self.focused {
                        let is_branch =
                            Self::find_node(&self.root, fid).is_some_and(TreeNode::is_branch);
                        if is_branch && !self.expanded.contains(&fid) {
                            self.expanded.insert(fid);
                            *handled = true;
                        }
                    }
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    if let Some(fid) = self.focused {
                        let is_branch =
                            Self::find_node(&self.root, fid).is_some_and(TreeNode::is_branch);
                        if is_branch && self.expanded.contains(&fid) {
                            self.expanded.remove(&fid);
                            *handled = true;
                        }
                    }
                }
                Key::Named(NamedKey::ArrowDown) => {
                    let rows = self.visible_rows();
                    if rows.is_empty() {
                        return;
                    }
                    let next_idx = match self.focused.and_then(|f| self.visible_index_of(f)) {
                        Some(i) => (i + 1).min(rows.len() - 1),
                        None => 0,
                    };
                    self.focused = Some(rows[next_idx].0);
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    let rows = self.visible_rows();
                    if rows.is_empty() {
                        return;
                    }
                    let prev_idx = match self.focused.and_then(|f| self.visible_index_of(f)) {
                        Some(i) => i.saturating_sub(1),
                        None => 0,
                    };
                    self.focused = Some(rows[prev_idx].0);
                    *handled = true;
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "TreeView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Rows are drawn, not child widgets, so the Tree is the single
        // accessible container. Labels are runtime strings (host-provided
        // node names), not FTL keys, so no `i18n!` wrap.
        let value = self
            .selected
            .and_then(|id| Self::find_node(&self.root, id))
            .map(|n| n.label.clone());
        let mut node = AccessNode::new(Role::Tree)
            .with_optional_value(value)
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: !self.is_enabled(),
                ..Default::default()
            });
        if let Some(sel) = self.selected {
            let rows = self.visible_rows();
            if let Some(idx) = rows.iter().position(|(id, _, _)| *id == sel) {
                node = node.with_position(idx + 1, rows.len());
                node = node.with_level(rows[idx].2 + 1);
            }
        }
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use taffy::TaffyTree;
    use taffy::prelude::TaffyMaxContent;
    use winit::event::{KeyEvent, Modifiers};
    use winit::keyboard::{KeyLocation, PhysicalKey};

    fn sample_tree() -> TreeNode<()> {
        TreeNode::branch(
            1,
            "root",
            (),
            vec![
                TreeNode::leaf(2, "child1", ()),
                TreeNode::leaf(3, "child2", ()),
            ],
        )
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn key_press(logical_key: Key) -> WindowEvent {
        // SAFETY: KeyEvent has private fields in winit 0.30; the public
        // ones tolerate the zero bit pattern and `handle_event` only
        // inspects `logical_key` + `state`.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn dispatch(view: &mut TreeView<()>, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        view.handle_event(
            event,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    #[test]
    fn tree_view_expand_node_shows_children() {
        let mut view = TreeView::new(sample_tree());
        let rows = view.visible_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 1);

        view.toggle_expand(1);
        let rows = view.visible_rows();
        let ids: Vec<NodeId> = rows.iter().map(|(id, _, _)| *id).collect();
        assert!(
            ids.contains(&2),
            "child1 must be visible after expanding root"
        );
        assert!(
            ids.contains(&3),
            "child2 must be visible after expanding root"
        );
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn tree_view_select_node_calls_on_select() {
        let captured: Rc<RefCell<Option<NodeId>>> = Rc::new(RefCell::new(None));
        let captured_clone = captured.clone();
        let mut view = TreeView::new(sample_tree()).on_select(move |id| {
            *captured_clone.borrow_mut() = Some(id);
        });
        view.toggle_expand(1);

        // Taffy's `auto` width on a leaf inside a block container collapses
        // to zero under `MAX_CONTENT`, so wrap it in a fixed-width parent
        // and patch `view.rect`/`row_rects` manually rather than trust
        // `apply_layout`'s width.
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let node = view.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root_taffy = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(200.0),
                    height: Dimension::length(80.0),
                },
                ..Default::default()
            },
            &[node],
        );
        taffy
            .compute_layout(root_taffy, taffy::Size::MAX_CONTENT)
            .unwrap();
        view.apply_layout(&taffy, node, Point::ZERO);
        view.rect = Rect::new(
            0.0,
            0.0,
            200.0,
            view.visible_rows().len() as f64 * ROW_HEIGHT,
        );
        view.row_rects = view
            .visible_rows()
            .iter()
            .enumerate()
            .map(|(i, (id, _, _))| {
                let y0 = view.rect.y0 + (i as f64) * ROW_HEIGHT;
                let y1 = y0 + ROW_HEIGHT;
                (*id, Rect::new(view.rect.x0, y0, view.rect.x1, y1))
            })
            .collect();

        assert!(view.row_rects.len() >= 2);
        let (target_id, target_rect) = view.row_rects[1];
        assert_eq!(target_id, 2);
        let click_x = target_rect.x0 + 60.0; // past depth-1 indent + glyph
        let click_y = target_rect.y0 + target_rect.height() / 2.0;

        dispatch(&mut view, &cursor_moved(click_x, click_y));
        dispatch(&mut view, &mouse_press());

        assert_eq!(
            *captured.borrow(),
            Some(2),
            "on_select must fire with child1 id"
        );
        assert_eq!(view.selected(), Some(2));
    }

    #[test]
    fn tree_view_arrow_right_expands_focused_node() {
        let mut view = TreeView::new(sample_tree());
        view.focused = Some(1);
        assert!(!view.is_expanded(1));

        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowRight)));

        assert!(
            view.is_expanded(1),
            "ArrowRight must expand the focused branch"
        );
    }

    #[test]
    fn tree_view_arrow_down_moves_focus_to_next_visible_row() {
        let mut view = TreeView::new(sample_tree());
        view.toggle_expand(1);
        view.focused = Some(1);

        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowDown)));

        let rows = view.visible_rows();
        let second_id = rows[1].0;
        assert_eq!(
            view.focused(),
            Some(second_id),
            "ArrowDown must move focus from row 0 to row 1",
        );
    }

    #[test]
    fn accessibility_reports_tree_role_and_no_value_when_unselected() {
        use crate::a11y::Role;
        let view = TreeView::new(sample_tree());
        let node = Widget::accessibility(&view).expect("tree is not presentational");
        assert_eq!(node.role, Role::Tree);
        assert_eq!(node.name, None, "the tree has no intrinsic label");
        assert_eq!(node.value, None, "no value until a row is selected");
        assert!(node.state.focusable);
        assert!(!node.state.disabled);
        assert!(!node.state.focused);
    }

    #[test]
    fn accessibility_value_is_selected_row_label() {
        let mut view = TreeView::new(sample_tree());
        view.toggle_expand(1);
        view.select(2); // "child1"
        let node = Widget::accessibility(&view).unwrap();
        assert_eq!(
            node.value.as_deref(),
            Some("child1"),
            "value is the selected node's label",
        );
    }

    #[test]
    fn accessibility_reflects_focus_state() {
        let mut view = TreeView::new(sample_tree());
        view.set_focus(true);
        let node = Widget::accessibility(&view).unwrap();
        assert!(
            node.state.focused,
            "widget focus surfaces in the access node"
        );
    }
}
