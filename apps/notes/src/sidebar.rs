//! `NoteTreeView` — left-pane page-tree widget.
//!
//! Renders the workspace tree (one row per page, indented by depth),
//! a header strip with a "+" button that creates a new root page,
//! handles selection / expansion clicks, double-click to enter inline
//! rename mode (a local-mode `TextInput` overlaid on the row), and
//! issues commands to the parent `NotesApp` via an [`OutEvent`] queue
//! the parent drains every tick.
//!
//! Keyboard (only while the panel itself holds focus, since these keys
//! otherwise belong to the editor):
//!   - Delete to delete the selected page (NOT Backspace — the editor and
//!     the tree can both hold focus, so Backspace stays the editor's),
//!   - F2 to rename the selected page,
//!   - ↑ / ↓ to move the selection between rows,
//!   - Alt-↑ / Alt-↓ to reorder the selected page among its siblings,
//!   - while renaming: Enter to commit, Escape to cancel.
//!
//! Creating a child of the selection (Cmd-N / Ctrl-N) is handled one
//! level up in `NotesApp`, so it works regardless of which pane has
//! focus — the editor holds focus on load.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::TextInput;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::drag::{DragController, ListMetrics};

use crate::model::{PageId, Workspace};
use crate::num;

/// Per-row height. Comfortable rather than cramped — a page row breathes
/// with vertical air above and below its label instead of butting rows
/// edge to edge.
const ROW_HEIGHT: f64 = 30.0;
/// Height of the sidebar header strip that hosts the "Pages" label and
/// the "+" new-page button. Rows start below it.
const HEADER_HEIGHT: f64 = 42.0;
/// Side length of the square "+" new-page button in the header.
const NEW_BTN_SIZE: f64 = 26.0;
/// Right/vertical inset of the "+" button inside the header strip.
const NEW_BTN_MARGIN: f64 = 8.0;
/// Left content inset: rows, the header label and the selection pill all
/// start this far in from the sidebar's left edge so nothing is jammed
/// against the window frame.
const ROW_INSET_X: f64 = 12.0;
/// Horizontal margin the selection pill leaves on each side of a row, so
/// the highlight reads as an inset rounded chip, not an edge-to-edge bar.
const ROW_PILL_MARGIN: f64 = 6.0;
/// Horizontal indent per tree depth level.
const INDENT_PX: f64 = 18.0;
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
    Rename {
        id: PageId,
        new_title: String,
    },
    Delete(PageId),
    /// Create a child of `parent` (None = root-level).
    CreateChild {
        parent: Option<PageId>,
    },
    /// Reorder `id` among its siblings: `delta < 0` moves it earlier,
    /// `delta > 0` later. Backs the Alt-↑ / Alt-↓ reorder shortcut.
    Move {
        id: PageId,
        delta: i32,
    },
    /// Re-parent / reorder `id` to be a child of `new_parent` (None =
    /// root), inserted immediately before sibling `before` (None =
    /// append to the group). Backs the drag-to-reorder + re-parent drop.
    Reparent {
        id: PageId,
        new_parent: Option<PageId>,
        before: Option<PageId>,
    },
    /// Deep-copy `id` and its subtree, placing the copy right after it.
    /// Backs the context menu's "Duplicate".
    Duplicate(PageId),
}

/// The action a context-menu row triggers. Kept in a parallel vector to
/// the menu's items so the `take_selection()` index maps back to intent.
#[derive(Clone, Copy, Debug)]
enum RowAction {
    Delete,
    Duplicate,
    NewChild,
    Rename,
}

/// The resolved outcome of the current drag position — what a drop here
/// would do, plus the geometry the drop feedback paints.
enum DropPlan {
    /// Re-parent onto row `row` (append as the target page's last child).
    Onto { row: usize, page: PageId },
    /// Sibling insertion: the dragged page lands at flat index `to`
    /// (the primitive's live target), resolving to (`parent`, `before`).
    /// The caret paints at `to`, indented to `depth`.
    Seam {
        to: usize,
        depth: usize,
        parent: Option<PageId>,
        before: Option<PageId>,
    },
    /// Dropped outside the droppable area — cancel, no change.
    Cancel,
}

/// What the sidebar is doing this frame: idle, or actively renaming
/// one specific page.
struct RenameState {
    /// The page being renamed.
    page: PageId,
    /// The active editor. Local-mode `TextInput`; we drive `set_text`
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
    /// Hit rect of the header "+" new-page button, from `apply_layout`.
    new_page_btn_rect: Rect,
    /// True while the pointer hovers the "+" button, so it can light up.
    new_page_btn_hover: bool,
    /// Currently active double-click detection state.
    last_click: Option<ClickRecord>,
    rename: Option<RenameState>,
    /// Outbound commands the parent should drain via [`take_events`].
    pending_events: Vec<OutEvent>,
    /// Set when the visible-row set needs rebuilding but no [`OutEvent`]
    /// was produced — e.g. after an expand/collapse toggle, which
    /// changes `expanded` locally but can't re-flatten the tree without
    /// the parent's [`Workspace`]. The parent drains it via
    /// [`take_rows_dirty`] and re-runs `sync_from_workspace`.
    rows_dirty: bool,
    /// True until the next `mark_drawn`. Coarse: every state mutation
    /// flips this on. Worth optimising later when we have profiler
    /// evidence that the sidebar is a hotspot.
    dirty: bool,
    /// Drag-to-reorder controller (shared toolkit primitive). Drives the
    /// floating ghost, the live reflow gap, and the insertion index; the
    /// model is only mutated on drop (or not at all on cancel).
    drag: DragController<PageId>,
    /// Right-click context menu on a page row.
    menu: ContextMenu,
    /// The page the open context menu targets. Its rows map back to
    /// intent by fixed index (see `apply_menu_selection`).
    menu_target: Option<PageId>,
    /// True when the open menu is the empty-area menu ("New note"), which has
    /// no page target.
    empty_menu: bool,
    /// False until the first `sync_from_workspace` expands every branch so
    /// the tree opens showing its structure rather than a collapsed flat
    /// list. After that, expand/collapse is fully user-driven.
    did_initial_expand: bool,
    /// The selection observed on the previous sync. When it changes we
    /// expand the new selection's ancestors so the selected row is always
    /// visible (e.g. after a `[[link]]` jump into a nested page).
    last_selection: Option<PageId>,
}

struct RowEntry {
    id: PageId,
    title: String,
    depth: usize,
    has_children: bool,
    parent: Option<PageId>,
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
            new_page_btn_rect: Rect::ZERO,
            new_page_btn_hover: false,
            last_click: None,
            rename: None,
            pending_events: Vec::new(),
            rows_dirty: false,
            dirty: true,
            drag: DragController::vertical(),
            menu: ContextMenu::new(),
            menu_target: None,
            empty_menu: false,
            did_initial_expand: false,
            last_selection: None,
        }
    }

    /// Drain any outbound commands accumulated since the last call.
    /// Called by `NotesApp` once per tick after `handle_event`.
    pub fn take_events(&mut self) -> Vec<OutEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// Drain the "visible rows need rebuilding" flag. The parent calls
    /// this after routing events and re-runs [`sync_from_workspace`] when
    /// it comes back true, so an expand/collapse toggle actually shows /
    /// hides the affected children.
    pub fn take_rows_dirty(&mut self) -> bool {
        std::mem::take(&mut self.rows_dirty)
    }

    /// Rebuild the flat row vector from the [`Workspace`]. Called by
    /// the parent during `sync_from_workspace`. Cheap — workspace
    /// trees are at most a few hundred pages in normal use.
    pub fn sync_from_workspace(&mut self, ws: &Workspace) {
        // First render: expand every branch that has children so the tree
        // opens showing its nesting rather than a collapsed flat list.
        if !self.did_initial_expand {
            for p in ws.pages() {
                if !ws.children_of(Some(p.id)).is_empty() {
                    self.expanded.insert(p.id);
                }
            }
            self.did_initial_expand = true;
        }
        // Reveal the selected page: whenever the selection changes, expand
        // its ancestor chain so a nested selection (e.g. from a link jump)
        // is actually on screen.
        let sel = ws.selected();
        if sel != self.last_selection {
            if let Some(id) = sel {
                let mut cur = ws.get(id).and_then(|p| p.parent);
                while let Some(pid) = cur {
                    self.expanded.insert(pid);
                    cur = ws.get(pid).and_then(|p| p.parent);
                }
            }
            self.last_selection = sel;
        }

        self.flat_rows.clear();
        let roots = ws.children_of(None);
        for root in roots {
            self.push_recursive(ws, root.id, 0);
        }
        self.selected = ws.selected();
        // If the page being renamed has disappeared (e.g. a delete
        // happened while in rename mode), close the editor.
        if let Some(r) = &self.rename
            && ws.get(r.page).is_none()
        {
            self.rename = None;
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
            parent: page.parent,
        });
        if self.expanded.contains(&id) {
            for child in children {
                self.push_recursive(ws, child.id, depth + 1);
            }
        }
    }

    /// Open the inline rename editor on the given page.
    pub fn begin_rename(&mut self, id: PageId, initial_title: &str) {
        // Plain chrome + the row's own text size so the field reads as the
        // row itself in edit mode, not a differently-styled box dropped on
        // top. `apply_layout` pins it over the row's label span.
        let mut input = TextInput::local()
            .with_plain_chrome()
            .with_font_size(ROW_TEXT_SIZE)
            .with_initial_value(initial_title);
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

    // ---- drag-to-reorder + re-parent ---------------------------------

    /// Uniform geometry of the visible row list along the drag axis, for
    /// the toolkit's [`DragController`]. Rows are contiguous
    /// (`ROW_HEIGHT` pitch, no gap) starting just below the header.
    fn list_metrics(&self) -> ListMetrics {
        ListMetrics::uniform(
            self.rect.y0 + HEADER_HEIGHT,
            ROW_HEIGHT,
            0.0,
            self.flat_rows.len(),
        )
    }

    /// The row index whose *middle band* the cursor is over, when that
    /// row is a valid re-parent target (not the dragged row and not one
    /// of its descendants). Dropping here nests the dragged page as that
    /// row's child.
    fn onto_target(&self, pt: Point) -> Option<usize> {
        let from = self.drag.from_index()?;
        if pt.x < self.rect.x0 || pt.x > self.rect.x1 {
            return None;
        }
        let n = self.flat_rows.len();
        let list_top = self.rect.y0 + HEADER_HEIGHT;
        let rel = pt.y - list_top;
        if rel < 0.0 {
            return None;
        }
        let raw = num::usize_of_f64((rel / ROW_HEIGHT).floor());
        if raw >= n {
            return None;
        }
        let frac = (rel / ROW_HEIGHT) - num::f64_of_usize(raw);
        if raw == from || self.is_descendant_of_dragged(raw, from) {
            return None;
        }
        // Central 40% band reads as "into this page"; the top/bottom
        // quarters read as "between rows".
        if (0.30..=0.70).contains(&frac) {
            Some(raw)
        } else {
            None
        }
    }

    /// True when flat row `row` is `from` itself or nested under it —
    /// i.e. dropping into it would fold the dragged subtree into its own
    /// branch. Walks the visible rows' parent links.
    fn is_descendant_of_dragged(&self, row: usize, from: usize) -> bool {
        let dragged = self.flat_rows[from].id;
        let mut cur = Some(self.flat_rows[row].id);
        let mut hops = 0;
        while let Some(c) = cur {
            if c == dragged {
                return true;
            }
            if hops > 4096 {
                break;
            }
            cur = self
                .flat_rows
                .iter()
                .find(|r| r.id == c)
                .and_then(|r| r.parent);
            hops += 1;
        }
        false
    }

    /// Resolve the primitive's live target index `to` (the seam the ghost
    /// would drop into) to a `(parent, before, depth)` sibling insertion,
    /// derived from the rows either side of the seam with the dragged row
    /// removed.
    fn seam_target(&self, to: usize, from: usize) -> (Option<PageId>, Option<PageId>, usize) {
        let reduced: Vec<&RowEntry> = self
            .flat_rows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != from)
            .map(|(_, r)| r)
            .collect();
        let above = if to == 0 {
            None
        } else {
            reduced.get(to - 1).copied()
        };
        let below = reduced.get(to).copied();
        match (above, below) {
            // Insert as a sibling of the row just below the seam, before
            // it. When `below` is deeper than `above` (right after an
            // expanded parent) this nests the page as that parent's first
            // child — a valid "drop into a page".
            (_, Some(b)) => (b.parent, Some(b.id), b.depth),
            // Past the last row: append to the last row's own group.
            (Some(a), None) => (a.parent, None, a.depth),
            (None, None) => (None, None, 0),
        }
    }

    /// What a drop at the current cursor would do (feedback + commit share
    /// this single source of truth). `Cancel` when the pointer has left
    /// the droppable area.
    fn drop_plan(&self, pt: Point) -> DropPlan {
        let Some(from) = self.drag.from_index() else {
            return DropPlan::Cancel;
        };
        let n = self.flat_rows.len();
        if n == 0 {
            return DropPlan::Cancel;
        }
        // Dropped clear of the sidebar column → cancel.
        if pt.x < self.rect.x0 - 40.0 || pt.x > self.rect.x1 + 40.0 {
            return DropPlan::Cancel;
        }
        let list_top = self.rect.y0 + HEADER_HEIGHT;
        let list_bottom = list_top + num::f64_of_usize(n) * ROW_HEIGHT;
        if pt.y < list_top - ROW_HEIGHT || pt.y > list_bottom + ROW_HEIGHT {
            return DropPlan::Cancel;
        }
        if let Some(row) = self.onto_target(pt) {
            return DropPlan::Onto {
                row,
                page: self.flat_rows[row].id,
            };
        }
        let to = self.drag.insertion().unwrap_or(from);
        let (parent, before, depth) = self.seam_target(to, from);
        DropPlan::Seam {
            to,
            depth,
            parent,
            before,
        }
    }

    // ---- context menu ------------------------------------------------

    /// Open the right-click context menu for `page` at `at`, seeding its
    /// action rows (Delete, Duplicate, New child, Rename).
    fn open_context_menu(&mut self, page: PageId, at: Point) {
        self.menu_target = Some(page);
        self.empty_menu = false;
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("New child", || {}));
        menu.push(MenuItem::action("Rename", || {}));
        menu.push(MenuItem::action("Duplicate", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Delete", || {}));
        // The separator sits between "Duplicate" and "Delete"; keep the
        // action list aligned to the *action* rows only (see the index
        // remap in `apply_menu_selection`).
        menu.open_at([num::f32_of_f64(at.x), num::f32_of_f64(at.y)]);
        self.menu = menu;
        self.dirty = true;
    }

    /// Open the empty-area context menu (a single "New note" action that
    /// creates a top-level page) at `at`.
    fn open_empty_area_menu(&mut self, at: Point) {
        self.menu_target = None;
        self.empty_menu = true;
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("New note", || {}));
        menu.open_at([num::f32_of_f64(at.x), num::f32_of_f64(at.y)]);
        self.menu = menu;
        self.dirty = true;
    }

    /// Map the menu's selected item index (which counts separators) back
    /// to a [`RowAction`] and enqueue the matching command.
    fn apply_menu_selection(&mut self, item_index: usize) {
        if self.empty_menu {
            self.empty_menu = false;
            // The empty-area menu has a single "New note" action at index 0.
            if item_index == 0 {
                self.pending_events
                    .push(OutEvent::CreateChild { parent: None });
                self.rows_dirty = true;
            }
            return;
        }
        let Some(page) = self.menu_target else { return };
        // Menu rows: 0 New child, 1 Rename, 2 Duplicate, 3 Separator,
        // 4 Delete. The separator (index 3) maps to no action.
        let action = match item_index {
            0 => Some(RowAction::NewChild),
            1 => Some(RowAction::Rename),
            2 => Some(RowAction::Duplicate),
            4 => Some(RowAction::Delete),
            _ => None,
        };
        let Some(action) = action else { return };
        match action {
            RowAction::NewChild => {
                self.pending_events
                    .push(OutEvent::CreateChild { parent: Some(page) });
                // Reveal the new child by expanding the parent.
                self.expanded.insert(page);
                self.rows_dirty = true;
            }
            RowAction::Rename => {
                if let Some(idx) = self.row_index_of(page) {
                    let title = self.flat_rows[idx].title.clone();
                    self.begin_rename(page, &title);
                }
            }
            RowAction::Duplicate => {
                self.pending_events.push(OutEvent::Duplicate(page));
            }
            RowAction::Delete => {
                self.pending_events.push(OutEvent::Delete(page));
            }
        }
        self.menu_target = None;
        self.dirty = true;
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
        let height = HEADER_HEIGHT + (rows as f64) * ROW_HEIGHT + 8.0;
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(160.0),
                    height: Dimension::length(num::f32_of_f64(height)),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let new_rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let resized =
            new_rect.width() != self.rect.width() || new_rect.height() != self.rect.height();
        self.rect = new_rect;
        // Header "+" new-page button — square, pinned to the top-right
        // of the header strip.
        self.new_page_btn_rect = Rect::new(
            self.rect.x1 - NEW_BTN_MARGIN - NEW_BTN_SIZE,
            self.rect.y0 + (HEADER_HEIGHT - NEW_BTN_SIZE) / 2.0,
            self.rect.x1 - NEW_BTN_MARGIN,
            self.rect.y0 + f64::midpoint(HEADER_HEIGHT, NEW_BTN_SIZE),
        );
        // Position each row, offset below the header strip.
        self.row_rects.clear();
        for i in 0..self.flat_rows.len() {
            let y0 = self.rect.y0 + HEADER_HEIGHT + (i as f64) * ROW_HEIGHT;
            let y1 = y0 + ROW_HEIGHT;
            self.row_rects
                .push(Rect::new(self.rect.x0, y0, self.rect.x1, y1));
        }
        if resized {
            self.dirty = true;
        }
        // If a rename is active, position the embedded editor against
        // the current row rect — it follows the row across resize.
        if let Some(r) = &mut self.rename
            && let Some(idx) = self.flat_rows.iter().position(|row| row.id == r.page)
        {
            let row_rect = self.row_rects[idx];
            let glyph_x =
                row_rect.x0 + ROW_INSET_X + (self.flat_rows[idx].depth as f64) * INDENT_PX;
            let editor_rect = Rect::new(
                glyph_x + GLYPH_WIDTH,
                row_rect.y0,
                row_rect.x1 - 4.0,
                row_rect.y1,
            );
            r.input.apply_layout_external(editor_rect);
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

        // Header strip: a small "PAGES" label on the left and a "+"
        // new-page button on the right. The button is the visible
        // affordance the welcome copy ("add a page from the left")
        // points at.
        let header_text_y = self.rect.y0 + (HEADER_HEIGHT - f64::from(ROW_TEXT_SIZE)) / 2.0;
        text_renderer.draw_text(
            scene,
            "PAGES",
            ROW_TEXT_SIZE - 2.0,
            tokens.colors.text_dim,
            transform * Affine::translate((self.rect.x0 + ROW_INSET_X, header_text_y)),
            Some(num::f32_of_f64(
                (self.new_page_btn_rect.x0 - self.rect.x0 - ROW_INSET_X - 6.0).max(0.0),
            )),
        );
        // Button chip — brighter when hovered so it reads as clickable.
        let btn_bg = if self.new_page_btn_hover {
            theme.accent_primary.with_alpha_factor(0.28)
        } else {
            tokens.colors.surface_white_10
        };
        let btn = vello::kurbo::RoundedRect::from_rect(
            self.new_page_btn_rect,
            f64::from(tokens.radii.sm),
        );
        scene.fill(vello::peniko::Fill::NonZero, transform, btn_bg, None, &btn);
        // "+" glyph, centred in the button.
        let plus_size = ROW_TEXT_SIZE + 2.0;
        let (plus_w, _) = text_renderer.measure("+", plus_size, None);
        let plus_x = self.new_page_btn_rect.center().x - f64::from(plus_w) / 2.0;
        let plus_y = self.new_page_btn_rect.center().y - f64::from(plus_size) / 2.0;
        text_renderer.draw_text(
            scene,
            "+",
            plus_size,
            tokens.colors.text_primary,
            transform * Affine::translate((plus_x, plus_y)),
            None,
        );

        // Drag feedback: keep the reflow gap pinned to the cursor, then
        // resolve what a drop here would do so the rows reflow and the
        // drop indicator match the eventual commit.
        let metrics = self.list_metrics();
        let drag_active = self.drag.is_active();
        if drag_active {
            self.drag.sync_insertion(&metrics);
        }
        let from_idx = self.drag.from_index();
        let plan = if drag_active {
            self.last_cursor.map(|p| self.drop_plan(p))
        } else {
            None
        };
        let seam_mode = matches!(plan, Some(DropPlan::Seam { .. }));

        for (i, row) in self.flat_rows.iter().enumerate() {
            // `row_rects` is rebuilt in `apply_layout`; a collab-driven
            // `sync` can grow `flat_rows` a frame before the relayout
            // lands, so guard rather than index out of bounds. The row
            // appears next frame once layout catches up.
            let Some(&base_rect) = self.row_rects.get(i) else {
                break;
            };
            // The dragged row paints as the floating ghost instead of in
            // place.
            if drag_active && Some(i) == from_idx {
                continue;
            }
            // In seam mode the toolkit's reflow slides each other row to
            // open the gap at the drop target; onto / no-drop modes leave
            // rows put.
            let shift = if seam_mode {
                self.drag.item_shift(i, &metrics)
            } else {
                0.0
            };
            let row_rect = Rect::new(
                base_rect.x0,
                base_rect.y0 + shift,
                base_rect.x1,
                base_rect.y1 + shift,
            );
            if self.selected == Some(row.id) {
                // An inset rounded pill rather than an edge-to-edge bar, so
                // the selection reads as a chip with breathing room.
                let pill = RoundedRect::new(
                    row_rect.x0 + ROW_PILL_MARGIN,
                    row_rect.y0 + 1.0,
                    row_rect.x1 - ROW_PILL_MARGIN,
                    row_rect.y1 - 1.0,
                    f64::from(tokens.radii.md),
                );
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.accent_primary.with_alpha_factor(0.22),
                    None,
                    &pill,
                );
            }
            // Indicative mist: glow this row in a peer's color when that
            // peer is currently viewing the page it leads to. Only remote
            // peers appear in `peers_on` (we don't register ourselves
            // locally), so the mist marks "where everyone else is".
            let here = presence.peers_on(&PresenceKey::Tab {
                id: row.id.to_string(),
            });
            if !here.is_empty() {
                let p0 = transform * Point::new(row_rect.x0, row_rect.y0);
                let p1 = transform * Point::new(row_rect.x1, row_rect.y1);
                let mist_rect = Rect::new(p0.x, p0.y, p1.x, p1.y);
                for user in here {
                    presence.draw_peer_mist(scene, theme, user, mist_rect);
                }
            }
            let glyph_x = row_rect.x0 + ROW_INSET_X + (row.depth as f64) * INDENT_PX;
            let label_x = glyph_x + GLYPH_WIDTH;
            let text_y = row_rect.y0 + (ROW_HEIGHT - f64::from(ROW_TEXT_SIZE)) / 2.0;

            // Nesting guides: a faint vertical rail at each ancestor depth
            // so the tree structure reads at a glance — this is what makes
            // an indented child obviously nested under its parent.
            for level in 0..row.depth {
                let gx = row_rect.x0 + ROW_INSET_X + (level as f64) * INDENT_PX + GLYPH_WIDTH / 2.0;
                let guide = Rect::new(gx - 0.5, row_rect.y0, gx + 0.5, row_rect.y1);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.border_standard,
                    None,
                    &guide,
                );
            }

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
                    Some(num::f32_of_f64(row_rect.x1 - label_x - 4.0)),
                );
            }
        }

        // Drop indicator.
        match &plan {
            Some(DropPlan::Onto { row, .. }) => {
                // Re-parent: outline the target page so it reads as "drop
                // in here".
                if let Some(&rr) = self.row_rects.get(*row) {
                    let shape = RoundedRect::from_rect(rr.inset(-1.0), f64::from(tokens.radii.sm));
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        theme.accent_primary.with_alpha_factor(0.20),
                        None,
                        &shape,
                    );
                    scene.stroke(
                        &Stroke::new(1.5),
                        transform,
                        theme.accent_primary,
                        None,
                        &shape,
                    );
                }
            }
            Some(DropPlan::Seam { to, depth, .. }) => {
                // Reorder: an accent caret between rows, indented to the
                // depth the page would land at (so nesting is visible).
                let y = metrics.slot_lead(*to);
                let x0 = self.rect.x0 + ROW_INSET_X + (*depth as f64) * INDENT_PX + 6.0;
                let caret = RoundedRect::new(x0, y - 1.5, self.rect.x1 - 6.0, y + 1.5, 1.5);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.accent_primary,
                    None,
                    &caret,
                );
            }
            _ => {}
        }

        // Paint the embedded rename editor *over* the row.
        if let Some(r) = &mut self.rename {
            r.input
                .draw(scene, transform, text_renderer, theme, presence);
        }

        // Floating ghost: a half-transparent copy of the dragged row
        // following the cursor at its grab offset, drawn above the
        // reflowing list.
        if drag_active
            && let (Some(fi), Some(ghost)) = (
                from_idx,
                self.drag.ghost_rect(self.rect.x0, self.rect.x1, &metrics),
            )
            && let Some(row) = self.flat_rows.get(fi)
        {
            let shape = RoundedRect::from_rect(ghost, f64::from(tokens.radii.sm));
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.surfaces.overlay.with_alpha_factor(0.85),
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.accent_primary.with_alpha_factor(0.7),
                None,
                &shape,
            );
            let label_x = ghost.x0 + ROW_INSET_X + GLYPH_WIDTH;
            let text_y = ghost.y0 + (ROW_HEIGHT - f64::from(ROW_TEXT_SIZE)) / 2.0;
            text_renderer.draw_text(
                scene,
                &row.title,
                ROW_TEXT_SIZE,
                tokens.colors.text_primary,
                transform * Affine::translate((label_x, text_y)),
                Some(num::f32_of_f64(ghost.x1 - label_x - 4.0)),
            );
        }

        // A single hairline down the sidebar's inner edge separates the
        // page tree from the editor. This replaces the old full-rectangle
        // focus ring (a jarring blue box around the whole pane): pane focus
        // is already legible from the selected row's accent fill, so the
        // divider is all the chrome the split needs.
        let divider = Rect::new(self.rect.x1 - 1.0, self.rect.y0, self.rect.x1, self.rect.y1);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.border_standard,
            None,
            &divider,
        );

        // Context menu paints last so it sits above every row.
        self.menu
            .draw(scene, transform, text_renderer, theme, presence);
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

        // While the right-click context menu is open it captures input:
        // route the event to it, then apply whatever the user chose. It
        // dismisses itself on Escape or a click outside its rect.
        if self.menu.is_open() {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.last_cursor = Some(Point::new(
                    position.x / scale_factor,
                    position.y / scale_factor,
                ));
            }
            let mut menu_handled = false;
            let mut ft = *focus_target;
            self.menu.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                &mut menu_handled,
                &mut ft,
                node_id,
                taffy,
            );
            if let Some(idx) = self.menu.take_selection() {
                self.apply_menu_selection(idx);
            }
            if menu_handled || !self.menu.is_open() {
                self.dirty = true;
            }
            if menu_handled {
                *handled = true;
                return;
            }
        }

        // Escape aborts an in-flight drag, snapping the list back to its
        // original order (the model is only mutated on a real drop).
        if self.drag.is_pending()
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            self.drag.cancel();
            self.dirty = true;
            *handled = true;
            return;
        }

        // Commit an in-progress rename when the user presses OUTSIDE its
        // editor (click-away ends editing), then let the same press fall
        // through to select / toggle whatever row it landed on.
        if self.rename.is_some()
            && let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
        {
            let inside = self.rename.as_ref().is_some_and(|r| {
                r.input
                    .get_rect()
                    .contains(self.last_cursor.unwrap_or(Point::ZERO))
            });
            if !inside {
                self.commit_rename();
            }
        }

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
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = Some(pt);
                let hover = self.new_page_btn_rect.contains(pt);
                if hover != self.new_page_btn_hover {
                    self.new_page_btn_hover = hover;
                    self.dirty = true;
                }
                // Feed the drag controller: it promotes to a live reorder
                // past the threshold and re-pins the reflow gap to the
                // cursor. Redraw every move while a drag is latched so the
                // floating ghost tracks the pointer.
                if self.drag.is_pending() {
                    let metrics = self.list_metrics();
                    self.drag.pointer_moved(pt, &metrics);
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.drag.is_pending() {
                    let active = self.drag.is_active();
                    let dragged = self.drag.dragged_id().copied();
                    let pt = self.last_cursor.unwrap_or(Point::ZERO);
                    let plan = if active {
                        self.drop_plan(pt)
                    } else {
                        DropPlan::Cancel
                    };
                    // Clear the session first — the model is mutated only
                    // through the event we enqueue below, so a cancel is a
                    // no-op by construction.
                    self.drag.cancel();
                    if let (true, Some(id)) = (active, dragged) {
                        match plan {
                            DropPlan::Onto { page, .. } => {
                                self.pending_events.push(OutEvent::Reparent {
                                    id,
                                    new_parent: Some(page),
                                    before: None,
                                });
                                // Reveal the moved page under its new parent.
                                self.expanded.insert(page);
                                self.rows_dirty = true;
                                *handled = true;
                            }
                            DropPlan::Seam { parent, before, .. } => {
                                self.pending_events.push(OutEvent::Reparent {
                                    id,
                                    new_parent: parent,
                                    before,
                                });
                                *handled = true;
                            }
                            DropPlan::Cancel => {}
                        }
                    }
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let Some(pt) = self.last_cursor else { return };
                if !self.rect.contains(pt) {
                    return;
                }
                // Open the context menu on whichever row was right-clicked.
                for i in 0..self.flat_rows.len() {
                    let Some(&row_rect) = self.row_rects.get(i) else {
                        break;
                    };
                    if row_rect.contains(pt) {
                        let page = self.flat_rows[i].id;
                        // Right-click also selects, matching every file tree.
                        self.selected = Some(page);
                        self.pending_events.push(OutEvent::Select(page));
                        self.is_focused = true;
                        *focus_target = Some(node_id);
                        self.open_context_menu(page, pt);
                        *handled = true;
                        return;
                    }
                }
                // Right-clicked empty space in the sidebar — offer "New note".
                self.is_focused = true;
                *focus_target = Some(node_id);
                self.open_empty_area_menu(pt);
                *handled = true;
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
                // Header "+" button — create a new root-level page. This
                // is the affordance the welcome copy refers to.
                if self.new_page_btn_rect.contains(pt) {
                    self.pending_events
                        .push(OutEvent::CreateChild { parent: None });
                    self.is_focused = true;
                    *focus_target = Some(node_id);
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                for i in 0..self.flat_rows.len() {
                    let Some(&row_rect) = self.row_rects.get(i) else {
                        break;
                    };
                    if !row_rect.contains(pt) {
                        continue;
                    }
                    let row_id = self.flat_rows[i].id;
                    let depth = self.flat_rows[i].depth;
                    let has_children = self.flat_rows[i].has_children;
                    let glyph_x0 = row_rect.x0 + ROW_INSET_X + (depth as f64) * INDENT_PX;
                    let glyph_x1 = glyph_x0 + GLYPH_WIDTH;
                    if has_children && pt.x >= glyph_x0 && pt.x < glyph_x1 {
                        if !self.expanded.insert(row_id) {
                            self.expanded.remove(&row_id);
                        }
                        // The visible-row set changed but we have no
                        // Workspace here to re-flatten — signal the parent
                        // to re-sync so the children show / hide.
                        self.rows_dirty = true;
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
                    // Double-click on the label → enter rename mode.
                    let now = std::time::Instant::now();
                    let is_double = self.last_click.as_ref().is_some_and(|c| {
                        c.page == row_id
                            && now.duration_since(c.at).as_millis() < DOUBLE_CLICK_INTERVAL_MS
                            && (c.pos.distance(pt)) < DOUBLE_CLICK_RADIUS_PX
                    });
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
                    // Arm a drag on this row. It only promotes to a live
                    // reorder once the pointer clears the threshold; a
                    // release without movement stays a plain click (the
                    // select above already happened).
                    let metrics = self.list_metrics();
                    let grab = pt.y - metrics.slot_lead(i);
                    self.drag.begin(row_id, i, grab, pt);
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
                // Alt-↑ / Alt-↓ — reorder the selected page among its
                // siblings. Checked before the plain arrow navigation
                // below so the modifier picks reorder over move-selection.
                if mods.contains(ModifiersState::ALT)
                    && let Some(sel) = self.selected
                {
                    match logical_key {
                        Key::Named(NamedKey::ArrowUp) => {
                            self.pending_events
                                .push(OutEvent::Move { id: sel, delta: -1 });
                            *handled = true;
                            return;
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            self.pending_events
                                .push(OutEvent::Move { id: sel, delta: 1 });
                            *handled = true;
                            return;
                        }
                        _ => {}
                    }
                }
                match logical_key {
                    // Delete removes the selected page. Backspace is
                    // deliberately NOT bound here: the editor and the page
                    // tree can both hold focus at once (selecting a page
                    // focuses the tree AND the editor), so binding Backspace
                    // to "delete page" meant a Backspace at the very start of
                    // a note deleted the note and jumped the user to another
                    // one. Backspace belongs to the editor (merge / no-op at
                    // the start of the first block).
                    Key::Named(NamedKey::Delete) => {
                        if let Some(sel) = self.selected {
                            self.pending_events.push(OutEvent::Delete(sel));
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        if let Some(idx) = self.selected.and_then(|s| self.row_index_of(s)) {
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
                        if let Some(idx) = self.selected.and_then(|s| self.row_index_of(s)) {
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
                        if let Some(sel) = self.selected
                            && let Some(idx) = self.row_index_of(sel)
                        {
                            let title = self.flat_rows[idx].title.clone();
                            self.begin_rename(sel, &title);
                            *handled = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
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
        // Losing focus commits any in-progress rename (focus-out ends
        // editing), matching every inline-rename field. Escape still
        // cancels explicitly before focus is dropped.
        if !focused && self.rename.is_some() {
            self.commit_rename();
        }
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    /// A latched drag (or an open context menu) wants to keep receiving
    /// pointer events even when the cursor leaves the sidebar rect, so the
    /// ghost keeps tracking and the drop still resolves.
    fn wants_pointer_capture(&self) -> bool {
        self.drag.wants_capture() || self.menu.is_open()
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

    /// Whether the right-click context menu is currently open. Read by
    /// the parent app so it can route keyboard nav to the menu (and away
    /// from the page list / editor) while it's up.
    pub fn menu_is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Used by tests + the parent: how many rows the tree exposes
    /// right now.
    #[cfg(test)]
    pub fn visible_row_count(&self) -> usize {
        self.flat_rows.len()
    }

    /// Test hook: the centre point of laid-out row `idx`, for click
    /// targeting through the real event path.
    #[doc(hidden)]
    pub fn row_center_for_test(&self, idx: usize) -> Option<Point> {
        self.row_rects.get(idx).map(vello::kurbo::Rect::center)
    }

    /// Test hook: the x of the disclosure-triangle hit centre for row `idx`,
    /// so tests click the real glyph instead of a hard-coded pixel that the
    /// row inset would shift out from under.
    #[doc(hidden)]
    pub fn row_disclosure_x_for_test(&self, idx: usize) -> Option<f64> {
        let row = self.row_rects.get(idx)?;
        let depth = self.flat_rows.get(idx)?.depth;
        Some(row.x0 + ROW_INSET_X + num::f64_of_usize(depth) * INDENT_PX + GLYPH_WIDTH / 2.0)
    }

    /// Test hook: the tree depth of visible row `idx` (0 = root), so a test
    /// can prove a child renders indented under its parent.
    #[doc(hidden)]
    pub fn row_depth_for_test(&self, idx: usize) -> Option<usize> {
        self.flat_rows.get(idx).map(|r| r.depth)
    }

    /// Test hook: the title of visible row `idx`, so a test can prove a
    /// rename propagated into the sidebar.
    #[doc(hidden)]
    pub fn row_title_for_test(&self, idx: usize) -> Option<&str> {
        self.flat_rows.get(idx).map(|r| r.title.as_str())
    }

    /// Test hook: whether visible row `idx` has children (draws a
    /// disclosure triangle).
    #[doc(hidden)]
    pub fn row_has_children_for_test(&self, idx: usize) -> Option<bool> {
        self.flat_rows.get(idx).map(|r| r.has_children)
    }

    /// Test hook: whether the active inline rename field renders in plain
    /// chrome (no form-field box), i.e. styled like the row rather than a
    /// jarring different box. `None` when no rename is in progress.
    #[doc(hidden)]
    pub fn rename_is_plain_for_test(&self) -> Option<bool> {
        self.rename.as_ref().map(|r| r.input.is_plain())
    }

    /// Test hook: the centre of the header "+" new-page button, for
    /// click targeting through the real event path.
    #[doc(hidden)]
    pub fn new_page_btn_center_for_test(&self) -> Point {
        self.new_page_btn_rect.center()
    }

    /// Test hook: whether a live drag (past the threshold) is in flight.
    #[doc(hidden)]
    pub fn drag_active_for_test(&self) -> bool {
        self.drag.is_active()
    }

    /// Test hook: the live reflow shift the toolkit computes for row
    /// `idx` this frame — non-zero means the list is visibly reflowing to
    /// open the drop gap.
    #[doc(hidden)]
    pub fn row_shift_for_test(&self, idx: usize) -> f64 {
        self.drag.item_shift(idx, &self.list_metrics())
    }

    /// Test hook: whether the right-click context menu is open.
    #[doc(hidden)]
    pub fn context_menu_open_for_test(&self) -> bool {
        self.menu.is_open()
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
        // Branches with children auto-expand on first render, so the full
        // nesting is on screen (root -> child -> grandchild).
        assert_eq!(tree.visible_row_count(), 3);
        // Collapsing the root hides all its descendants (only the root remains).
        tree.expanded.remove(&root);
        tree.sync_from_workspace(&ws);
        assert_eq!(tree.visible_row_count(), 1);
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
