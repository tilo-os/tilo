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
//!   - ← / → to collapse / expand branches (or move to parent / first child),
//!   - Tab / Shift-Tab to indent / outdent the selected page,
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
use ui::chrome::GLOW_ALPHA;
use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::surface::{edge_on, edge_outline};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::colors::Colors;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::drag::{DragController, ListMetrics};
use ui::widget::{Chip, ChipLead, ChipStyle, TextInput};

use crate::model::{PageId, Workspace};
use crate::num;

/// Per-row height. Comfortable rather than cramped — a page row breathes
/// with vertical air above and below its label instead of butting rows
/// edge to edge.
const ROW_HEIGHT: f64 = 30.0;
/// Height of the sidebar header strip that hosts the "Notes" label and
/// the "+" new-page button. Rows start below it.
const HEADER_HEIGHT: f64 = 42.0;
/// Side length of the square "+" new-page button in the header.
const NEW_BTN_SIZE: f64 = 30.0;
/// Right/vertical inset of the "+" button inside the header strip.
const NEW_BTN_MARGIN: f64 = 8.0;
/// Compact row actions appear only while a note is hovered. Keeping them a
/// little smaller than the row preserves label space without making the hit
/// targets fiddly.
const ROW_ACTION_SIZE: f64 = 26.0;
const ROW_ACTION_GAP: f64 = ui::tokens::Spacing::BASE.s1 as f64 / 2.0;
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
const DOUBLE_CLICK_RADIUS_PX: f64 = ui::tokens::Spacing::BASE.s1 as f64;
/// Default text size for tree row labels.
const ROW_TEXT_SIZE: f32 = 14.0;
/// Deliberate dwell before a collapsed branch springs open during a drag.
/// Long enough to cross a row without disruption, short enough to browse a
/// deep destination without dropping and restarting the gesture.
const DRAG_EXPAND_DWELL: std::time::Duration = std::time::Duration::from_millis(450);

/// Height of one line of `size`-pt text at `text_scale` — the value the
/// renderer measures for a single line — so labels centre on their line box
/// without measuring each row's own title (a fixed probe, once per row
/// height). The crate's one copy of the line formula.
fn line_h(size: f32, text_scale: f64) -> f64 {
    ui::text_metrics::line_height_px(size, num::f32_of_f64(text_scale))
}

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
    CopyLink,
    ExportMarkdown,
    InsertImage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTransfer {
    ImportMarkdown,
    CopyLink(PageId),
    ExportMarkdown(PageId),
    InsertImage(PageId),
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independently-toggled UI/dirty-tracking flag (focus, hover, two unrelated dirty producers, menu kind, one-time init gate) with no shared sub-concept; grouping them would only relocate the count, not reduce it"
)]
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
    tooltips_suppressed: bool,
    /// Currently active double-click detection state.
    last_click: Option<ClickRecord>,
    rename: Option<RenameState>,
    /// Outbound commands the parent should drain via [`take_events`].
    pending_events: Vec<OutEvent>,
    pending_transfer: Option<SidebarTransfer>,
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
    drag_hover: Option<(PageId, std::time::Instant)>,
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
    /// Cached from the latest theme during layout. `TextRenderer` applies this
    /// scale to glyphs, so every hit row must grow in lockstep to prevent the
    /// large-text preference from making labels collide.
    text_scale: f64,
    /// Vertical viewport offset for large page trees.
    scroll_y: f64,
    max_scroll_y: f64,
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

/// The tier a chosen control lifts to off a `surfaces.raised` ground — the
/// sidebar, the narrow top bar, the History page tree: whichever of the
/// theme's semantic tiers moves furthest from `raised`, so the selection is a
/// contrast step on every preset (HIG §9.3) instead of raised-on-raised with
/// only an outline. `chrome::selection_tier` lifts base → raised and would
/// land on the ground itself here.
pub(crate) fn selection_fill_on_raised(theme: &Theme) -> vello::peniko::Color {
    use ui::a11y::relative_luminance;
    let c = &theme.tokens.colors;
    let g = relative_luminance(c.surfaces.raised);
    [c.surfaces.base, c.lift(1.0)]
        .into_iter()
        .max_by(|a, b| {
            (relative_luminance(*a) - g)
                .abs()
                .total_cmp(&(relative_luminance(*b) - g).abs())
        })
        .unwrap_or(c.surfaces.base)
}

impl NoteTreeView {
    fn scaled(&self, value: f64) -> f64 {
        value * self.text_scale
    }

    fn row_height(&self) -> f64 {
        ROW_HEIGHT * self.text_scale
    }

    fn row_action_rects(&self, row_rect: Rect) -> (Rect, Rect) {
        let size = self.scaled(ROW_ACTION_SIZE);
        let gap = self.scaled(ROW_ACTION_GAP);
        let right = row_rect.x1 - self.scaled(ROW_PILL_MARGIN + 2.0);
        let top = row_rect.y0 + (row_rect.height() - size) / 2.0;
        let more = Rect::new(right - size, top, right, top + size);
        let child = Rect::new(more.x0 - gap - size, top, more.x0 - gap, top + size);
        (child, more)
    }

    fn selection_pill(row: Rect, margin: f64, tokens: &ui::tokens::Tokens) -> Rect {
        let half_gap = f64::from(tokens.spacing.s1) / 2.0;
        Rect::new(
            row.x0 + margin,
            row.y0 + half_gap,
            row.x1 - margin,
            row.y1 - half_gap,
        )
    }

    fn scroll_by(&mut self, delta: f64) -> bool {
        let before = self.scroll_y;
        self.scroll_y = (self.scroll_y + delta).clamp(0.0, self.max_scroll_y);
        let changed = (self.scroll_y - before).abs() > f64::EPSILON;
        if changed {
            self.dirty = true;
        }
        changed
    }

    fn reveal_row(&mut self, index: usize) {
        let viewport_height = (self.rect.height() - self.header_height()).max(self.row_height());
        let top = num::f64_of_usize(index) * self.row_height();
        let bottom = top + self.row_height();
        if top < self.scroll_y {
            self.scroll_y = top.clamp(0.0, self.max_scroll_y);
            self.dirty = true;
        } else if bottom > self.scroll_y + viewport_height {
            self.scroll_y = (bottom - viewport_height).clamp(0.0, self.max_scroll_y);
            self.dirty = true;
        }
    }

    fn header_height(&self) -> f64 {
        HEADER_HEIGHT * self.text_scale
    }

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
            tooltips_suppressed: false,
            last_click: None,
            rename: None,
            pending_events: Vec::new(),
            pending_transfer: None,
            rows_dirty: false,
            dirty: true,
            drag: DragController::vertical(),
            drag_hover: None,
            menu: ContextMenu::new(),
            menu_target: None,
            empty_menu: false,
            did_initial_expand: false,
            last_selection: None,
            text_scale: 1.0,
            scroll_y: 0.0,
            max_scroll_y: 0.0,
        }
    }

    /// Drain any outbound commands accumulated since the last call.
    /// Called by `NotesApp` once per tick after `handle_event`.
    pub fn take_events(&mut self) -> Vec<OutEvent> {
        std::mem::take(&mut self.pending_events)
    }

    pub fn take_transfer(&mut self) -> Option<SidebarTransfer> {
        self.pending_transfer.take()
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
        let Some(page) = ws.get(id) else {
            return;
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

    pub fn expanded_pages(&self) -> Vec<PageId> {
        let mut ids = self.expanded.iter().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    pub fn restore_expanded_pages(&mut self, ids: &[PageId], workspace: &Workspace) {
        self.expanded = ids
            .iter()
            .copied()
            .filter(|id| workspace.get(*id).is_some())
            .collect();
        self.did_initial_expand = true;
        // Re-run the existing ancestor revelation even for an unchanged selection.
        self.last_selection = None;
        self.sync_from_workspace(workspace);
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
            self.rect.y0 + self.header_height() - self.scroll_y,
            self.row_height(),
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
        let list_top = self.rect.y0 + self.header_height();
        if pt.y < list_top || pt.y > self.rect.y1 {
            return None;
        }
        let rel = pt.y - list_top + self.scroll_y;
        if rel < 0.0 {
            return None;
        }
        let raw = num::usize_of_f64((rel / self.row_height()).floor());
        if raw >= n {
            return None;
        }
        let frac = (rel / self.row_height()) - num::f64_of_usize(raw);
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
        let Some(dragged) = self.flat_rows.get(from).map(|r| r.id) else {
            return false;
        };
        let Some(row_id) = self.flat_rows.get(row).map(|r| r.id) else {
            return false;
        };
        let mut cur = Some(row_id);
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
        let list_top = self.rect.y0 + self.header_height();
        let list_bottom = self.rect.y1;
        if pt.y < list_top - self.row_height() || pt.y > list_bottom + self.row_height() {
            return DropPlan::Cancel;
        }
        if let Some(row) = self.onto_target(pt) {
            let Some(entry) = self.flat_rows.get(row) else {
                return DropPlan::Cancel;
            };
            return DropPlan::Onto {
                row,
                page: entry.id,
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
        let mut menu = note_context_menu();
        // Selection uses the shared menu's item indices, including separators
        // (see `apply_menu_selection`).
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
        menu.push(MenuItem::icon_action(Icon::FilePlus, "New note", || {}));
        menu.push(MenuItem::icon_action(
            Icon::CloudDownload,
            "Import Markdown",
            || {},
        ));
        menu.open_at([num::f32_of_f64(at.x), num::f32_of_f64(at.y)]);
        self.menu = menu;
        self.dirty = true;
    }

    /// Map the menu's selected item index (which counts separators) back
    /// to a [`RowAction`] and enqueue the matching command.
    fn apply_menu_selection(&mut self, item_index: usize) {
        if self.empty_menu {
            self.empty_menu = false;
            match item_index {
                0 => {
                    self.pending_events
                        .push(OutEvent::CreateChild { parent: None });
                    self.rows_dirty = true;
                }
                1 => self.pending_transfer = Some(SidebarTransfer::ImportMarkdown),
                _ => {}
            }
            return;
        }
        let Some(page) = self.menu_target else { return };
        // Menu rows: 0 New child, 1 Rename, 2 Duplicate, 3 Copy link,
        // 4 Export, 5 Insert image, 6 Separator, 7 Delete.
        let action = match item_index {
            0 => Some(RowAction::NewChild),
            1 => Some(RowAction::Rename),
            2 => Some(RowAction::Duplicate),
            3 => Some(RowAction::CopyLink),
            4 => Some(RowAction::ExportMarkdown),
            5 => Some(RowAction::InsertImage),
            7 => Some(RowAction::Delete),
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
                if let Some(idx) = self.row_index_of(page)
                    && let Some(row) = self.flat_rows.get(idx)
                {
                    let title = row.title.clone();
                    self.begin_rename(page, &title);
                }
            }
            RowAction::Duplicate => {
                self.pending_events.push(OutEvent::Duplicate(page));
            }
            RowAction::Delete => {
                self.pending_events.push(OutEvent::Delete(page));
            }
            RowAction::CopyLink => {
                self.pending_transfer = Some(SidebarTransfer::CopyLink(page));
            }
            RowAction::ExportMarkdown => {
                self.pending_transfer = Some(SidebarTransfer::ExportMarkdown(page));
            }
            RowAction::InsertImage => {
                self.pending_transfer = Some(SidebarTransfer::InsertImage(page));
            }
        }
        self.menu_target = None;
        self.dirty = true;
    }
}

pub(crate) fn note_context_menu() -> ContextMenu {
    let mut menu = ContextMenu::new();
    menu.push(MenuItem::icon_action(
        Icon::FilePlus,
        ui::i18n!("notes-menu-new-child"),
        || {},
    ));
    menu.push(MenuItem::icon_action(
        Icon::Pencil,
        ui::i18n!("notes-menu-rename"),
        || {},
    ));
    menu.push(MenuItem::icon_action(
        Icon::Copy,
        ui::i18n!("notes-menu-duplicate"),
        || {},
    ));
    menu.push(MenuItem::icon_action(
        Icon::ExternalLink,
        ui::i18n!("notes-menu-copy-link"),
        || {},
    ));
    menu.push(MenuItem::icon_action(
        Icon::Download,
        ui::i18n!("notes-menu-export"),
        || {},
    ));
    menu.push(MenuItem::icon_action(
        Icon::Image,
        ui::i18n!("notes-menu-insert-image"),
        || {},
    ));
    menu.push(MenuItem::separator());
    menu.push(MenuItem::icon_action(
        Icon::Trash2,
        ui::i18n!("notes-menu-delete"),
        || {},
    ));
    // Keep these row indices aligned with both menu selection dispatchers.
    menu
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
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale);
        // `new_leaf` only errors for a corrupted tree; `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so surfacing a
        // theoretical error here would mean threading a `Result` through the
        // whole toolkit for a branch that can't be taken.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh style cannot fail")]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(160.0),
                    height: Dimension::length(num::f32_of_f64(
                        self.header_height() + self.row_height(),
                    )),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let layout = taffy.layout(node).unwrap();
        let new_rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let resized = (new_rect.width() - self.rect.width()).abs() > f64::EPSILON
            || (new_rect.height() - self.rect.height()).abs() > f64::EPSILON;
        self.rect = new_rect;
        let list_height = (self.rect.height() - self.header_height()).max(0.0);
        let content_height = num::f64_of_usize(self.flat_rows.len()) * self.row_height();
        self.max_scroll_y = (content_height - list_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y);
        // Header "+" new-page button — square, pinned to the top-right
        // of the header strip.
        self.new_page_btn_rect = Rect::new(
            self.rect.x1 - self.scaled(NEW_BTN_MARGIN) - self.scaled(NEW_BTN_SIZE),
            self.rect.y0 + (self.header_height() - self.scaled(NEW_BTN_SIZE)) / 2.0,
            self.rect.x1 - self.scaled(NEW_BTN_MARGIN),
            self.rect.y0 + f64::midpoint(self.header_height(), self.scaled(NEW_BTN_SIZE)),
        );
        // Position each row, offset below the header strip.
        self.row_rects.clear();
        for i in 0..self.flat_rows.len() {
            let y0 = self.rect.y0 + self.header_height() + (i as f64) * self.row_height()
                - self.scroll_y;
            let y1 = y0 + self.row_height();
            self.row_rects
                .push(Rect::new(self.rect.x0, y0, self.rect.x1, y1));
        }
        if resized {
            self.dirty = true;
        }
        // If a rename is active, position the embedded editor against
        // the current row rect — it follows the row across resize.
        let scale = self.text_scale;
        if let Some(r) = &mut self.rename
            && let Some(idx) = self.flat_rows.iter().position(|row| row.id == r.page)
            && let Some(row_rect) = self.row_rects.get(idx).copied()
            && let Some(depth) = self.flat_rows.get(idx).map(|row| row.depth)
        {
            let glyph_x = row_rect.x0 + ROW_INSET_X * scale + (depth as f64) * INDENT_PX * scale;
            let editor_rect = Rect::new(
                glyph_x + GLYPH_WIDTH * scale,
                row_rect.y0,
                row_rect.x1 - 4.0 * scale,
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

        self.draw_header(scene, transform, text_renderer, theme);

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

        let list_clip = Rect::new(
            self.rect.x0,
            self.rect.y0 + self.header_height(),
            self.rect.x1,
            self.rect.y1,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &list_clip,
        );
        self.draw_rows(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            &metrics,
            drag_active,
            from_idx,
            seam_mode,
        );
        self.draw_drop_indicator(scene, transform, theme, &metrics, plan.as_ref());

        // Paint the embedded rename editor *over* the row.
        if let Some(r) = &mut self.rename {
            r.input
                .draw(scene, transform, text_renderer, theme, presence);
        }

        self.draw_drag_ghost(
            scene,
            transform,
            text_renderer,
            theme,
            &metrics,
            drag_active,
            from_idx,
        );
        scene.pop_layer();

        if self.max_scroll_y > 0.0 {
            let track_top = self.rect.y0 + self.header_height() + self.scaled(4.0);
            let track_bottom = self.rect.y1 - self.scaled(4.0);
            let track_height = (track_bottom - track_top).max(1.0);
            let content_height = num::f64_of_usize(self.flat_rows.len()) * self.row_height();
            let viewport_height = (self.rect.height() - self.header_height()).max(1.0);
            let thumb_height = (track_height * viewport_height / content_height)
                .clamp(self.scaled(24.0), track_height);
            let travel = (track_height - thumb_height).max(0.0);
            let fraction = (self.scroll_y / self.max_scroll_y).clamp(0.0, 1.0);
            let y0 = track_top + travel * fraction;
            let thumb = RoundedRect::new(
                self.rect.x1 - self.scaled(5.0),
                y0,
                self.rect.x1 - self.scaled(2.0),
                y0 + thumb_height,
                self.scaled(1.5),
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.text_dim.with_alpha_factor(0.65),
                None,
                &thumb,
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
            edge_on(tokens.colors.surfaces.raised),
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

        if self.handle_menu_and_drag_escape(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target.as_ref(),
            node_id,
            taffy,
        ) {
            return;
        }
        if self.handle_rename_routing(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        ) {
            return;
        }

        match event {
            WindowEvent::CursorLeft { .. } => {
                self.last_cursor = None;
                self.new_page_btn_hover = false;
                self.dirty = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.handle_cursor_moved(pt);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_drag_release(handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.handle_right_click(focus_target, node_id, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(focus_target, node_id, handled);
            }
            WindowEvent::MouseWheel { delta, .. }
                if self
                    .last_cursor
                    .is_some_and(|point| self.rect.contains(point)) =>
            {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => {
                        -f64::from(*y) * self.row_height() * 2.0
                    }
                    winit::event::MouseScrollDelta::PixelDelta(position) => -position.y,
                };
                if self.scroll_by(dy) {
                    *handled = true;
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
            } => {
                let mods = modifiers.state();
                self.handle_key_press(logical_key, mods, handled);
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

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut children = Vec::with_capacity(self.flat_rows.len().saturating_add(1));
        if self.new_page_btn_rect.area() > 0.0 {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("New note")
                    .with_description("Create a top-level note")
                    .with_state(AccessState::new().focusable(true))
                    .with_key(0)
                    .with_action_target("notes:new-page")
                    .with_bounds(self.new_page_btn_rect),
            );
        }
        let count = self.flat_rows.len();
        for (index, row) in self.flat_rows.iter().enumerate() {
            let Some(rect) = self.row_rects.get(index).copied() else {
                break;
            };
            if rect.area() <= 0.0 {
                continue;
            }
            if let Some(rename) = self.rename.as_ref().filter(|rename| rename.page == row.id)
                && let Some(mut input) = Widget::accessibility(&rename.input)
            {
                input.name = Some(ui::i18n!("notes-a11y-rename"));
                input.action_target = Some(format!("notes:rename:{}", row.id));
                input.bounds = Some(rename.input.get_rect().into());
                children.push(input);
                continue;
            }
            let mut state = AccessState::new()
                .focusable(true)
                .selected(self.selected == Some(row.id));
            state.expanded = row.has_children.then(|| self.expanded.contains(&row.id));
            let (child_action, more_action) = self.row_action_rects(rect);
            children.push(
                AccessNode::new(Role::TreeItem)
                    .with_name(row.title.clone())
                    .with_state(state)
                    .with_position(index.saturating_add(1), count)
                    .with_level(row.depth.saturating_add(1))
                    .with_hashed_key(row.id)
                    .with_action_target(format!("notes:page:{}", row.id))
                    .with_bounds(rect)
                    .with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name(format!(
                                "{} {}",
                                ui::i18n!("notes-a11y-new-child-under"),
                                row.title
                            ))
                            .with_state(AccessState::new().focusable(true))
                            .with_action_target(format!("notes:new-child:{}", row.id))
                            .with_bounds(child_action),
                        AccessNode::new(Role::Button)
                            .with_name(format!(
                                "{} {}",
                                ui::i18n!("notes-a11y-more-actions-for"),
                                row.title
                            ))
                            .with_state(AccessState::new().focusable(true))
                            .with_action_target(format!("notes:actions:{}", row.id))
                            .with_bounds(more_action),
                    ]),
            );
        }
        if self.menu.is_open()
            && let Some(mut menu) = Widget::accessibility(&self.menu)
        {
            fn namespace(node: &mut AccessNode) {
                if let Some(target) = node.action_target.take() {
                    node.action_target = Some(format!("notes.sidebar-menu:{target}"));
                }
                for child in &mut node.children {
                    namespace(child);
                }
            }
            namespace(&mut menu);
            children.push(menu);
        }
        Some(
            AccessNode::new(Role::Tree)
                .with_name(ui::i18n!("notes-title"))
                .with_state(AccessState::new().focusable(true).focused(self.is_focused))
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        let has_new_page = self.new_page_btn_rect.area() > 0.0;
        if *index == 0 && has_new_page {
            return Some(self.new_page_btn_rect);
        }
        // The new-note affordance is the first synthetic child whenever it
        // has geometry. The visible rows follow in the same ordinal order as
        // `accessibility`, so AT activation and pointer hit testing agree.
        let row_index = if has_new_page {
            index.saturating_sub(1)
        } else {
            *index
        };
        self.row_rects.get(row_index).copied()
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        self.is_focused = true;
        let has_new_page = self.new_page_btn_rect.area() > 0.0;
        let row_index = if has_new_page {
            index.saturating_sub(1)
        } else {
            *index
        };
        if (has_new_page || *index > 0)
            && let Some(row) = self.flat_rows.get(row_index)
        {
            self.selected = Some(row.id);
        }
        self.dirty = true;
        true
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if matches!(action, ui::a11y::Action::Activate)
            && let Some(page) = target
                .strip_prefix("notes:new-child:")
                .and_then(|page| page.parse::<PageId>().ok())
            && self.flat_rows.iter().any(|row| row.id == page)
        {
            self.pending_events
                .push(OutEvent::CreateChild { parent: Some(page) });
            self.dirty = true;
            return true;
        }
        if matches!(action, ui::a11y::Action::Activate)
            && let Some(page) = target
                .strip_prefix("notes:actions:")
                .and_then(|page| page.parse::<PageId>().ok())
            && let Some(index) = self.row_index_of(page)
            && let Some(rect) = self.row_rects.get(index).copied()
        {
            self.selected = Some(page);
            self.pending_events.push(OutEvent::Select(page));
            let (_, more) = self.row_action_rects(rect);
            self.open_context_menu(page, Point::new(more.x0, more.y1));
            return true;
        }
        if let Some(target) = target.strip_prefix("notes.sidebar-menu:") {
            let handled = self
                .menu
                .perform_semantic_action(target, action, state, path);
            if handled {
                if let Some(index) = self.menu.take_selection() {
                    self.apply_menu_selection(index);
                }
                self.dirty = true;
            }
            return handled;
        }
        if let Some(page) = target.strip_prefix("notes:rename:")
            && self
                .rename
                .as_ref()
                .is_some_and(|rename| rename.page.to_string() == page)
            && let Some(rename) = self.rename.as_mut()
        {
            let handled = rename.input.perform_action(action, state, path);
            self.dirty |= handled;
            return handled;
        }
        false
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.rename.is_some() || self.menu.is_open() {
            return None;
        }
        (self.new_page_btn_rect.contains(point) || self.hovered_row(point).is_some())
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        if self.tooltips_suppressed
            || self.menu.is_open()
            || self.rename.is_some()
            || self.drag.is_pending()
        {
            return None;
        }
        if self.new_page_btn_rect.contains(point) {
            return Some((
                self.new_page_btn_rect,
                ui::i18n!("notes-affordance-new-note"),
            ));
        }
        let index = self.hovered_row(point)?;
        let row = self.flat_rows.get(index)?;
        let row_rect = *self.row_rects.get(index)?;
        let (child_action, more_action) = self.row_action_rects(row_rect);
        if child_action.contains(point) {
            return Some((child_action, ui::i18n!("notes-affordance-new-child")));
        }
        if more_action.contains(point) {
            return Some((more_action, ui::i18n!("notes-affordance-note-actions")));
        }
        let hint = ui::i18n!("notes-affordance-note-row");
        Some((row_rect, format!("{}\n{}", row.title, hint)))
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
    /// The "Notes" label + "+" new-page button header strip.
    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        // Centre the label on the line the renderer measures for it, not on
        // the font size: a line is `size × 1.25` tall, so centring on the bare
        // size sat the label 1.75px low at 1× and further at larger scales.
        let label_h = line_h(ROW_TEXT_SIZE, self.text_scale);
        let header_text_y = self.rect.y0 + (self.header_height() - label_h) / 2.0;
        text_renderer.draw_text(
            scene,
            &ui::i18n!("notes-title"),
            ROW_TEXT_SIZE,
            tokens.colors.text_dim,
            transform * Affine::translate((self.rect.x0 + self.scaled(ROW_INSET_X), header_text_y)),
            Some(num::f32_of_f64(
                (self.new_page_btn_rect.x0
                    - self.rect.x0
                    - self.scaled(ROW_INSET_X)
                    - self.scaled(6.0))
                .max(0.0),
            )),
        );
        // The new-page control is the shared icon chip — a glyph lead with no
        // label, painted into exactly the rect `apply_layout` hit-tests — so
        // it carries the toolkit's states and theming instead of a local
        // square with a typeset "+". Hover takes the accent tint.
        let style = if self.new_page_btn_hover {
            ChipStyle::tinted(theme.accent_primary)
        } else {
            ChipStyle::quiet()
        };
        Chip::draw_at(
            scene,
            transform,
            self.new_page_btn_rect,
            "",
            ChipLead::Glyph(Icon::Plus),
            style,
            theme,
            text_renderer,
        );
    }

    /// Every visible row: selection pill, presence mist, nesting guides,
    /// expand glyph and label.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        metrics: &ListMetrics,
        drag_active: bool,
        from_idx: Option<usize>,
        seam_mode: bool,
    ) {
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
                self.drag.item_shift(i, metrics)
            } else {
                0.0
            };
            let row_rect = Rect::new(
                base_rect.x0,
                base_rect.y0 + shift,
                base_rect.x1,
                base_rect.y1 + shift,
            );
            self.draw_one_row(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                row,
                row_rect,
            );
        }
    }

    /// One row's nesting guides, selection pill, presence mist, expand
    /// glyph and label.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one row's layered visuals share geometry and interaction state"
    )]
    fn draw_one_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        row: &RowEntry,
        row_rect: Rect,
    ) {
        let tokens = &theme.tokens;
        let hovered = !self.drag.is_pending()
            && !self.menu.is_open()
            && self.rename.is_none()
            && self
                .last_cursor
                .is_some_and(|point| row_rect.contains(point));
        for level in 0..row.depth {
            let gx = row_rect.x0
                + self.scaled(ROW_INSET_X)
                + (level as f64) * self.scaled(INDENT_PX)
                + self.scaled(GLYPH_WIDTH) / 2.0;
            let guide = Rect::new(
                gx - self.scaled(0.5),
                row_rect.y0,
                gx + self.scaled(0.5),
                row_rect.y1,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                edge_on(tokens.colors.surfaces.raised),
                None,
                &guide,
            );
        }
        if self.selected == Some(row.id) {
            // Selection is a lift (HIG §9.3): the toolkit's selection tier —
            // fill, derived hairline and inner glow — on a pill inset from
            // the row edges, so a selected row and its neighbour's pill keep
            // an `s1` gap rather than touching.
            let pill_rect = Self::selection_pill(row_rect, self.scaled(ROW_PILL_MARGIN), tokens);
            let radius = f64::from(tokens.radii.md);
            // The sidebar already sits on `surfaces.raised`, so the lift goes
            // the other way — onto the tier that moves furthest from it —
            // rather than onto itself.
            let lifted = selection_fill_on_raised(theme);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                lifted,
                None,
                &RoundedRect::from_rect(pill_rect, radius),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                edge_on(lifted),
                None,
                &edge_outline(pill_rect, radius),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                Colors::hairline_on(lifted, GLOW_ALPHA),
                None,
                &edge_outline(pill_rect.inset(-1.0), (radius - 1.0).max(0.0)),
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
        let glyph_x =
            row_rect.x0 + self.scaled(ROW_INSET_X) + (row.depth as f64) * self.scaled(INDENT_PX);
        let label_x = glyph_x + self.scaled(GLYPH_WIDTH);
        // Centre on the measured line box, not the font size (see `draw_header`).
        let text_y =
            row_rect.y0 + (self.row_height() - line_h(ROW_TEXT_SIZE, self.text_scale)) / 2.0;

        if row.has_children {
            let icon = if self.expanded.contains(&row.id) {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            let size = self.scaled(14.0);
            icon.draw(
                scene,
                transform,
                Rect::from_center_size(
                    Point::new(
                        glyph_x + self.scaled(GLYPH_WIDTH) / 2.0,
                        row_rect.center().y,
                    ),
                    (size, size),
                ),
                tokens.colors.text_dim,
            );
        }

        // Skip the label when the row is being renamed — the
        // embedded editor paints in its place.
        let is_renaming = matches!(&self.rename, Some(r) if r.page == row.id);
        if !is_renaming {
            let (child_action, _) = self.row_action_rects(row_rect);
            let label_right = if hovered {
                child_action.x0 - self.scaled(4.0)
            } else {
                row_rect.x1 - self.scaled(4.0)
            };
            text_renderer.draw_text(
                scene,
                &row.title,
                ROW_TEXT_SIZE,
                tokens.colors.text_primary,
                transform * Affine::translate((label_x, text_y)),
                Some(num::f32_of_f64((label_right - label_x).max(0.0))),
            );
        }
        if hovered {
            let (child_action, more_action) = self.row_action_rects(row_rect);
            for (rect, icon) in [
                (child_action, Icon::Plus),
                (more_action, Icon::MoreHorizontal),
            ] {
                let active = self.last_cursor.is_some_and(|point| rect.contains(point));
                if active {
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        tokens.colors.lift(0.58),
                        None,
                        &RoundedRect::from_rect(rect, f64::from(tokens.radii.sm)),
                    );
                }
                icon.draw(
                    scene,
                    transform,
                    rect.inset(-self.scaled(4.0)),
                    if active {
                        tokens.colors.text_primary
                    } else {
                        tokens.colors.text_dim
                    },
                );
            }
        }
    }

    /// The re-parent outline (`Onto`) or reorder caret (`Seam`) that shows
    /// what the current drag would do on drop.
    fn draw_drop_indicator(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        metrics: &ListMetrics,
        plan: Option<&DropPlan>,
    ) {
        let tokens = &theme.tokens;
        match plan {
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
                let x0 = self.rect.x0
                    + self.scaled(ROW_INSET_X)
                    + (*depth as f64) * self.scaled(INDENT_PX)
                    + self.scaled(6.0);
                let caret = RoundedRect::new(
                    x0,
                    y - self.scaled(1.5),
                    self.rect.x1 - self.scaled(6.0),
                    y + self.scaled(1.5),
                    self.scaled(1.5),
                );
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
    }

    /// The half-transparent floating copy of the dragged row, following
    /// the cursor above the reflowing list.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_drag_ghost(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        metrics: &ListMetrics,
        drag_active: bool,
        from_idx: Option<usize>,
    ) {
        let tokens = &theme.tokens;
        if drag_active
            && let (Some(fi), Some(ghost)) = (
                from_idx,
                self.drag.ghost_rect(self.rect.x0, self.rect.x1, metrics),
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
            let label_x = ghost.x0 + self.scaled(ROW_INSET_X) + self.scaled(GLYPH_WIDTH);
            let text_y =
                ghost.y0 + (self.row_height() - line_h(ROW_TEXT_SIZE, self.text_scale)) / 2.0;
            text_renderer.draw_text(
                scene,
                &row.title,
                ROW_TEXT_SIZE,
                tokens.colors.text_primary,
                transform * Affine::translate((label_x, text_y)),
                Some(num::f32_of_f64(ghost.x1 - label_x - self.scaled(4.0))),
            );
        }
    }

    /// Routes to the context menu and an in-flight drag's Escape-to-cancel
    /// — whatever currently owns input, in that priority order. Returns
    /// `true` when the event was fully consumed and [`Self::handle_event`]
    /// should stop dispatching.
    #[allow(
        clippy::too_many_arguments,
        reason = "forwards the exact Widget::handle_event context bundle"
    )]
    fn handle_menu_and_drag_escape(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: Option<&NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
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
            let mut ft = focus_target.copied();
            self.menu.handle_event(
                event,
                scale_factor,
                &modifiers,
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
                return true;
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
            self.drag_hover = None;
            self.dirty = true;
            *handled = true;
            return true;
        }

        false
    }

    /// Routes to the active rename editor — commits it on a click outside
    /// its rect, then forwards the event (with Enter/Escape handled first
    /// so the editor doesn't swallow them). Returns `true` when the event
    /// was fully consumed and [`Self::handle_event`] should stop
    /// dispatching.
    #[allow(
        clippy::too_many_arguments,
        reason = "forwards the exact Widget::handle_event context bundle"
    )]
    fn handle_rename_routing(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
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
                        return true;
                    }
                    Key::Named(NamedKey::Escape) => {
                        self.cancel_rename();
                        *handled = true;
                        return true;
                    }
                    _ => {}
                }
            }
            r.input.handle_event(
                event,
                scale_factor,
                &modifiers,
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
                return true;
            }
        }

        false
    }

    fn handle_cursor_moved(&mut self, pt: Point) {
        if self.last_cursor.and_then(|point| self.hovered_row(point)) != self.hovered_row(pt) {
            self.dirty = true;
        }
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
            // Keep distant drop targets reachable without ending the gesture.
            // Speed ramps smoothly inside the top/bottom edge bands.
            let list_top = self.rect.y0 + self.header_height();
            let edge = self.scaled(36.0).max(1.0);
            let delta = if pt.y < list_top + edge {
                -((list_top + edge - pt.y) / edge).clamp(0.0, 1.0) * self.row_height() * 0.7
            } else if pt.y > self.rect.y1 - edge {
                ((pt.y - (self.rect.y1 - edge)) / edge).clamp(0.0, 1.0) * self.row_height() * 0.7
            } else {
                0.0
            };
            if delta != 0.0 {
                self.scroll_by(delta);
            }
            let metrics = self.list_metrics();
            self.drag.pointer_moved(pt, &metrics);
            self.update_drag_hover(pt, std::time::Instant::now());
            self.dirty = true;
        }
    }

    fn update_drag_hover(&mut self, pt: Point, now: std::time::Instant) {
        let target = self
            .drag
            .is_active()
            .then(|| self.onto_target(pt))
            .flatten()
            .and_then(|row| self.flat_rows.get(row))
            .filter(|row| row.has_children && !self.expanded.contains(&row.id))
            .map(|row| row.id);
        let Some(target) = target else {
            self.drag_hover = None;
            return;
        };
        match self.drag_hover {
            Some((hovered, since)) if hovered == target => {
                if now.duration_since(since) >= DRAG_EXPAND_DWELL {
                    self.expanded.insert(target);
                    self.drag_hover = None;
                    self.rows_dirty = true;
                    self.dirty = true;
                }
            }
            _ => self.drag_hover = Some((target, now)),
        }
    }

    fn handle_drag_release(&mut self, handled: &mut bool) {
        if !self.drag.is_pending() {
            return;
        }
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
        self.drag_hover = None;
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

    fn handle_right_click(
        &mut self,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
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
                let Some(page) = self.flat_rows.get(i).map(|r| r.id) else {
                    break;
                };
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

    fn handle_left_click(
        &mut self,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
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
            let Some((row_id, depth, has_children, title)) = self
                .flat_rows
                .get(i)
                .map(|r| (r.id, r.depth, r.has_children, r.title.clone()))
            else {
                break;
            };
            let (child_action, more_action) = self.row_action_rects(row_rect);
            if child_action.contains(pt) {
                self.pending_events.push(OutEvent::CreateChild {
                    parent: Some(row_id),
                });
                self.is_focused = true;
                *focus_target = Some(node_id);
                self.dirty = true;
                *handled = true;
                return;
            }
            if more_action.contains(pt) {
                self.selected = Some(row_id);
                self.pending_events.push(OutEvent::Select(row_id));
                self.is_focused = true;
                *focus_target = Some(node_id);
                self.open_context_menu(row_id, Point::new(more_action.x0, more_action.y1));
                *handled = true;
                return;
            }
            let glyph_x0 =
                row_rect.x0 + self.scaled(ROW_INSET_X) + (depth as f64) * self.scaled(INDENT_PX);
            let glyph_x1 = glyph_x0 + self.scaled(GLYPH_WIDTH);
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
                self.begin_rename(row_id, &title);
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

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive keyboard dispatcher keeps tree focus, hierarchy, reorder, rename, and delete precedence auditable in a single place"
    )]
    fn handle_key_press(&mut self, logical_key: &Key, mods: ModifiersState, handled: &mut bool) {
        if !self.is_focused {
            return;
        }
        // Keyboard context-menu parity: the hardware Menu key and the
        // conventional Shift+F10 chord open exactly the same menu as a
        // right-click, anchored to the selected row rather than the pointer.
        if matches!(logical_key, Key::Named(NamedKey::ContextMenu))
            || (mods.shift_key() && matches!(logical_key, Key::Named(NamedKey::F10)))
        {
            if let Some(page) = self.selected
                && let Some(index) = self.row_index_of(page)
                && let Some(rect) = self.row_rects.get(index).copied()
            {
                self.open_context_menu(page, Point::new(rect.x0 + self.scaled(28.0), rect.y1));
            } else {
                self.open_empty_area_menu(Point::new(
                    self.rect.x0 + self.scaled(16.0),
                    self.new_page_btn_rect.y1 + self.scaled(4.0),
                ));
            }
            *handled = true;
            return;
        }
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
        let selection_before = self.selected;
        match logical_key {
            // Outliner structure editing. Indent beneath the previous sibling;
            // outdent immediately after the current parent. Both become the
            // same canonical reparent action used by drag and drop.
            Key::Named(NamedKey::Tab) => {
                if let Some(sel) = self.selected {
                    let event = if mods.shift_key() {
                        self.outdent_event(sel)
                    } else {
                        self.indent_event(sel)
                    };
                    if let Some(event) = event {
                        if let OutEvent::Reparent {
                            new_parent: Some(parent),
                            ..
                        } = &event
                        {
                            // The target may have been a leaf until this
                            // action. Reveal the newly nested page instead of
                            // letting a successful Tab make it disappear.
                            self.expanded.insert(*parent);
                            self.rows_dirty = true;
                            self.dirty = true;
                        }
                        self.pending_events.push(event);
                        *handled = true;
                    }
                }
            }
            // Conventional tree navigation: Left first closes an open branch,
            // then moves upward; Right first opens a branch, then enters it.
            Key::Named(NamedKey::ArrowLeft) => {
                if let Some(sel) = self.selected {
                    if self.expanded.remove(&sel) {
                        self.rows_dirty = true;
                        self.dirty = true;
                        *handled = true;
                    } else if let Some(parent) = self
                        .flat_rows
                        .iter()
                        .find(|row| row.id == sel)
                        .and_then(|row| row.parent)
                    {
                        self.selected = Some(parent);
                        self.pending_events.push(OutEvent::Select(parent));
                        self.dirty = true;
                        *handled = true;
                    }
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                if let Some(sel) = self.selected
                    && let Some(idx) = self.row_index_of(sel)
                {
                    let Some(row) = self.flat_rows.get(idx) else {
                        return;
                    };
                    if row.has_children && !self.expanded.contains(&sel) {
                        self.expanded.insert(sel);
                        self.rows_dirty = true;
                        self.dirty = true;
                        *handled = true;
                    } else if row.has_children
                        && let Some(child) = self.flat_rows.get(idx + 1)
                        && child.parent == Some(sel)
                    {
                        let child = child.id;
                        self.selected = Some(child);
                        self.pending_events.push(OutEvent::Select(child));
                        self.dirty = true;
                        *handled = true;
                    }
                }
            }
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
                    if next != idx
                        && let Some(id) = self.flat_rows.get(next).map(|r| r.id)
                    {
                        self.selected = Some(id);
                        self.pending_events.push(OutEvent::Select(id));
                        self.dirty = true;
                        *handled = true;
                    }
                } else if let Some(id) = self.flat_rows.first().map(|r| r.id) {
                    self.selected = Some(id);
                    self.pending_events.push(OutEvent::Select(id));
                    self.dirty = true;
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(idx) = self.selected.and_then(|s| self.row_index_of(s)) {
                    let next = idx.saturating_sub(1);
                    if next != idx
                        && let Some(id) = self.flat_rows.get(next).map(|r| r.id)
                    {
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
                    && let Some(title) = self.flat_rows.get(idx).map(|r| r.title.clone())
                {
                    self.begin_rename(sel, &title);
                    *handled = true;
                }
            }
            _ => {}
        }
        if self.selected != selection_before
            && let Some(index) = self.selected.and_then(|page| self.row_index_of(page))
        {
            self.reveal_row(index);
        }
    }
}

impl NoteTreeView {
    /// Build the canonical action for indenting a page beneath its previous
    /// sibling. A first sibling cannot be indented: there is no unambiguous
    /// parent target.
    fn indent_event(&self, id: PageId) -> Option<OutEvent> {
        let idx = self.row_index_of(id)?;
        let row = self.flat_rows.get(idx)?;
        let previous = self
            .flat_rows
            .get(..idx)?
            .iter()
            .rev()
            .find(|candidate| candidate.parent == row.parent)?;
        Some(OutEvent::Reparent {
            id,
            new_parent: Some(previous.id),
            before: None,
        })
    }

    /// Build the canonical action for lifting a page one level. Position it
    /// directly after its former parent (before the parent's next sibling),
    /// which preserves the visual reading order expected by outliners.
    fn outdent_event(&self, id: PageId) -> Option<OutEvent> {
        let idx = self.row_index_of(id)?;
        let row = self.flat_rows.get(idx)?;
        let parent_id = row.parent?;
        let parent = self.flat_rows.iter().find(|row| row.id == parent_id)?;
        let before = self
            .flat_rows
            .iter()
            .skip_while(|candidate| candidate.id != parent_id)
            .skip(1)
            .find(|candidate| candidate.parent == parent.parent)
            .map(|candidate| candidate.id);
        Some(OutEvent::Reparent {
            id,
            new_parent: parent.parent,
            before,
        })
    }

    fn hovered_row(&self, point: Point) -> Option<usize> {
        if !self.rect.contains(point) || point.y < self.rect.y0 + self.header_height() {
            return None;
        }
        self.row_rects
            .iter()
            .position(|rect| rect.contains(point))
            .filter(|index| *index < self.flat_rows.len())
    }

    fn row_index_of(&self, id: PageId) -> Option<usize> {
        self.flat_rows.iter().position(|r| r.id == id)
    }

    /// Whether the right-click context menu is currently open. Read by
    /// the parent app so it can route keyboard nav to the menu (and away
    /// from the page list / editor) while it's up.
    pub fn menu_is_open(&self) -> bool {
        self.menu.is_open()
    }

    pub fn suppress_tooltips(&mut self, suppressed: bool) {
        self.tooltips_suppressed = suppressed;
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

    /// Test hook: the sidebar's laid-out rect, so a test can prove the
    /// overlay drawer stays inside a narrow window at larger text scales.
    #[doc(hidden)]
    pub fn rect_for_test(&self) -> Rect {
        self.rect
    }

    /// Test hook: the x of the disclosure-triangle hit centre for row `idx`,
    /// so tests click the real glyph instead of a hard-coded pixel that the
    /// row inset would shift out from under.
    #[doc(hidden)]
    pub fn row_disclosure_x_for_test(&self, idx: usize) -> Option<f64> {
        let row = self.row_rects.get(idx)?;
        let depth = self.flat_rows.get(idx)?.depth;
        Some(
            row.x0
                + self.scaled(ROW_INSET_X)
                + num::f64_of_usize(depth) * self.scaled(INDENT_PX)
                + self.scaled(GLYPH_WIDTH) / 2.0,
        )
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

    /// Rule 1 of the 1× polish bar for the page tree: two neighbouring rows'
    /// selection pills keep an `s1` between them and the pill sits inside its
    /// row band, from the same helper the painter uses.
    #[test]
    fn neighbouring_selection_pills_keep_an_s1_gap() {
        let theme = Theme::dark();
        let tokens = &theme.tokens;
        let row = |i: f64| Rect::new(0.0, i * ROW_HEIGHT, 240.0, (i + 1.0) * ROW_HEIGHT);
        let a = NoteTreeView::selection_pill(row(0.0), ROW_PILL_MARGIN, tokens);
        let b = NoteTreeView::selection_pill(row(1.0), ROW_PILL_MARGIN, tokens);
        assert!((b.y0 - a.y1 - f64::from(tokens.spacing.s1)).abs() < 1e-9);
        assert!((a.x0 - ROW_PILL_MARGIN).abs() < 1e-9);
        assert!((240.0 - a.x1 - ROW_PILL_MARGIN).abs() < 1e-9);
        assert!(
            a.y0 > 0.0 && a.y1 < ROW_HEIGHT,
            "the pill stays inside its row band"
        );
    }

    /// The selected page's fill is a contrast step off the raised sidebar on
    /// every preset, with an edge that registers on that fill (HIG §9.3/§9.5).
    #[test]
    fn selection_fill_moves_off_the_raised_sidebar_on_every_preset() {
        use ui::surface::{edge_delta, edge_on};
        use ui::tokens::colors::HAIRLINE_MIN_DELTA;
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let raised = theme.tokens.colors.surfaces.raised;
            let lifted = selection_fill_on_raised(&theme);
            assert!(edge_delta(lifted, raised) > 0.0, "{}", theme.name);
            if theme.name != "High Contrast" {
                assert!(
                    edge_delta(lifted, raised) >= HAIRLINE_MIN_DELTA,
                    "{}",
                    theme.name
                );
            }
            assert!(
                edge_delta(edge_on(lifted), lifted) >= HAIRLINE_MIN_DELTA,
                "{}",
                theme.name
            );
        }
    }

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
    fn dragging_over_collapsed_branch_springs_it_open_after_dwell() {
        let mut ws = Workspace::new();
        let target = ws.create_page(None);
        let _child = ws.create_page(Some(target));
        let dragged = ws.create_page(None);
        let mut tree = NoteTreeView::new();
        tree.sync_from_workspace(&ws);
        tree.expanded.remove(&target);
        tree.sync_from_workspace(&ws);
        tree.rect = Rect::new(0.0, 0.0, 220.0, 300.0);

        let start = Point::new(100.0, tree.header_height() + tree.row_height() * 1.5);
        tree.drag.begin(dragged, 1, tree.row_height() / 2.0, start);
        let over_target = Point::new(100.0, tree.header_height() + tree.row_height() / 2.0);
        tree.drag.pointer_moved(over_target, &tree.list_metrics());
        let now = std::time::Instant::now();
        tree.update_drag_hover(over_target, now);
        assert!(!tree.expanded.contains(&target));

        tree.update_drag_hover(over_target, now + DRAG_EXPAND_DWELL);
        assert!(tree.expanded.contains(&target));
        assert!(tree.take_rows_dirty());
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

    #[test]
    fn page_menu_copy_link_uses_the_selected_page_identity() {
        let mut workspace = Workspace::new();
        let page = workspace.create_page(None);
        let mut tree = NoteTreeView::new();
        tree.sync_from_workspace(&workspace);
        tree.menu_target = Some(page);
        tree.apply_menu_selection(3);
        assert_eq!(tree.take_transfer(), Some(SidebarTransfer::CopyLink(page)));
    }

    #[test]
    fn hover_row_actions_create_a_child_or_open_the_existing_note_menu() {
        let mut workspace = Workspace::new();
        let page = workspace.create_page(None);
        let mut tree = NoteTreeView::new();
        tree.sync_from_workspace(&workspace);
        tree.rect = Rect::new(0.0, 0.0, 240.0, 200.0);
        tree.row_rects = vec![Rect::new(0.0, 42.0, 240.0, 72.0)];
        let (child, more) = tree.row_action_rects(tree.row_rects[0]);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut focus = None;
        let mut handled = false;

        tree.last_cursor = Some(child.center());
        tree.handle_left_click(&mut focus, node, &mut handled);
        assert!(handled);
        assert!(matches!(
            tree.take_events().as_slice(),
            [OutEvent::CreateChild { parent: Some(id) }] if *id == page
        ));

        handled = false;
        tree.last_cursor = Some(more.center());
        tree.handle_left_click(&mut focus, node, &mut handled);
        assert!(handled);
        assert!(tree.context_menu_open_for_test());
        assert!(matches!(
            tree.take_events().as_slice(),
            [OutEvent::Select(id)] if *id == page
        ));
    }

    #[test]
    fn page_menu_semantics_activate_the_same_queued_page_action() {
        let mut workspace = Workspace::new();
        let page = workspace.create_page(None);
        let mut tree = NoteTreeView::new();
        tree.sync_from_workspace(&workspace);
        tree.open_context_menu(page, Point::ZERO);
        let node = tree.accessibility().unwrap();
        let menu = node
            .children
            .iter()
            .find(|node| node.role == ui::a11y::Role::Menu)
            .unwrap();
        assert_eq!(menu.state.expanded, Some(true));
        assert_eq!(
            menu.children
                .iter()
                .map(|item| item.name.clone().unwrap())
                .collect::<Vec<_>>(),
            vec![
                ui::i18n!("notes-menu-new-child"),
                ui::i18n!("notes-menu-rename"),
                ui::i18n!("notes-menu-duplicate"),
                ui::i18n!("notes-menu-copy-link"),
                ui::i18n!("notes-menu-export"),
                ui::i18n!("notes-menu-insert-image"),
                ui::i18n!("notes-menu-delete")
            ]
        );
        assert!(
            menu.children
                .iter()
                .all(|item| item.role == ui::a11y::Role::MenuItem)
        );
        let target = menu.children.last().unwrap().action_target.clone().unwrap();
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        assert!(!tree.perform_semantic_action(
            &target,
            &ui::a11y::Action::Increment,
            &mut doc,
            &ObjId::Root
        ));
        assert!(tree.take_events().is_empty());
        assert!(tree.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut doc,
            &ObjId::Root
        ));
        assert!(matches!(tree.take_events().as_slice(), [OutEvent::Delete(id)] if *id == page));
        assert_eq!(
            doc.get_heads(),
            before,
            "sidebar activation only queues; app owns document admission"
        );
        assert!(
            !tree.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut doc,
                &ObjId::Root
            ),
            "closed menu target is stale"
        );
    }

    #[test]
    fn page_tree_semantics_follow_visible_rows_and_their_hit_rects() {
        let mut ws = Workspace::new();
        let root = ws.create_page(None);
        ws.rename(root, "Project plan");
        let mut tree = NoteTreeView::new();
        tree.sync_from_workspace(&ws);
        tree.new_page_btn_rect = Rect::new(160.0, 8.0, 186.0, 34.0);
        tree.row_rects = vec![Rect::new(0.0, 42.0, 200.0, 72.0)];

        let node = Widget::accessibility(&tree).expect("page tree is semantic");
        assert_eq!(node.role, ui::a11y::Role::Tree);
        assert_eq!(node.children.len(), 2);
        assert_eq!(
            node.children[0].action_target.as_deref(),
            Some("notes:new-page")
        );
        assert_eq!(node.children[1].name.as_deref(), Some("Project plan"));
        assert_eq!(
            Widget::synthetic_accessibility_rect(&tree, &[1]),
            Some(tree.row_rects[0])
        );
        assert!(Widget::focus_synthetic_accessibility(&mut tree, &[1]));
        assert_eq!(
            tree.selected,
            Some(root),
            "AT focus selects the visible page"
        );
    }

    #[test]
    fn high_text_rows_and_labels_keep_distinct_scaled_geometry() {
        let mut tree = NoteTreeView::new();
        tree.text_scale = 2.0;
        tree.rect = Rect::new(0.0, 0.0, 528.0, 600.0);
        let first = Rect::new(
            tree.rect.x0,
            tree.rect.y0 + tree.header_height(),
            tree.rect.x1,
            tree.rect.y0 + tree.header_height() + tree.row_height(),
        );
        let second = Rect::new(first.x0, first.y1, first.x1, first.y1 + tree.row_height());
        let label_x = first.x0 + tree.scaled(ROW_INSET_X) + tree.scaled(GLYPH_WIDTH);

        assert!(tree.header_height() <= first.y0);
        assert!(first.y1 <= second.y0, "adjacent rows must not overlap");
        assert!(label_x < first.x1 - tree.scaled(4.0));
    }

    #[test]
    fn large_tree_scroll_is_clamped_and_moves_drag_geometry() {
        let mut tree = NoteTreeView::new();
        tree.rect = Rect::new(0.0, 0.0, 240.0, 300.0);
        tree.max_scroll_y = 600.0;
        assert!(tree.scroll_by(90.0));
        assert!((tree.scroll_y - 90.0).abs() < f64::EPSILON);
        assert!(
            (tree.list_metrics().slot_lead(0) - (tree.rect.y0 + tree.header_height() - 90.0)).abs()
                < f64::EPSILON
        );
        assert!(tree.scroll_by(1_000.0));
        assert!((tree.scroll_y - 600.0).abs() < f64::EPSILON);
        assert!(!tree.scroll_by(1.0), "scrolling past the end is a no-op");
        assert!(tree.scroll_by(-1_000.0));
        assert!(tree.scroll_y.abs() < f64::EPSILON);

        tree.max_scroll_y = 600.0;
        tree.reveal_row(15);
        assert!(
            tree.scroll_y > 0.0,
            "keyboard navigation reveals a selected row below the viewport"
        );
        tree.reveal_row(0);
        assert!(tree.scroll_y.abs() < f64::EPSILON);
    }

    #[test]
    fn dragging_in_bottom_edge_band_autoscrolls_toward_hidden_targets() {
        let mut workspace = Workspace::new();
        for _ in 0..30 {
            workspace.create_page(None);
        }
        let mut tree = NoteTreeView::new();
        tree.sync_from_workspace(&workspace);
        tree.rect = Rect::new(0.0, 0.0, 240.0, 300.0);
        tree.max_scroll_y = 700.0;
        let dragged = tree.flat_rows[0].id;
        let start = Point::new(100.0, tree.header_height() + tree.row_height() / 2.0);
        tree.drag.begin(dragged, 0, tree.row_height() / 2.0, start);

        tree.handle_cursor_moved(Point::new(100.0, tree.rect.y1 - 1.0));

        assert!(tree.drag.is_active());
        assert!(
            tree.scroll_y > 0.0,
            "edge dragging reveals lower drop targets without ending the gesture"
        );
    }
}
