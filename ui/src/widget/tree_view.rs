//! `TreeView` — hierarchical list. One row per visible node, indented by
//! depth; branch nodes carry an expand/collapse glyph that toggles their
//! children's visibility. Selection and focus are tracked separately so
//! arrow-key navigation can move the focus ring without committing a
//! selection until the user explicitly chooses (Enter or click on the row
//! label, vs. clicking the disclosure glyph).
//!
//! Spec: `docs/superpowers/plans/2026-05-22-quick-ui-widget-expansion.md`
//! Unit 15.
//!
//! State lives entirely on the widget — the tree is a static `TreeNode<T>`
//! shape owned by the caller and the per-row UI state (which IDs are
//! expanded, which is selected/focused) is ephemeral display sugar. The
//! `on_select` callback is the canonical way to persist the user's choice
//! into a CRDT slot owned by the caller.

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
use crate::widget::Widget;

/// Stable identifier for a node in the source tree. Callers assign these
/// so the `on_select` callback can be matched against the source data
/// without the widget needing to know about the caller's identity scheme.
pub type NodeId = u64;

/// One node in the source tree. Leaves carry an empty `children` vec;
/// branches carry one or more children. `data` is opaque payload that
/// the caller stores on each node; the widget never inspects it.
pub struct TreeNode<T> {
    pub id: NodeId,
    pub label: String,
    pub data: T,
    pub children: Vec<TreeNode<T>>,
}

impl<T> TreeNode<T> {
    /// Construct a leaf — no children, can never be expanded.
    pub fn leaf(id: NodeId, label: impl Into<String>, data: T) -> Self {
        Self {
            id,
            label: label.into(),
            data,
            children: Vec::new(),
        }
    }

    /// Construct a branch with the given children. A branch with zero
    /// children is permitted (e.g. an empty folder) and still renders
    /// the disclosure glyph so the caller can populate it later.
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

    /// True iff this node has at least one direct child. Used by the
    /// draw pass to decide whether to paint the disclosure glyph and by
    /// `handle_event` to filter ArrowRight/Left toggles.
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
    /// Per-visible-row hit rects populated in `apply_layout`. Each entry
    /// is `(id, full_row_rect)` — the disclosure glyph occupies the left
    /// portion of the row; clicks inside that sub-rect trigger
    /// `toggle_expand` instead of `select`.
    row_rects: Vec<(NodeId, Rect)>,
    rect: Rect,
    is_focused: bool,
    /// Last logical pointer position observed by `handle_event`. Used to
    /// hit-test the next `MouseInput::Pressed`. `None` until the first
    /// `CursorMoved` arrives.
    last_cursor: Option<Point>,
}

/// Width of the disclosure glyph hit area on the left of each branch
/// row. Picked to match the default `▶`/`▼` glyph metrics at the theme
/// `sm` text scale plus a small padding margin. Also used as the
/// horizontal indent unit for the click hit-test (the draw pass uses
/// `theme.tokens.spacing.s4 == 16.0` which matches).
const DISCLOSURE_WIDTH: f64 = 16.0;
/// Vertical height of each row in px. The text baseline sits centred in
/// this band at the theme's `sm` scale.
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

    /// Install a callback that fires whenever a row is selected — either
    /// by click or by the public `select` API. The callback receives the
    /// `NodeId` of the row.
    pub fn on_select<F: FnMut(NodeId) + 'static>(mut self, f: F) -> Self {
        self.on_select = Some(Box::new(f));
        self
    }

    /// True iff `id` is currently expanded. Leaves always return `false`
    /// because they're never added to the expanded set.
    pub fn is_expanded(&self, id: NodeId) -> bool {
        self.expanded.contains(&id)
    }

    /// Flip the expanded state for `id`. Has no effect on leaves —
    /// expanding a leaf would be a no-op anyway since `visible_rows`
    /// only descends into nodes that *have* children, but we still
    /// keep the set tight by refusing to insert non-branch ids.
    pub fn toggle_expand(&mut self, id: NodeId) {
        if !Self::find_node(&self.root, id)
            .map(TreeNode::is_branch)
            .unwrap_or(false)
        {
            return;
        }
        if !self.expanded.insert(id) {
            self.expanded.remove(&id);
        }
    }

    /// Mark `id` as the selected row and fire `on_select`. The widget
    /// does *not* check that `id` is currently visible — collapsing a
    /// parent doesn't clear the selection, so the caller can rely on
    /// `selected()` reflecting their last explicit choice.
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

    /// Snapshot of the currently visible rows, in draw order. Each
    /// entry is `(id, label, depth)`; depth is 0 for the root, 1 for
    /// its children, etc. A node is visible if every ancestor in its
    /// chain is in the expanded set (the root is always visible).
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

    /// Walk the tree looking for the node whose id matches. Returns
    /// `None` for unknown ids.
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

    /// Index of `id` in the current visible-rows list, or `None` if
    /// it's not visible. Used by ArrowDown/Up to step through the
    /// flattened view.
    fn visible_index_of(&self, id: NodeId) -> Option<usize> {
        self.visible_rows().iter().position(|(nid, _, _)| *nid == id)
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
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(height as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &TaffyTree<()>,
        node: taffy::NodeId,
        origin: Point,
    ) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        // Lay out one full-width row per visible node so click hit-tests
        // can address every row by id.
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
        let indent_px = tokens.spacing.s4 as f64;
        let accent = tokens.colors.accent[ACCENT_PRIMARY_IDX];

        // Background fill so the rows blend with the shell surface.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        let rows = self.visible_rows();
        // Pre-compute branch-status lookup to avoid walking the tree
        // per row inside the loop.
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

            if self.focused == Some(*id) {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &row_rect,
                );
            }

            let depth_px = (*depth as f64) * indent_px;
            let glyph_x = self.rect.x0 + depth_px;
            let label_x = glyph_x + DISCLOSURE_WIDTH;
            let text_y = y0 + (ROW_HEIGHT - text_size as f64) / 2.0;

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
                // Collect (id, depth) before mutating self via `select`/
                // `toggle_expand` — the visible_rows borrow holds a
                // shared reference into `self.root.label` which would
                // conflict with the mutation below.
                let snapshots: Vec<(NodeId, usize)> =
                    rows.iter().map(|(id, _, depth)| (*id, *depth)).collect();
                for (i, (id, depth)) in snapshots.iter().enumerate() {
                    let Some((_, row_rect)) = self.row_rects.get(i) else { continue };
                    if !row_rect.contains(pt) {
                        continue;
                    }
                    let glyph_x0 = row_rect.x0 + (*depth as f64) * DISCLOSURE_WIDTH;
                    let glyph_x1 = glyph_x0 + DISCLOSURE_WIDTH;
                    let is_branch = Self::find_node(&self.root, *id)
                        .map(TreeNode::is_branch)
                        .unwrap_or(false);
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
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => {
                match logical_key {
                    Key::Named(NamedKey::ArrowRight) => {
                        if let Some(fid) = self.focused {
                            let is_branch = Self::find_node(&self.root, fid)
                                .map(TreeNode::is_branch)
                                .unwrap_or(false);
                            if is_branch && !self.expanded.contains(&fid) {
                                self.expanded.insert(fid);
                                *handled = true;
                            }
                        }
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        if let Some(fid) = self.focused {
                            let is_branch = Self::find_node(&self.root, fid)
                                .map(TreeNode::is_branch)
                                .unwrap_or(false);
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
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "TreeView" }

    fn get_rect(&self) -> Rect { self.rect }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::TaffyTree;
    use taffy::prelude::TaffyMaxContent;
    use winit::event::{KeyEvent, Modifiers};
    use winit::keyboard::{KeyLocation, PhysicalKey};
    use std::cell::RefCell;
    use std::rc::Rc;

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
        ev.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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
        let node = tree.new_leaf(Style::default()).unwrap();
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
        // Pre-condition: only the root is visible.
        let rows = view.visible_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 1);

        view.toggle_expand(1);
        let rows = view.visible_rows();
        let ids: Vec<NodeId> = rows.iter().map(|(id, _, _)| *id).collect();
        assert!(ids.contains(&2), "child1 must be visible after expanding root");
        assert!(ids.contains(&3), "child2 must be visible after expanding root");
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn tree_view_select_node_calls_on_select() {
        let captured: Rc<RefCell<Option<NodeId>>> = Rc::new(RefCell::new(None));
        let captured_clone = captured.clone();
        let mut view = TreeView::new(sample_tree()).on_select(move |id| {
            *captured_clone.borrow_mut() = Some(id);
        });
        // Expand the root so child rows lay out.
        view.toggle_expand(1);

        // Drive layout: we wrap the leaf in a parent with a fixed width
        // *and* set the leaf's container to fill that width via the
        // build_layout result. Taffy's `auto` width on a leaf inside a
        // block container will collapse to zero with `MAX_CONTENT` —
        // mirror the host's behaviour by giving the parent a fixed
        // width and then patching `view.rect` to span the parent's
        // width manually before `apply_layout` runs. The simplest path
        // is to override `view.rect` and `view.row_rects` after layout.
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let node = view.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root_taffy = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(200.0),
                        height: Dimension::length(80.0),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout(root_taffy, taffy::Size::MAX_CONTENT)
            .unwrap();
        view.apply_layout(&taffy, node, Point::ZERO);
        // Patch the host rect/row_rects so the click hit-test has a
        // non-zero-width target. `apply_layout` may collapse `auto`
        // width to 0 depending on the parent's available space — the
        // production caller would always supply a fixed parent width,
        // but the test wraps the leaf directly so we widen it here.
        view.rect = Rect::new(0.0, 0.0, 200.0, view.visible_rows().len() as f64 * ROW_HEIGHT);
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

        // The second visible row is child1 (id = 2). Click in its label
        // region (past the depth-1 indent + disclosure column).
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
        // Focus the root branch and ensure it's collapsed.
        view.focused = Some(1);
        assert!(!view.is_expanded(1));

        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowRight)));

        assert!(view.is_expanded(1), "ArrowRight must expand the focused branch");
    }

    #[test]
    fn tree_view_arrow_down_moves_focus_to_next_visible_row() {
        let mut view = TreeView::new(sample_tree());
        view.toggle_expand(1);
        // Focus the first visible row.
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
}
