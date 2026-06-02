//! `NoteTreeView` — left-pane page-tree widget.
//!
//! Renders the workspace tree (one row per page, indented by depth),
//! handles selection / expansion clicks, double-click to enter inline
//! rename mode (a local-mode `TextInput` overlaid on the row), and
//! issues commands to the parent `NotesApp` via an [`OutEvent`] queue
//! the parent drains every tick.
//!
//! Keyboard: pre-rename the user can press
//!   - Enter to commit, Escape to cancel,
//!   - Delete to ask the parent to delete the selected page,
//!   - Cmd-N (or Ctrl-N) to ask the parent to create a child of the
//!     selected page.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::TextInput;

use crate::model::{PageId, Workspace};

/// Per-row height; matches `TreeView::ROW_HEIGHT` so the visual cadence
/// reads as part of the same widget family.
const ROW_HEIGHT: f64 = 24.0;
/// Horizontal indent per tree depth level.
const INDENT_PX: f64 = 16.0;
/// Glyph hit-area width — the expand/collapse triangle on the left of
/// a branch row.
const GLYPH_WIDTH: f64 = 16.0;
/// Time gap below which two clicks count as a double-click. Matches
/// `ui::widget::text_input::MULTI_CLICK_INTERVAL`.
const DOUBLE_CLICK_INTERVAL_MS: u128 = 400;
/// Pixels of cursor jitter still tolerated between the two clicks of
/// a double-click.
const DOUBLE_CLICK_RADIUS_PX: f64 = 4.0;
/// Default text size for tree row labels.
const ROW_TEXT_SIZE: f32 = 14.0;

/// Outbound commands from the sidebar to the parent `NotesApp`. The
/// parent owns the workspace model and storage — the sidebar can only
/// *ask* for things to happen.
#[derive(Debug, Clone)]
pub enum OutEvent {
    Select(PageId),
    Rename { id: PageId, new_title: String },
    Delete(PageId),
    /// Create a child of `parent` (None = root-level).
    CreateChild { parent: Option<PageId> },
}

/// What the sidebar is doing this frame: idle, or actively renaming
/// one specific page.
struct RenameState {
    /// The page being renamed.
    page: PageId,
    /// The active editor. Local-mode TextInput; we drive `set_text`
    /// once when we open it and drain `take_dirty` on commit. The
    /// editor lives in this struct so we can route events to it
    /// without exposing it through the widget tree.
    input: TextInput,
}

pub struct NoteTreeView {
    /// Snapshot of the page tree at sync-time. Re-built from the
    /// [`Workspace`] every `sync` so we don't hold mutable references
    /// across frames.
    flat_rows: Vec<RowEntry>,
    expanded: HashSet<PageId>,
    selected: Option<PageId>,
    rect: Rect,
    is_focused: bool,
    last_cursor: Option<Point>,
    last_modifiers: ModifiersState,
    /// Per-row hit rect, recomputed in `apply_layout` from the flat
    /// row vector. Indexed identically.
    row_rects: Vec<Rect>,
    /// Currently active double-click detection state.
    last_click: Option<ClickRecord>,
    rename: Option<RenameState>,
    /// Outbound commands the parent should drain via [`take_events`].
    pending_events: Vec<OutEvent>,
    /// True until the next `mark_drawn`. Coarse: every state mutation
    /// flips this on. Worth optimising later when we have profiler
    /// evidence that the sidebar is a hotspot.
    dirty: bool,
}

struct RowEntry {
    id: PageId,
    title: String,
    depth: usize,
    has_children: bool,
}

struct ClickRecord {
    page: PageId,
    at: std::time::Instant,
    pos: Point,
}

impl NoteTreeView {
    pub fn new() -> Self {
        Self {
            flat_rows: Vec::new(),
            expanded: HashSet::new(),
            selected: None,
            rect: Rect::ZERO,
            is_focused: false,
            last_cursor: None,
            last_modifiers: ModifiersState::empty(),
            row_rects: Vec::new(),
            last_click: None,
            rename: None,
            pending_events: Vec::new(),
            dirty: true,
        }
    }

    /// Drain any outbound commands accumulated since the last call.
    /// Called by `NotesApp` once per tick after `handle_event`.
    pub fn take_events(&mut self) -> Vec<OutEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// Rebuild the flat row vector from the [`Workspace`]. Called by
    /// the parent during `sync_from_workspace`. Cheap — workspace
    /// trees are at most a few hundred pages in normal use.
    pub fn sync_from_workspace(&mut self, ws: &Workspace) {
        self.flat_rows.clear();
        let roots = ws.children_of(None);
        for root in roots {
            self.push_recursive(ws, root.id, 0);
        }
        self.selected = ws.selected();
        // If the page being renamed has disappeared (e.g. a delete
        // happened while in rename mode), close the editor.
        if let Some(r) = &self.rename {
            if ws.get(r.page).is_none() {
                self.rename = None;
            }
        }
        self.dirty = true;
    }

    fn push_recursive(&mut self, ws: &Workspace, id: PageId, depth: usize) {
        let page = match ws.get(id) {
            Some(p) => p,
            None => return,
        };
        let children = ws.children_of(Some(id));
        let has_children = !children.is_empty();
        self.flat_rows.push(RowEntry {
            id,
            title: page.title.clone(),
            depth,
            has_children,
        });
        if self.expanded.contains(&id) {
            for child in children {
                self.push_recursive(ws, child.id, depth + 1);
            }
        }
    }

    /// Open the inline rename editor on the given page.
    pub fn begin_rename(&mut self, id: PageId, initial_title: &str) {
        let mut input = TextInput::local().with_initial_value(initial_title);
        // Mark as focused so the caret blinks immediately and key
        // events route to it without an extra click.
        input.set_focus(true);
        self.rename = Some(RenameState { page: id, input });
        self.dirty = true;
    }

    /// True when an inline rename editor is currently open.
    pub fn is_renaming(&self) -> bool {
        self.rename.is_some()
    }

    /// Force-cancel any rename in progress without committing.
    fn cancel_rename(&mut self) {
        self.rename = None;
        self.dirty = true;
    }

    /// Commit the rename editor (push a `Rename` event) and close it.
    fn commit_rename(&mut self) {
        if let Some(r) = self.rename.take() {
            let new_title = r.input.value().trim().to_string();
            // Drop empty renames — keep the existing title untouched.
            if !new_title.is_empty() {
                self.pending_events.push(OutEvent::Rename {
                    id: r.page,
                    new_title,
                });
            }
            self.dirty = true;
        }
    }
}

impl Default for NoteTreeView {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for NoteTreeView {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let rows = self.flat_rows.len().max(1);
        let height = (rows as f64) * ROW_HEIGHT + 8.0;
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(160.0),
                    height: Dimension::length(height as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let new_rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        let resized = new_rect.width() != self.rect.width()
            || new_rect.height() != self.rect.height();
        self.rect = new_rect;
        // Position each row.
        self.row_rects.clear();
        for i in 0..self.flat_rows.len() {
            let y0 = self.rect.y0 + 4.0 + (i as f64) * ROW_HEIGHT;
            let y1 = y0 + ROW_HEIGHT;
            self.row_rects
                .push(Rect::new(self.rect.x0, y0, self.rect.x1, y1));
        }
        if resized {
            self.dirty = true;
        }
        // If a rename is active, position the embedded editor against
        // the current row rect — it follows the row across resize.
        if let Some(r) = &mut self.rename {
            if let Some(idx) = self
                .flat_rows
                .iter()
                .position(|row| row.id == r.page)
            {
                let row_rect = self.row_rects[idx];
                let glyph_x = row_rect.x0 + (self.flat_rows[idx].depth as f64) * INDENT_PX;
                let editor_rect = Rect::new(
                    glyph_x + GLYPH_WIDTH,
                    row_rect.y0,
                    row_rect.x1 - 4.0,
                    row_rect.y1,
                );
                r.input.apply_layout_external(editor_rect);
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        // Sidebar background — slightly off the main surface so the
        // split between panes reads.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &self.rect,
        );

        for (i, row) in self.flat_rows.iter().enumerate() {
            let row_rect = self.row_rects[i];
            if self.selected == Some(row.id) {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.accent_primary.with_alpha_factor(0.25),
                    None,
                    &row_rect,
                );
            }
            let glyph_x = row_rect.x0 + (row.depth as f64) * INDENT_PX;
            let label_x = glyph_x + GLYPH_WIDTH;
            let text_y = row_rect.y0 + (ROW_HEIGHT - ROW_TEXT_SIZE as f64) / 2.0;

            if row.has_children {
                let glyph = if self.expanded.contains(&row.id) {
                    "\u{25BC}" // ▼
                } else {
                    "\u{25B6}" // ▶
                };
                text_renderer.draw_text(
                    scene,
                    glyph,
                    ROW_TEXT_SIZE,
                    tokens.colors.text_dim,
                    transform * Affine::translate((glyph_x + 2.0, text_y)),
                    None,
                );
            }

            // Skip the label when the row is being renamed — the
            // embedded editor paints in its place.
            let is_renaming = matches!(&self.rename, Some(r) if r.page == row.id);
            if !is_renaming {
                text_renderer.draw_text(
                    scene,
                    &row.title,
                    ROW_TEXT_SIZE,
                    tokens.colors.text_primary,
                    transform * Affine::translate((label_x, text_y)),
                    Some((row_rect.x1 - label_x - 4.0) as f32),
                );
            }
        }

        // Paint the embedded rename editor *over* the row.
        if let Some(r) = &mut self.rename {
            r.input.draw(scene, transform, text_renderer, theme, presence);
        }

        // Focus ring around the whole sidebar when the panel itself
        // has keyboard focus — a small accessibility win for the
        // Cmd-N / Delete shortcuts that target the selected page.
        if self.is_focused {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                tokens.colors.border_focus,
                None,
                &self.rect,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.last_modifiers = modifiers.state();

        // Route everything to the active rename editor first — it
        // owns focus while open. Enter / Escape are handled by us
        // *before* forwarding so the editor doesn't try to swallow
        // them.
        if let Some(r) = &mut self.rename {
            if let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            {
                match logical_key {
                    Key::Named(NamedKey::Enter) => {
                        self.commit_rename();
                        *handled = true;
                        return;
                    }
                    Key::Named(NamedKey::Escape) => {
                        self.cancel_rename();
                        *handled = true;
                        return;
                    }
                    _ => {}
                }
            }
            r.input.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
            // Any user edit clears the dirty flag on the input;
            // we mark our own dirty so the next frame re-emits.
            if r.input.take_dirty().is_some() {
                self.dirty = true;
            }
            if *handled {
                return;
            }
        }

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
                if !self.rect.contains(pt) {
                    return;
                }
                for i in 0..self.flat_rows.len() {
                    let row_rect = self.row_rects[i];
                    if !row_rect.contains(pt) {
                        continue;
                    }
                    let row_id = self.flat_rows[i].id;
                    let depth = self.flat_rows[i].depth;
                    let has_children = self.flat_rows[i].has_children;
                    let glyph_x0 = row_rect.x0 + (depth as f64) * INDENT_PX;
                    let glyph_x1 = glyph_x0 + GLYPH_WIDTH;
                    if has_children && pt.x >= glyph_x0 && pt.x < glyph_x1 {
                        if !self.expanded.insert(row_id) {
                            self.expanded.remove(&row_id);
                        }
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
                    // Double-click on the label → enter rename mode.
                    let now = std::time::Instant::now();
                    let is_double = self
                        .last_click
                        .as_ref()
                        .map(|c| {
                            c.page == row_id
                                && now.duration_since(c.at).as_millis()
                                    < DOUBLE_CLICK_INTERVAL_MS
                                && (c.pos.distance(pt)) < DOUBLE_CLICK_RADIUS_PX
                        })
                        .unwrap_or(false);
                    self.last_click = Some(ClickRecord {
                        page: row_id,
                        at: now,
                        pos: pt,
                    });
                    if is_double {
                        self.begin_rename(row_id, &self.flat_rows[i].title.clone());
                        *handled = true;
                        return;
                    }
                    self.selected = Some(row_id);
                    self.pending_events.push(OutEvent::Select(row_id));
                    self.is_focused = true;
                    *focus_target = Some(node_id);
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                // Click landed inside the sidebar rect but outside
                // any row — claim focus so Cmd-N still targets us,
                // but don't change selection.
                self.is_focused = true;
                *focus_target = Some(node_id);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if !self.is_focused {
                    return;
                }
                let mods = modifiers.state();
                // Cmd-N / Ctrl-N — create child of current selection.
                if matches!(logical_key, Key::Character(c) if c.eq_ignore_ascii_case("n"))
                    && (mods.contains(ModifiersState::SUPER)
                        || mods.contains(ModifiersState::CONTROL))
                {
                    self.pending_events.push(OutEvent::CreateChild {
                        parent: self.selected,
                    });
                    *handled = true;
                    return;
                }
                match logical_key {
                    Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                        if let Some(sel) = self.selected {
                            self.pending_events.push(OutEvent::Delete(sel));
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        if let Some(idx) =
                            self.selected.and_then(|s| self.row_index_of(s))
                        {
                            let next = (idx + 1).min(self.flat_rows.len().saturating_sub(1));
                            if next != idx {
                                let id = self.flat_rows[next].id;
                                self.selected = Some(id);
                                self.pending_events.push(OutEvent::Select(id));
                                self.dirty = true;
                                *handled = true;
                            }
                        } else if !self.flat_rows.is_empty() {
                            let id = self.flat_rows[0].id;
                            self.selected = Some(id);
                            self.pending_events.push(OutEvent::Select(id));
                            self.dirty = true;
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        if let Some(idx) =
                            self.selected.and_then(|s| self.row_index_of(s))
                        {
                            let next = idx.saturating_sub(1);
                            if next != idx {
                                let id = self.flat_rows[next].id;
                                self.selected = Some(id);
                                self.pending_events.push(OutEvent::Select(id));
                                self.dirty = true;
                                *handled = true;
                            }
                        }
                    }
                    Key::Named(NamedKey::F2) => {
                        if let Some(sel) = self.selected {
                            if let Some(idx) = self.row_index_of(sel) {
                                let title = self.flat_rows[idx].title.clone();
                                self.begin_rename(sel, &title);
                                *handled = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "NoteTreeView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
        // Dropping focus from the panel collapses any open rename
        // editor — we don't have a sensible "commit on blur" UX
        // story yet, so cancel rather than silently committing.
        if !focused && self.rename.is_some() {
            self.cancel_rename();
        }
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl NoteTreeView {
    fn row_index_of(&self, id: PageId) -> Option<usize> {
        self.flat_rows.iter().position(|r| r.id == id)
    }

    /// Used by tests + the parent: how many rows the tree exposes
    /// right now.
    #[cfg(test)]
    pub fn visible_row_count(&self) -> usize {
        self.flat_rows.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_from_workspace_flattens_to_visible_rows() {
        let mut ws = Workspace::new();
        let root = ws.create_page(None);
        let child = ws.create_page(Some(root));
        let _grand = ws.create_page(Some(child));
        let mut tree = NoteTreeView::new();
        tree.sync_from_workspace(&ws);
        // Only the root is visible by default — branches start collapsed.
        assert_eq!(tree.visible_row_count(), 1);
        tree.expanded.insert(root);
        tree.sync_from_workspace(&ws);
        assert_eq!(tree.visible_row_count(), 2);
    }

    #[test]
    fn begin_and_commit_rename_emits_event() {
        let mut ws = Workspace::new();
        let id = ws.create_page(None);
        let mut tree = NoteTreeView::new();
        tree.sync_from_workspace(&ws);
        tree.begin_rename(id, "Untitled");
        // Drain any spurious dirty events from `take_dirty()`.
        tree.commit_rename();
        let events = tree.take_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            OutEvent::Rename { id: rid, new_title } => {
                assert_eq!(*rid, id);
                assert_eq!(new_title, "Untitled");
            }
            _ => panic!("expected Rename event"),
        }
    }

    #[test]
    fn cancel_rename_does_not_emit() {
        let mut ws = Workspace::new();
        let id = ws.create_page(None);
        let mut tree = NoteTreeView::new();
        tree.sync_from_workspace(&ws);
        tree.begin_rename(id, "Untitled");
        tree.cancel_rename();
        assert!(tree.take_events().is_empty());
    }
}
