//! How a [`VirtualList`]'s rows stand as a tree ([`VirtualList::with_tree`]): which row each
//! stands under, how deep it stands, and which rows a folded row hides.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::ops::Range;
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::keyboard::{Key, NamedKey};

use super::{ListModel, VirtualList};
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::theme::Theme;

/// One step of a tree's nesting ([`VirtualList::with_tree`]): the disclosure gutter a row's
/// content stands in by, and where the chevron that folds what stands under it hangs, left of
/// its own depth's column.
pub const TREE_INDENT: f64 =
    (crate::tokens::Metrics::BASE.icon_sm + crate::tokens::Spacing::BASE.rhythm.related) as f64;
/// The room a tree row's disclosure marker takes at its leading edge.
pub const TREE_DISCLOSURE: f64 = crate::tokens::Metrics::BASE.icon_sm as f64;
/// How far up a row's chain of parents the list follows it: deeper than anyone nests a row, so
/// a chain that loops still ends.
const TREE_DEPTH_GUARD: u32 = 64;

/// Where a tree's rows stand: `x0` the list's leading edge, `lead` where its rows stand in
/// from it, and `step` one depth's disclosure gutter.
#[derive(Clone, Copy)]
pub(super) struct Chevrons {
    pub(super) x0: f64,
    pub(super) lead: f64,
    pub(super) step: f64,
}

impl<M: ListModel> VirtualList<M> {
    /// The chevrons of a row draw placed where this list's rows stand.
    pub(super) fn chevrons(&self) -> Chevrons {
        Chevrons {
            x0: self.rect.x0,
            lead: self.lead,
            step: self.disclosure,
        }
    }
}

impl Chevrons {
    /// The gutter the row `key`'s chevron hangs in, when it has subtasks: left of its own
    /// depth's content column, saying whether they show.
    pub(super) fn gutter<K: Clone + Eq + Hash>(
        &self,
        marks: &TreeMarks<K>,
        key: &K,
    ) -> Option<(f64, bool)> {
        if !marks.is_branch(key) {
            return None;
        }
        let gutter = self.x0 + self.lead + f64::from(marks.depth(key)) * self.step - self.step;
        Some((gutter, marks.is_folded(key)))
    }

    /// The box the row `key`'s chevron takes: in its own depth's gutter, centred on the row's
    /// own box `row`, and how the row folds what stands under it. `None` when the row has no
    /// subtasks to fold.
    pub(super) fn rect<K: Clone + Eq + Hash>(
        &self,
        marks: &TreeMarks<K>,
        theme: &Theme,
        key: &K,
        row: Rect,
    ) -> Option<(Rect, bool)> {
        let (gutter, folded) = self.gutter(marks, key)?;
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
            let x = self.x0 + self.lead + f64::from(level) * self.step + self.step / 2.0;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color,
                None,
                &Rect::new(x - 0.5, row.y0, x + 0.5, row.y1),
            );
        }
    }

    /// Paint the row `key`'s chevron into the row's own box `row`, at its own depth's gutter.
    pub(super) fn paint<K: Clone + Eq + Hash>(
        &self,
        marks: &TreeMarks<K>,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        key: &K,
        row: Rect,
    ) {
        let Some((chevron, folded)) = self.rect(marks, theme, key, row) else {
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

/// What a row is in a tree of rows: the row of the item it is a subtask of, when that item is
/// one of the list's rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeRow<K> {
    /// The row of the item this row is a subtask of, when that item is a row of the list.
    pub parent: Option<K>,
}

/// What a tree list works out of its rows, for the rows it builds: how deep each stands, which
/// of them are folded away, which have subtasks, and which stand under the row they follow. The
/// list fills it as it mounts rows; a row's builder reads it, and the person's folding stays
/// with the list, so a row folded away stays folded while its neighbours come and go.
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
    /// How many rows under its top row `key` stands.
    pub fn depth(&self, key: &K) -> u32 {
        self.depth.borrow().get(key).copied().unwrap_or(0)
    }

    /// Whether the person folded the row's subtasks away.
    pub fn is_folded(&self, key: &K) -> bool {
        self.folded.borrow().contains(key)
    }

    /// Whether the row has subtasks, a disclosure to show.
    pub fn is_branch(&self, key: &K) -> bool {
        self.branches.borrow().contains(key)
    }

    /// Whether the row stands under the row it follows, rather than away from the parent it
    /// names: a list keeps the order its filter gave it, and a subtask the sort put elsewhere
    /// says whose it is instead of standing indented under nothing.
    pub fn is_nested(&self, key: &K) -> bool {
        self.nested.borrow().contains(key)
    }

    /// The rows whose subtasks the person folded away, as a view saved them.
    #[must_use]
    pub fn folded_keys(&self) -> Vec<K> {
        self.folded.borrow().iter().cloned().collect()
    }

    /// Fold exactly `keys`' subtasks away, showing every other row's: a saved view restored.
    pub fn restore_folded(&self, keys: impl IntoIterator<Item = K>) {
        *self.folded.borrow_mut() = keys.into_iter().collect();
    }

    /// Fold the row's subtasks away, or show them again.
    pub fn fold(&self, key: &K, folded: bool) {
        let mut folded_set = self.folded.borrow_mut();
        if folded {
            folded_set.insert(key.clone());
        } else {
            folded_set.remove(key);
        }
    }
}

/// One step a row moves through a tree: its order among the rows it stands beside, and how
/// deep it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeMove {
    /// Before the row above it at its own depth.
    Up,
    /// After the row below it at its own depth.
    Down,
    /// Into the row above it at its own depth, as its last subtask.
    Indent,
    /// Out of the row it stands under, directly after it.
    Outdent,
}

impl<M: ListModel> VirtualList<M> {
    /// The rows stand as a tree: each under the row of the item it names as its parent, indented
    /// by the rows over it, with a disclosure on the rows that have subtasks, folding them away
    /// where the person folds them. `marks` is what the rows the list builds read, filled as it
    /// mounts them ([`TreeMarks`]).
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

    /// The box the row `key`'s disclosure takes: the chevron in its own depth's gutter, centred
    /// on the row. `None` for a row with no subtasks, or one not placed.
    #[must_use]
    pub fn disclosure_rect(&self, key: &M::Key) -> Option<Rect> {
        let marks = self.tree.as_ref()?;
        let row = self.row_rect(key)?;
        let (gutter, _) = self.chevrons().gutter(marks, key)?;
        let side = self.disclosure;
        let cy = row.center().y;
        Some(Rect::new(
            gutter,
            cy - side / 2.0,
            gutter + side,
            cy + side / 2.0,
        ))
    }

    /// How many rows under its top row `key` stands, as the list's tree shows it.
    #[must_use]
    pub fn row_depth(&self, key: &M::Key) -> u32 {
        self.tree.as_ref().map_or(0, |marks| marks.depth(key))
    }

    /// Whether the row `key` has subtasks, a disclosure to show.
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

    /// How many rows the list shows, counting the rows not folded away.
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

    /// Whether a drag past the threshold is in flight over the rows.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.reorder.is_active()
    }

    /// Where the hover action `action` of the row `key` sits, reordering what the row shows:
    /// the centre a person presses.
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

    /// Fold the row's subtasks away, or show them again, as its disclosure, its menu or the
    /// keyboard does, and remember what a person folded even for a row whose subtasks the
    /// model has not yet said it holds.
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
        // The row draws the disclosure the other way round.
        self.invalidated.insert(key.clone());
        self.visible_window_changed();
        true
    }

    /// Whether the row folds its subtasks away now.
    #[must_use]
    pub fn row_folded(&self, key: &M::Key) -> bool {
        self.tree.as_ref().is_some_and(|marks| marks.is_folded(key))
    }

    /// What the tree's rows are in `range`: what stands under what, how deep each row stands,
    /// and which rows a folded one hides. A tree's rows come in order: a row stands under the
    /// row it follows when it names it, so the walk reads each row of the range once, and only
    /// the rows in view.
    pub(super) fn shape_tree(&mut self, range: &Range<usize>) {
        if self.tree.is_none() {
            return;
        }
        // Rows rebuilt since the last pass are asked again.
        let rebuilt: Vec<M::Key> = self.invalidated.iter().cloned().collect();
        for key in rebuilt {
            self.tree_rows.remove(&key);
        }
        let keys: Vec<M::Key> = range
            .clone()
            .filter_map(|index| self.model.key_at(index))
            .collect();
        // The rows over the row being read, outermost first, each with how deep it stands and
        // whether a row over it hides it.
        let mut over: Vec<(M::Key, u32, bool)> = Vec::new();
        let mut depth: HashMap<M::Key, u32> = HashMap::new();
        let mut hidden: HashSet<M::Key> = HashSet::new();
        let mut branches: Vec<M::Key> = Vec::new();
        let mut nested: Vec<M::Key> = Vec::new();
        for key in &keys {
            let parent = self.tree_facts(key).and_then(|facts| facts.parent);
            let at = parent
                .as_ref()
                .and_then(|parent| over.iter().rposition(|(row, _, _)| row == parent));
            // The row it follows is the row it names, or a row over that one: it stands under
            // its parent, as deep as its parent plus one.
            let beneath = at.zip(parent.clone());
            let (found, away) = if let Some((at, parent)) = beneath {
                let (_, depth, hidden) = over[at];
                branches.push(parent.clone());
                nested.push(key.clone());
                over.truncate(at + 1);
                (depth + 1, hidden || self.folded(&parent))
            } else {
                // The list holds its order, not a tree: a row whose parent the sort put
                // elsewhere stands on its own, and names the parent instead.
                over.clear();
                (0, false)
            };
            let found = found.min(TREE_DEPTH_GUARD);
            depth.insert(key.clone(), found);
            if away {
                hidden.insert(key.clone());
            }
            over.push((key.clone(), found, away));
        }
        // What the model says has subtasks discloses them whether or not any stands in the
        // list yet: a folder read when it opens.
        for index in range.clone() {
            if self.model.has_subtasks(index)
                && let Some(key) = self.model.key_at(index)
            {
                branches.push(key);
            }
        }
        if let Some(marks) = &self.tree {
            marks.depth.borrow_mut().extend(depth);
            marks.branches.borrow_mut().extend(branches);
            marks.nested.borrow_mut().extend(nested);
        }
        self.tree_hidden = hidden;
    }

    /// What the model says row `key` is in a tree, read once while its props stand as they
    /// were.
    fn tree_facts(&mut self, key: &M::Key) -> Option<TreeRow<M::Key>> {
        if let Some(facts) = self.tree_rows.get(key) {
            return Some(facts.clone());
        }
        let index = self.model.index_of(key)?;
        let facts = self.model.tree_at(index)?;
        self.tree_rows.insert(key.clone(), facts.clone());
        Some(facts)
    }

    /// Whether the person folded the row's subtasks away.
    fn folded(&self, key: &M::Key) -> bool {
        self.tree.as_ref().is_some_and(|marks| marks.is_folded(key))
    }

    /// The tree as the model gives it changed: its rows are read again.
    pub(super) fn forget_tree(&mut self) {
        self.tree_rows.clear();
        self.tree_hidden.clear();
        if let Some(marks) = &self.tree {
            marks.depth.borrow_mut().clear();
            marks.branches.borrow_mut().clear();
            marks.nested.borrow_mut().clear();
        }
    }

    /// Whether the pointer is over the disclosure at the leading edge of the row at `index`:
    /// the room the row indents by, then the column that folds its subtasks.
    fn disclosure_at(&self, index: usize, key: &M::Key) -> bool {
        let Some(cursor) = self.last_cursor else {
            return false;
        };
        let Some(marks) = self.tree.as_ref() else {
            return false;
        };
        if !marks.is_branch(key) {
            return false;
        }
        // The chevron hangs in the gutter left of the row's content column, at the row's own
        // depth: the rows stand `lead` in, and step one disclosure gutter per depth.
        let chevrons = self.chevrons();
        let Some((x0, _)) = chevrons.gutter(marks, key) else {
            return false;
        };
        // The row's own box, below the band of a heading it starts a group under, where the
        // chevron is drawn.
        let Some(row) = self.row_box(index) else {
            return false;
        };
        let rect = Rect::new(x0, row.y0, x0 + self.disclosure, row.y1);
        rect.contains(cursor)
    }

    /// A completed click on the row of `key` at `index`: its disclosure folds what is under
    /// it, and anywhere else the click is the row's.
    pub(super) fn press_row_at(
        &mut self,
        index: usize,
        key: M::Key,
        modifiers: winit::event::Modifiers,
    ) {
        if self.disclosure_at(index, &key) {
            let folded = self.row_folded(&key);
            self.fold_row(&key, !folded);
            return;
        }
        // A press on the row standing open landed inside the item it shows — in its
        // document, or on the row over it — not on a row of the list: what the item's own
        // controls did not take, the item keeps.
        if self.open.as_ref() == Some(&key) {
            return;
        }
        self.click_row(key, modifiers);
    }

    /// As [`Self::press_row_at`], for a row found by its key.
    pub(super) fn press_row(&mut self, key: M::Key, modifiers: winit::event::Modifiers) {
        if let Some(index) = self.model.index_of(&key) {
            self.press_row_at(index, key, modifiers);
        }
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

    /// The row above `index` that stands as deep as `depth`, stepping over the rows under it.
    fn sibling_above(&mut self, index: usize, depth: u32) -> Option<usize> {
        let mut at = index;
        while let Some(above) = at.checked_sub(1) {
            let key = self.model.key_at(above)?;
            match self.depth_of(&key) {
                found if found == depth => return Some(above),
                // A row less deep is the parent it stands under: no row beside it above.
                found if found < depth => return None,
                _ => at = above,
            }
        }
        None
    }

    /// The row below `index` that stands as deep as `depth`, stepping over the rows under it.
    fn sibling_below(&mut self, index: usize, depth: u32) -> Option<usize> {
        let mut at = index.saturating_add(1);
        while let Some(key) = self.model.key_at(at) {
            match self.depth_of(&key) {
                found if found == depth => return Some(at),
                // A row less deep is the parent it stands under: no row beside it below.
                found if found < depth => return None,
                _ => at = at.saturating_add(1),
            }
        }
        None
    }

    /// The drop target that performs `movement` on the row of `key`, when the tree has room
    /// for it: a row only indents into a row that takes subtasks, and a top row has nowhere
    /// to step out to.
    pub(super) fn move_target(
        &mut self,
        key: &M::Key,
        movement: TreeMove,
    ) -> Option<DropTarget<M::Key>> {
        let index = self.model.index_of(key)?;
        let depth = self.depth_of(key);
        match movement {
            TreeMove::Up => {
                let above = self.sibling_above(index, depth)?;
                Some(DropTarget::new(
                    self.model.key_at(above)?,
                    DropPlacement::Before,
                ))
            }
            TreeMove::Down => {
                let below = self.sibling_below(index, depth)?;
                Some(DropTarget::new(
                    self.model.key_at(below)?,
                    DropPlacement::After,
                ))
            }
            TreeMove::Indent => {
                let above = self.sibling_above(index, depth)?;
                self.model
                    .accepts_children(above)
                    .then(|| {
                        self.model
                            .key_at(above)
                            .map(|above| DropTarget::new(above, DropPlacement::Inside))
                    })
                    .flatten()
            }
            // Out of the row it stands under, directly after it: a top row stays where it is.
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

    /// Move the focused row one step through its tree ([`TreeMove`]). Whether it moved: no
    /// focused row, a row with the keyboard elsewhere, or a step the tree has no room for
    /// moves nothing.
    pub fn move_focused_step(&mut self, movement: TreeMove) -> bool {
        let Some(key) = self.focused_row_key() else {
            return false;
        };
        self.move_row(&key, movement)
    }

    /// The move `key` with `modifiers` asks of a tree that reorders: the arrows with `Alt`,
    /// and `Tab`/`Shift+Tab` where the list [indents with it](super::VirtualList::tab_indents).
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

    /// The drop a pointer on the row of `key`, standing on its box `row`, offers this tree: a
    /// row that takes subtasks takes a drop into it from its middle, and the seam below an
    /// open branch lands above the first row it holds, where the line is drawn. `None` for a
    /// list whose rows stand no tree.
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
        // Below an open branch the seam sits above the first row it holds: a drop there lands
        // as that row's predecessor, where the line is drawn, not as the branch's next sibling
        // far under everything it holds.
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
        let indent = self.rect.x0 + self.lead + f64::from(depth) * self.disclosure;
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

    /// The row a drop hovered into a folded branch shows, as a tree's disclosure does: what
    /// it holds comes into view where it will land.
    pub(super) fn unfold_for_drop(&mut self, key: &M::Key) -> bool {
        self.fold_row(key, false)
    }

    /// A fold step of the keyboard, as the disclosure makes it under the pointer: `open` (the
    /// right key) opens the focused branch, or steps into the row that stands under it; `!open`
    /// (the left key) folds the focused branch away, or steps out to the row it stands under.
    /// Whether it folded or moved; `false` where the key has no place: no tree, no focused row,
    /// or a leaf with nothing to step into.
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
            // What stands under the row follows it, one depth deeper; a row of its own depth
            // follows a leaf, and the key leaves the keyboard where it is.
            let under = index
                .checked_add(1)
                .and_then(|below| self.model.key_at(below))
                .is_some_and(|below| marks.depth(&below) == depth + 1);
            under && self.focus_index(index + 1, false)
        } else {
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
