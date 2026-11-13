//! How a [`VirtualList`]'s rows stand as a tree ([`VirtualList::with_tree`]): which row each
//! stands under, how deep it stands, and which rows a folded row hides.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::ops::Range;
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::keyboard::{Key, NamedKey};

use super::{ListModel, RowGroup, RowStance, VirtualList};
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::theme::Theme;

pub const TREE_INDENT: f64 =
    (crate::tokens::Metrics::BASE.icon_sm + crate::tokens::Spacing::BASE.rhythm.related) as f64;
const TREE_DEPTH_GUARD: u32 = 64;

#[derive(Clone, Copy)]
pub(super) struct Chevrons {
    pub(super) x0: f64,
    pub(super) lead: f64,
    pub(super) column: f64,
    pub(super) step: f64,
}

impl<M: ListModel> VirtualList<M> {
    pub(super) fn chevrons(&self) -> Chevrons {
        Chevrons {
            x0: self.rect.x0,
            lead: self.lead,
            column: self.chevron_column,
            step: self.disclosure,
        }
    }
}

impl Chevrons {
    pub(super) fn gutter<K: Clone + Eq + Hash>(
        &self,
        marks: &TreeMarks<K>,
        key: &K,
    ) -> Option<f64> {
        if !marks.is_branch(key) {
            return None;
        }
        Some(self.x0 + self.lead + self.column + (f64::from(marks.depth(key)) - 1.0) * self.step)
    }

    pub(super) fn rect<K: Clone + Eq + Hash>(
        &self,
        marks: &TreeMarks<K>,
        theme: &Theme,
        key: &K,
        row: Rect,
        folded: bool,
    ) -> Option<(Rect, bool)> {
        let gutter = self.gutter(marks, key)?;
        let side = f64::from(theme.tokens.metrics.icon_sm);
        let cy = row.center().y;
        Some((
            Rect::new(gutter, cy - side / 2.0, gutter + side, cy + side / 2.0),
            folded,
        ))
    }

    /// Paint the hairline up each depth's gutter the row `key` stands past, through its own box
    /// `row`, so a deep tree keeps its structure legible.
    pub(super) fn paint_guides<K: Clone + Eq + Hash>(
        &self,
        marks: &TreeMarks<K>,
        scene: &mut Scene,
        transform: Affine,
        key: &K,
        row: Rect,
        color: vello::peniko::Color,
    ) {
        let depth = marks.depth(key);
        for level in 0..depth {
            let x =
                self.x0 + self.lead + self.column + f64::from(level) * self.step + self.step / 2.0;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color,
                None,
                &Rect::new(x - 0.5, row.y0, x + 0.5, row.y1),
            );
        }
    }

    pub(super) fn paint<K: Clone + Eq + Hash>(
        &self,
        marks: &TreeMarks<K>,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        key: &K,
        row: Rect,
        folded: bool,
    ) {
        let Some((chevron, folded)) = self.rect(marks, theme, key, row, folded) else {
            return;
        };
        let icon = if folded {
            crate::icons::Icon::ChevronRight
        } else {
            crate::icons::Icon::ChevronDown
        };
        icon.draw(scene, transform, chevron, theme.text_dim);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeRow<K> {
    pub parent: Option<K>,
    /// How deep it stands, when the model says: the list indents the row by it rather than by
    /// the rows over it, so a row whose parent stands off screen — a row the host renders as it
    /// is shown, and one of a window deep in a long tree — keeps its place.
    pub depth: Option<u32>,
    pub above: Option<K>,
}

#[derive(Debug)]
pub struct TreeMarks<K> {
    depth: std::cell::RefCell<HashMap<K, u32>>,
    folded: std::cell::RefCell<HashSet<K>>,
    branches: std::cell::RefCell<HashSet<K>>,
    nested: std::cell::RefCell<HashSet<K>>,
}

impl<K> Default for TreeMarks<K> {
    fn default() -> Self {
        Self {
            depth: std::cell::RefCell::default(),
            folded: std::cell::RefCell::default(),
            branches: std::cell::RefCell::default(),
            nested: std::cell::RefCell::default(),
        }
    }
}

impl<K: Clone + Eq + Hash> TreeMarks<K> {
    pub fn depth(&self, key: &K) -> u32 {
        self.depth.borrow().get(key).copied().unwrap_or(0)
    }

    pub fn is_folded(&self, key: &K) -> bool {
        self.folded.borrow().contains(key)
    }

    pub fn is_branch(&self, key: &K) -> bool {
        self.branches.borrow().contains(key)
    }

    /// Whether the row stands under the row it follows, rather than away from the parent it
    /// names: a list keeps the order its filter gave it, and a subtask the sort put elsewhere —
    /// in another group, under another heading — says whose it is instead of standing indented
    /// under nothing.
    pub fn is_nested(&self, key: &K) -> bool {
        self.nested.borrow().contains(key)
    }

    #[must_use]
    pub fn folded_keys(&self) -> Vec<K> {
        self.folded.borrow().iter().cloned().collect()
    }

    pub fn restore_folded(&self, keys: impl IntoIterator<Item = K>) {
        *self.folded.borrow_mut() = keys.into_iter().collect();
    }

    pub fn fold(&self, key: &K, folded: bool) {
        let mut folded_set = self.folded.borrow_mut();
        if folded {
            folded_set.insert(key.clone());
        } else {
            folded_set.remove(key);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeMove {
    Up,
    Down,
    Indent,
    Outdent,
}

impl<M: ListModel> VirtualList<M> {
    #[must_use]
    pub fn with_tree(mut self, marks: Rc<TreeMarks<M::Key>>) -> Self {
        self.tree = Some(marks);
        self.tree_rows.clear();
        self.tree_hidden.clear();
        self.visible_window_changed();
        self
    }

    /// Draw a hairline up each depth's gutter through nested rows, so a deep tree keeps its
    /// structure legible.
    #[must_use]
    pub fn with_indent_guides(mut self) -> Self {
        self.indent_guides = true;
        self
    }

    /// Where the row of `key` stands on screen, its own box below the band of a heading it
    /// starts a group under. `None` while the row is not placed (folded away, or far from the
    /// box).
    #[must_use]
    pub fn row_rect(&self, key: &M::Key) -> Option<Rect> {
        self.row_box(self.model.index_of(key)?)
    }

    #[must_use]
    pub fn disclosure_rect(&self, key: &M::Key) -> Option<Rect> {
        let marks = self.tree.as_ref()?;
        let row = self.line_box(key, self.row_rect(key)?);
        let gutter = self.chevrons().gutter(marks, key)?;
        let side = self.disclosure;
        let cy = row.center().y;
        Some(Rect::new(
            gutter,
            cy - side / 2.0,
            gutter + side,
            cy + side / 2.0,
        ))
    }

    /// The row's own line, where a row that grew into its item still starts: its chevron and
    /// the way back to the part of it on screen hang on the line, not beside the document the
    /// row opened into, which may run far below it.
    ///
    /// The line is the box the row's content stands in — the one its text, its leading control
    /// and its lift share — across the slot the list gives it. A finger's row takes a taller slot
    /// than its box, so the slot's own centre is not the line's.
    pub(super) fn line_box(&self, key: &M::Key, row: Rect) -> Rect {
        let own = self
            .mounted
            .iter()
            .find(|mounted| &mounted.key == key)
            .map(|mounted| mounted.widget.get_rect())
            .filter(|own| own.height() > 0.0)
            .unwrap_or(row);
        let line = Rect::new(row.x0, own.y0, row.x1, own.y1);
        if self.open.as_ref() == Some(key) && line.height() > self.row_line {
            Rect::new(line.x0, line.y0, line.x1, line.y0 + self.row_line)
        } else {
            line
        }
    }

    #[must_use]
    pub fn row_depth(&self, key: &M::Key) -> u32 {
        self.tree.as_ref().map_or(0, |marks| marks.depth(key))
    }

    #[must_use]
    pub fn row_is_branch(&self, key: &M::Key) -> bool {
        self.tree.as_ref().is_some_and(|marks| marks.is_branch(key))
    }

    /// The key of the `nth` row the list shows, counting the rows on screen in reading order: a
    /// folded row's subtasks are not among them, nor a closed group's rows.
    #[must_use]
    pub fn visible_key(&self, nth: usize) -> Option<M::Key> {
        let mut seen = 0;
        for index in 0..self.model.len() {
            let Some(key) = self.model.key_at(index) else {
                continue;
            };
            if !self.row_open(index) || self.tree_hidden.contains(&key) {
                continue;
            }
            if seen == nth {
                return Some(key);
            }
            seen = seen.saturating_add(1);
        }
        None
    }

    #[must_use]
    pub fn visible_len(&self) -> usize {
        (0..self.model.len())
            .filter(|index| {
                self.row_open(*index)
                    && self
                        .model
                        .key_at(*index)
                        .is_some_and(|key| !self.tree_hidden.contains(&key))
            })
            .count()
    }

    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.reorder.is_active()
    }

    #[must_use]
    pub fn row_action_rect(&self, key: &M::Key, action: usize) -> Option<Rect> {
        let index = self.model.index_of(key)?;
        let row = self
            .mounted
            .iter()
            .find(|row| row.index == index && row.state == super::MountedRowState::Ready)?;
        crate::widget::capability::capability::<dyn super::RowActionRects>(row.widget.as_ref())?
            .row_action_rects()
            .get(action)
            .copied()
    }

    pub fn fold_row(&mut self, key: &M::Key, folded: bool) -> bool {
        let Some(marks) = self.tree.clone() else {
            return false;
        };
        if marks.is_folded(key) == folded {
            return false;
        }
        marks.fold(key, folded);
        if let Some(callback) = &mut self.on_fold {
            callback(key, folded);
        }
        self.invalidated.insert(key.clone());
        self.visible_window_changed();
        true
    }

    #[must_use]
    pub fn row_folded(&self, key: &M::Key) -> bool {
        self.tree.as_ref().is_some_and(|marks| marks.is_folded(key))
    }

    pub(super) fn shape_tree(&mut self, range: &Range<usize>) {
        if self.tree.is_none() {
            return;
        }
        let rebuilt: Vec<M::Key> = self.invalidated.iter().cloned().collect();
        for key in rebuilt {
            self.tree_rows.remove(&key);
        }
        let rows: Vec<(usize, M::Key)> = range
            .clone()
            .filter_map(|index| Some((index, self.model.key_at(index)?)))
            .collect();
        let mut over: Vec<(M::Key, u32, bool)> = Vec::new();
        let mut depth: HashMap<M::Key, u32> = HashMap::new();
        let mut hidden: HashSet<M::Key> = HashSet::new();
        let mut branches: Vec<M::Key> = Vec::new();
        let mut nested: Vec<M::Key> = Vec::new();
        let mut heading: Option<RowGroup> = None;
        for (index, key) in &rows {
            let group = self.model.group_at(*index);
            if group.is_some() && group != heading {
                over.clear();
            }
            heading = group;
            let facts = self.tree_facts(key);
            let parent = facts.as_ref().and_then(|facts| facts.parent.clone());
            let stated = facts.as_ref().and_then(|facts| facts.depth);
            let at = parent
                .as_ref()
                .and_then(|parent| over.iter().rposition(|(row, _, _)| row == parent));
            let beneath = at.zip(parent.clone());
            let (found, away) = if let Some((at, parent)) = beneath {
                let (_, depth, hidden) = over[at];
                branches.push(parent.clone());
                nested.push(key.clone());
                over.truncate(at + 1);
                let open = self.open.as_ref() == Some(&parent);
                (depth + 1, hidden || self.folded(&parent) || open)
            } else if let Some(depth) = stated {
                // The model says how deep it stands: a row whose parent the window does not
                // hold keeps the place the model gives it, and the row it names still opens.
                if let Some(parent) = parent.clone() {
                    branches.push(parent);
                }
                over.clear();
                (depth, self.under_open(key))
            } else {
                over.clear();
                (0, self.under_open(key))
            };
            let found = found.min(TREE_DEPTH_GUARD);
            depth.insert(key.clone(), found);
            if away {
                hidden.insert(key.clone());
            }
            over.push((key.clone(), found, away));
        }
        for index in range.clone() {
            if self.model.has_subtasks(index)
                && let Some(key) = self.model.key_at(index)
            {
                branches.push(key);
            }
        }
        let first_level = rows
            .iter()
            .map(|(_, key)| key)
            .filter(|key| depth.get(*key) == Some(&0))
            .fold(None, |found: Option<bool>, key| {
                Some(found.unwrap_or(false) || branches.contains(key))
            });
        // A first level that stops folding anything while rows it showed still stand keeps its
        // column: a task ticked off taking its subtasks out of the list, say. Giving the column
        // back would move every row the person is looking at sideways, the one under their
        // finger too; the list lines its rows up with the words over them again when it next
        // reads rows anew. A column that a first-level row now needs is taken at once.
        // A range whose rows are still on their way says nothing of either.
        if let Some(branch) = first_level {
            let kept = self.chevron_column > 0.5 || self.lead > self.bleed + 0.5;
            let staying = rows.iter().any(|(_, key)| self.column_rows.contains(key));
            self.column_rows = rows.iter().map(|(_, key)| key.clone()).collect();
            self.first_level_branch = Some(branch || (kept && staying));
        }
        if let Some(marks) = &self.tree {
            marks.depth.borrow_mut().extend(depth);
            marks.branches.borrow_mut().extend(branches);
            marks.nested.borrow_mut().extend(nested);
        }
        self.tree_hidden = hidden;
    }

    fn under_open(&mut self, key: &M::Key) -> bool {
        let Some(open) = self.open.clone() else {
            return false;
        };
        let mut cursor = key.clone();
        for _ in 0..TREE_DEPTH_GUARD {
            let Some(parent) = self.tree_facts(&cursor).and_then(|facts| facts.parent) else {
                return false;
            };
            if parent == open {
                return true;
            }
            cursor = parent;
        }
        false
    }

    fn tree_facts(&mut self, key: &M::Key) -> Option<TreeRow<M::Key>> {
        if let Some(facts) = self.tree_rows.get(key) {
            return Some(facts.clone());
        }
        let index = self.model.index_of(key)?;
        let facts = self.model.tree_at(index)?;
        self.tree_rows.insert(key.clone(), facts.clone());
        Some(facts)
    }

    fn folded(&self, key: &M::Key) -> bool {
        self.tree.as_ref().is_some_and(|marks| marks.is_folded(key))
    }

    pub(super) fn forget_tree(&mut self) {
        self.tree_rows.clear();
        self.tree_hidden.clear();
        self.first_level_branch = None;
        if let Some(marks) = &self.tree {
            marks.depth.borrow_mut().clear();
            marks.branches.borrow_mut().clear();
            marks.nested.borrow_mut().clear();
        }
    }

    /// Keep the column a chevron hangs in left of the rows only while a row of the first level
    /// has subtasks to fold: a tree whose first level folds nothing stands its rows on the
    /// column the words over them do. The rows are built again where the column moves, as they
    /// are as wide as the column leaves them.
    pub(super) fn settle_lead(&mut self) {
        let gutter = if self.tree.is_some() {
            // A tree's first level says whether it folds anything, read as its rows mount:
            // until a row of it has been read, the column stands as it always has.
            match self.first_level_branch {
                Some(false) => 0.0,
                Some(true) | None => self.disclosure,
            }
        } else if self.model.discloses() {
            self.disclosure
        } else {
            0.0
        };
        // Where the rows' box starts in from the list's leading edge, and the column kept for
        // the chevrons in front of their content. A column's list keeps its gutter: the rows' box
        // stands on the pane's content edge — so the row's lift keeps the column's gutter — and
        // the chevron column stands inside that box, as the first thing at the edge, where on a
        // phone it would otherwise reach the glass. A list inside a page has its own edge as the
        // gutter: the page already stands in from the window, so the chevron column stands in
        // front of the rows' box there.
        let (lead, chevron_column) = match self.stance {
            RowStance::Column => (self.bleed, gutter),
            RowStance::Bleed | RowStance::Flush => (self.bleed.max(gutter), 0.0),
        };
        let stepped = (self.chevron_column - chevron_column).abs() > 0.5;
        self.chevron_column = chevron_column;
        if (lead - self.lead).abs() <= 0.5 && !stepped {
            return;
        }
        self.lead = lead;
        self.update_edges();
        let mounted: Vec<M::Key> = self.mounted.iter().map(|row| row.key.clone()).collect();
        self.invalidate_rows(mounted);
    }

    fn disclosure_at(&self, key: &M::Key) -> bool {
        self.last_cursor.is_some_and(|cursor| {
            self.disclosure_rect(key)
                .is_some_and(|rect| rect.contains(cursor))
        })
    }

    pub(super) fn press_row(&mut self, key: M::Key, modifiers: winit::event::Modifiers) {
        if self.disclosure_at(&key) {
            if self.open.as_ref() == Some(&key) {
                self.open_where_it_stands(key);
                return;
            }
            let folded = self.row_folded(&key);
            self.fold_row(&key, !folded);
            return;
        }
        if self.open.as_ref() == Some(&key) {
            return;
        }
        self.click_row(key, modifiers);
    }

    /// Bind `Tab` / `Shift+Tab` to indent and outdent the focused row of a tree, as an
    /// outliner does. The `Alt` arrows indent and outdent regardless. Needs
    /// [`Self::with_reorder`]; a `Tab` that cannot move the row is left to focus traversal.
    #[must_use]
    pub fn tab_indents(mut self, enabled: bool) -> Self {
        self.tab_indents = enabled;
        self
    }

    /// How many rows under its top row `key` stands, read from what the model says of its
    /// parents rather than from the rows in view: a move reaches rows the window never built.
    pub(super) fn depth_of(&mut self, key: &M::Key) -> u32 {
        if let Some(depth) = self.tree_facts(key).and_then(|facts| facts.depth) {
            return depth;
        }
        let mut depth = 0;
        let mut cursor = key.clone();
        while depth < TREE_DEPTH_GUARD {
            let Some(parent) = self.tree_facts(&cursor).and_then(|facts| facts.parent) else {
                break;
            };
            depth = depth.saturating_add(1);
            cursor = parent;
        }
        depth
    }

    fn above_of(&mut self, key: &M::Key, index: usize, depth: u32) -> Option<M::Key> {
        if let Some(above) = self.tree_facts(key).and_then(|facts| facts.above) {
            return Some(above);
        }
        let above = self.sibling_above(index, depth)?;
        self.model.key_at(above)
    }

    fn sibling_above(&mut self, index: usize, depth: u32) -> Option<usize> {
        let mut at = index;
        while let Some(above) = at.checked_sub(1) {
            let key = self.model.key_at(above)?;
            match self.depth_of(&key) {
                found if found == depth => return Some(above),
                found if found < depth => return None,
                _ => at = above,
            }
        }
        None
    }

    fn sibling_below(&mut self, index: usize, depth: u32) -> Option<usize> {
        let mut at = index.saturating_add(1);
        while let Some(key) = self.model.key_at(at) {
            match self.depth_of(&key) {
                found if found == depth => return Some(at),
                found if found < depth => return None,
                _ => at = at.saturating_add(1),
            }
        }
        None
    }

    pub(super) fn move_target(
        &mut self,
        key: &M::Key,
        movement: TreeMove,
    ) -> Option<DropTarget<M::Key>> {
        let index = self.model.index_of(key)?;
        let depth = self.depth_of(key);
        match movement {
            TreeMove::Up => {
                let above = self.above_of(key, index, depth)?;
                Some(DropTarget::new(above, DropPlacement::Before))
            }
            TreeMove::Down => {
                let below = self.sibling_below(index, depth)?;
                Some(DropTarget::new(
                    self.model.key_at(below)?,
                    DropPlacement::After,
                ))
            }
            TreeMove::Indent => {
                let above = self.above_of(key, index, depth)?;
                Some(DropTarget::new(above, DropPlacement::Inside))
            }
            TreeMove::Outdent => {
                let parent = self.tree_facts(key).and_then(|facts| facts.parent)?;
                Some(DropTarget::new(parent, DropPlacement::After))
            }
        }
    }

    /// Move the row of `key` one step through its tree, through the same validator and commit
    /// a drop uses; the row keeps the keyboard and stays in view. Whether it moved.
    pub fn move_row(&mut self, key: &M::Key, movement: TreeMove) -> bool {
        if self.reorder_validator.is_none() || self.reorder_commit.is_none() {
            return false;
        }
        let Some(target) = self.move_target(key, movement) else {
            return false;
        };
        let model = &self.model;
        let accepted = self
            .reorder_validator
            .as_mut()
            .is_some_and(|validate| validate(model, key, &target));
        if !accepted
            || !self.commit_reorder(ReorderDrop {
                source: key.clone(),
                target,
            })
        {
            return false;
        }
        if let Some(at) = self.model.index_of(key) {
            self.scroll_to_index(at);
        }
        self.focus_row(key.clone());
        true
    }

    pub fn move_focused_step(&mut self, movement: TreeMove) -> bool {
        let Some(key) = self.focused_row_key() else {
            return false;
        };
        self.move_row(&key, movement)
    }

    pub(super) fn tree_move_chord(
        &self,
        key: &Key,
        modifiers: winit::keyboard::ModifiersState,
    ) -> Option<TreeMove> {
        if self.reorder_validator.is_none() || self.reorder_commit.is_none() {
            return None;
        }
        let plain = !modifiers.control_key() && !modifiers.super_key();
        match key {
            Key::Named(NamedKey::ArrowUp) if modifiers.alt_key() && plain => Some(TreeMove::Up),
            Key::Named(NamedKey::ArrowDown) if modifiers.alt_key() && plain => Some(TreeMove::Down),
            Key::Named(NamedKey::ArrowRight) if modifiers.alt_key() && plain => {
                Some(TreeMove::Indent)
            }
            Key::Named(NamedKey::ArrowLeft) if modifiers.alt_key() && plain => {
                Some(TreeMove::Outdent)
            }
            Key::Named(NamedKey::Tab) if self.tab_indents && plain && !modifiers.alt_key() => {
                Some(if modifiers.shift_key() {
                    TreeMove::Outdent
                } else {
                    TreeMove::Indent
                })
            }
            _ => None,
        }
    }

    pub(super) fn tree_candidate(
        &mut self,
        index: usize,
        key: &M::Key,
        row: Rect,
        cursor: vello::kurbo::Point,
    ) -> Option<crate::reorder::DropCandidate<M::Key>> {
        let marks = self.tree.clone()?;
        let fraction = ((cursor.y - row.y0) / row.height().max(1.0)).clamp(0.0, 1.0);
        let depth = self.depth_of(key);
        let mut placement = if fraction < 0.25 {
            DropPlacement::Before
        } else if fraction > 0.75 || !self.model.accepts_children(index) {
            DropPlacement::After
        } else {
            DropPlacement::Inside
        };
        let mut target = key.clone();
        let mut seam = if fraction < 0.5 { row.y0 } else { row.y1 };
        if placement == DropPlacement::After
            && marks.is_branch(key)
            && !marks.is_folded(key)
            && let Some(below) = self.model.key_at(index.saturating_add(1))
            && self.depth_of(&below) == depth.saturating_add(1)
        {
            target = below;
            placement = DropPlacement::Before;
            seam = row.y1;
        }
        let indent = self.rect.x0
            + self.lead
            + self.chevron_column
            + (f64::from(depth) + 1.0) * self.disclosure;
        let right = self.lift_rect(0.0, 0.0).x1;
        let preview = match placement {
            DropPlacement::Inside => Rect::new(indent, row.y0, right, row.y1),
            DropPlacement::Before | DropPlacement::After => {
                Rect::new(indent, seam - 1.0, right, seam + 1.0)
            }
        };
        let folded = placement == DropPlacement::Inside
            && marks.is_branch(&target)
            && marks.is_folded(&target);
        Some(crate::reorder::DropCandidate {
            target: DropTarget::new(target, placement),
            preview,
            can_hover_expand: folded,
        })
    }

    pub(super) fn unfold_for_drop(&mut self, key: &M::Key) -> bool {
        self.fold_row(key, false)
    }

    pub(super) fn fold_step(&mut self, open: bool) -> bool {
        let Some(marks) = self.tree.clone() else {
            return false;
        };
        let Some(key) = self.focused_key.clone() else {
            return false;
        };
        let Some(index) = self.model.index_of(&key) else {
            return false;
        };
        let depth = marks.depth(&key);
        if open {
            if marks.is_branch(&key) && marks.is_folded(&key) {
                return self.fold_row(&key, false);
            }
            let under = index
                .checked_add(1)
                .and_then(|below| self.model.key_at(below))
                .is_some_and(|below| marks.depth(&below) == depth + 1);
            under && self.focus_index(index + 1, false)
        } else {
            if self.open.as_ref() == Some(&key) {
                self.open_where_it_stands(key);
                return true;
            }
            if marks.is_branch(&key) && !marks.is_folded(&key) {
                return self.fold_row(&key, true);
            }
            let parent = self.tree_facts(&key).and_then(|facts| facts.parent);
            let Some(at) = parent
                .as_ref()
                .and_then(|parent| self.model.index_of(parent))
                .filter(|_| depth > 0)
            else {
                return false;
            };
            self.focus_index(at, false)
        }
    }
}
