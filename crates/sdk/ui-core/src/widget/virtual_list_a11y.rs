//! The accessibility tree of a [`super::VirtualList`]: its mounted rows, and the heading
//! standing before each group of them.
//!
//! Each heading is a disclosure of its own, as the band over its group is: focusable, named
//! after the group, expanded while its rows show, and carrying the number of rows it holds,
//! so a reader hears what folding it away would hide. An action on such a node comes back to
//! the list by its [`super::groups::HEADING_TARGET`].

use crate::i18n::widget_text;
use std::collections::HashMap;
use std::sync::Arc;

use super::edit::RenameStamp;
use super::groups::{Band, Group, HEADING_TARGET};
use super::open::first_page_title;
use super::{ListModel, MountedRow, MountedRowState, VirtualList};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::widget::Widget;
use vello::kurbo::Rect;

/// The row nodes of the last accessibility walk, kept: while everything a row's
/// node is built from stands, the walk hands the tree the very node it handed it
/// last time, which the tree takes without comparing it. A notebook's page list
/// is walked on every keystroke the note beside it takes, and nothing about its
/// rows changed.
pub(super) struct KeptAccessRows<K> {
    rows: HashMap<K, KeptRow>,
    /// Counts the walks, to tell the rows the last one reached.
    walk: u64,
    /// The row nodes walks built rather than kept.
    builds: usize,
}

impl<K> Default for KeptAccessRows<K> {
    fn default() -> Self {
        Self {
            rows: HashMap::new(),
            walk: 0,
            builds: 0,
        }
    }
}

struct KeptRow {
    inputs: RowAccessInputs,
    node: Arc<AccessNode>,
    /// The last walk that reached the row.
    walk: u64,
}

/// Everything a row's node is built from: its own state and place, and
/// [`MountedRow::stamp`], which changes when the row's widget is built anew
/// (its content or its selection changed). While these stand, so does the node.
#[derive(PartialEq)]
struct RowAccessInputs {
    stamp: u64,
    index: usize,
    count: usize,
    bounds: Option<Rect>,
    focused: bool,
    selected: bool,
    /// Whether the row's menu can be asked for.
    menu: bool,
    disabled: bool,
    /// How deep the row stands, where it stands as a tree, and whether its
    /// subtasks show.
    level: Option<usize>,
    expanded: Option<bool>,
    /// The name the host gave the row.
    semantic: Option<String>,
    /// What the row is showing instead of the delegate's own row, if anything.
    state: RowStateStamp,
    /// The inline rename field over the row, while one is open on it.
    renaming: Option<RenameStamp>,
}

/// The part of [`MountedRowState`] a row's node says.
#[derive(PartialEq)]
enum RowStateStamp {
    Ready,
    Pending,
    Failed(String),
}

impl From<&MountedRowState> for RowStateStamp {
    fn from(state: &MountedRowState) -> Self {
        match state {
            MountedRowState::Ready => Self::Ready,
            MountedRowState::Pending => Self::Pending,
            MountedRowState::Failed(message, _) => Self::Failed(message.clone()),
        }
    }
}

impl<M: ListModel> VirtualList<M> {
    /// The row nodes walks built rather than kept, since the list was made.
    #[cfg(any(test, feature = "test-support"))]
    pub fn access_row_builds_for_test(&self) -> usize {
        self.access_rows.borrow().builds
    }

    /// Run `walk` with no row kept, as a list that keeps none walks, and
    /// keep the rows kept before it again afterwards.
    #[cfg(test)]
    pub(crate) fn walk_keeping_nothing<R>(&self, walk: impl FnOnce() -> R) -> R {
        let kept = self.access_rows.replace(KeptAccessRows::default());
        let walked = walk();
        *self.access_rows.borrow_mut() = kept;
        walked
    }

    /// Whether `row`'s own widget tree is handed to a reader under the row's node: the item it
    /// stands open into, whose document holds the fields that edit it, and any row whose widget
    /// carries a control of its own (a box to tick, a field, a button) that the row's state
    /// alone does not reach. A row of words stays summarized: the row states it, and its words
    /// are not read twice.
    pub(super) fn handing_controls(&self, row: &MountedRow<M::Key>) -> bool {
        self.standing_open(row) || holds_controls(row.widget.as_ref())
    }

    /// Whether `row` stands open into its item where it stands: the item's document takes the
    /// row's place in the list, so the row is the item, named and announced by what it states.
    fn standing_open(&self, row: &MountedRow<M::Key>) -> bool {
        self.open.as_ref() == Some(&row.key)
    }

    /// The node of one mounted row: a list item holding the row's own semantics.
    pub(super) fn row_node(
        &self,
        row: &MountedRow<M::Key>,
        enabled: bool,
    ) -> crate::a11y::AccessNode {
        let count = self.model.len();
        // What the row states of itself: its widget's own node, or the node of the widget it wraps
        // that names it. A row drawn by a box or a toolbar that adds no node stands where what it
        // wraps does.
        let child = crate::a11y::stated_node(row.widget.as_ref());
        // The item standing open in its row is still the row: it is named by the title its
        // document states, never by the name of the field the document happens to begin with.
        let name = self
            .standing_open(row)
            .then(|| first_page_title(row.widget.as_ref()))
            .flatten()
            .or_else(|| child.as_ref().and_then(|node| node.name.clone()));
        let mut item = AccessNode::new(if self.tree.is_some() {
            Role::TreeItem
        } else {
            Role::ListItem
        })
        .with_optional_name(name)
        .with_position(row.index.saturating_add(1), count)
        .with_state(
            AccessState::new()
                .focusable(true)
                .focused(self.is_focused && self.focused_key.as_ref() == Some(&row.key))
                .selected(self.selection.is_selected(&row.key))
                // A row whose menu the host offers can be asked for it, as a
                // secondary click does.
                .context_menu(self.menus.offers_row_menu())
                .disabled(!enabled),
        );
        // A row that is a control of its own — a task's box, say — keeps its state and what it
        // explains of itself on the node the list gives it, so a reader hears one row rather
        // than a row and an unnamed child of it.
        if let Some(checked) = child.as_ref().and_then(|node| node.state.checked) {
            item.state.checked = Some(checked);
        }
        if let Some(description) = child.as_ref().and_then(|node| node.description.clone()) {
            item.description = Some(description);
        }
        // A row that says how deep it stands is a tree item of its own list: its level, whether
        // what stands under it shows, and the value it reads (a count) are the row's.
        if let Some(own) = child.as_ref().filter(|node| node.level.is_some()) {
            item.role = Role::TreeItem;
            item.level = own.level;
            item.state.expanded = own.state.expanded;
            item.value.clone_from(&own.value);
        }
        // A tree row says how deep it stands and, where it has subtasks, whether they show.
        if let Some(marks) = &self.tree {
            item = item.with_level(marks.depth(&row.key).saturating_add(1) as usize);
            if marks.is_branch(&row.key) {
                item.state.expanded = Some(!marks.is_folded(&row.key));
            }
        }
        // The row standing open into its item says so, as the row over a document does: what
        // the item holds shows under the row, where the row stands in the list.
        if self.standing_open(row) {
            item.state.expanded = Some(true);
        }
        match &row.state {
            MountedRowState::Ready => {}
            MountedRowState::Pending => item.state.busy = true,
            MountedRowState::Failed(message, _) => {
                item.state.invalid = true;
                item.description = Some(message.clone());
            }
        }
        if let Some(row_box) = self.row_box(row.index) {
            // The row's own box, below the band of a heading it starts a group under. The
            // list places it, so it stands however far the row's own widget has been laid
            // out; what that widget says of its own bounds stands inside it.
            item = item.with_bounds(row_box);
        }
        if let Some(encode) = &self.semantic_key {
            // The row keeps the name its host gave it across rebuilds, so a
            // reader, an agent and a control find it however the rows moved.
            let key = encode(&row.key);
            item = item
                .with_action_target(format!("key:{key}"))
                .with_semantic_key(key);
        }
        // The name the row's own widget gives it for assistive technology stands over the key
        // when that widget is the row itself (a tree row, which says how deep it stands): a
        // widget the list draws inside its row names what it draws, not the row, and the list's
        // own key ([`VirtualList::with_semantic_key`]) names the row it holds.
        let owns_the_row = child.as_ref().is_some_and(|node| node.level.is_some());
        if (owns_the_row || self.semantic_key.is_none())
            && let Some(semantic) = child.as_ref().and_then(|node| node.semantic_key.clone())
        {
            item.key = Some(crate::a11y::hash_key(&semantic));
            item.semantic_key = Some(semantic);
        }
        // A row renamed in place carries the field over its label, as a tree's row does, so a
        // reader reaches the title it edits.
        let field = self
            .rename
            .as_ref()
            .filter(|field| field.key == row.key)
            .and_then(|field| {
                Widget::accessibility(&field.input).map(|mut input| {
                    input.name = Some(widget_text("tree-rename-field"));
                    input.bounds = Some(field.input.get_rect().into());
                    input
                })
            });
        // The row's own widget is this node: the list states the row (its name, its place, its
        // state), and whatever the widget says of itself inside it — how deep it stands, what it
        // reads, the box to tick and the actions over its trailing corner — stands under the row
        // itself, not under a second node of the same row. So do the controls of the item a row
        // stands open into: that item is the row, named by the title it states, so a reader hears
        // the row once and reaches what it holds.
        let mut children: Vec<AccessNode> = Vec::new();
        if let Some(node) = child {
            if item.value.is_none() {
                item.value = node.value;
            }
            if item.item.is_none() {
                item.item = node.item;
            }
            children = node.children;
        }
        children.splice(0..0, field);
        if !children.is_empty() {
            item = item.with_children(children);
        }
        item
    }

    pub(super) fn perform_a11y_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        use crate::a11y::Action;

        // The rows' menu, whose items are named into the list's own targets.
        if let Some(performed) = self.perform_menu_action(target, action, state, path) {
            return performed;
        }

        // A heading's group, as its node in the accessibility tree names it: read, folded and
        // unfolded by something that is not the keyboard.
        if let Some(heading) = target.strip_prefix(super::groups::HEADING_TARGET) {
            let heading = heading.to_owned();
            return match action {
                Action::Focus => self.focus_heading(&heading),
                Action::Activate => self.focus_heading(&heading) && self.toggle_heading(&heading),
                Action::Expand => self.set_heading_open(&heading, true),
                Action::Collapse => self.set_heading_open(&heading, false),
                Action::ScrollIntoView => self.heading_start(&heading).is_some_and(|index| {
                    self.scroll_to_index(index);
                    true
                }),
                _ => false,
            };
        }
        if let Some(key) = self.key_for_name(target.strip_prefix("key:").unwrap_or(target))
            && let Some(index) = self.model.index_of(&key)
        {
            return match action {
                Action::Focus => {
                    self.focus_row(key.clone());
                    self.select_focused(key, false);
                    self.scroll_to_index(index);
                    true
                }
                Action::Activate => {
                    if !matches!(
                        self.model.row_state(index),
                        crate::arriving::Arrival::Ready(())
                    ) {
                        return false;
                    }
                    self.focus_row(key.clone());
                    self.activate_key(key);
                    true
                }
                Action::ScrollIntoView => {
                    self.scroll_to_index(index);
                    true
                }
                // A tree row folds what stands under it, as its disclosure does. A row
                // standing open shows its branch in its item's document, so collapsing it
                // closes the row: the rows it holds come back to the list.
                Action::Expand | Action::Collapse if self.tree.is_some() => {
                    if matches!(action, Action::Collapse) && self.open.as_ref() == Some(&key) {
                        self.open_where_it_stands(key);
                        true
                    } else {
                        self.fold_row(&key, matches!(action, Action::Collapse))
                    }
                }
                Action::ShowContextMenu if self.menus.offers_row_menu() => {
                    let point = self
                        .row_box(index)
                        .map_or(self.rect.center(), |row| row.center());
                    self.open_row_menu(key, point)
                }
                // A row that carries a box of its own takes the tick, as a
                // press on the box does.
                Action::ToggleCheck => self
                    .mounted
                    .iter_mut()
                    .find(|mounted| mounted.key == key)
                    .is_some_and(|mounted| {
                        mounted
                            .widget
                            .perform_semantic_action(target, action, state, path)
                    }),
                _ => false,
            };
        }
        // A row's own controls name the row, not the list: the row answers for them, as it
        // did when the walk reached it as a widget of its own. So does the widget that states
        // the row where the row is drawn by a box or a toolbar that adds no node of its own:
        // that widget published the node a reader acts on.
        for row in &mut self.mounted {
            if row
                .widget
                .perform_semantic_action(target, action, state, path)
            {
                return true;
            }
            if let Some(stated) = crate::a11y::stated_widget_mut(row.widget.as_mut())
                && stated.perform_semantic_action(target, action, state, path)
            {
                return true;
            }
        }
        false
    }

    /// The accessibility children the list carries, in reading order: each group's heading
    /// before the rows it holds, those still on screen. A row nothing changed about is the
    /// very node the last walk handed over, so a walk beside it costs nothing per row.
    pub(super) fn accessibility_children(&self, enabled: bool) -> Vec<Arc<AccessNode>> {
        let count = self.model.len();
        let mut kept = self.access_rows.borrow_mut();
        let walk = kept.walk.wrapping_add(1);
        kept.walk = walk;
        let mut children = Vec::new();
        let mut reached = 0_usize;
        let mut bands = self.tree_bands().into_iter().peekable();
        for row in &self.mounted {
            while bands.peek().is_some_and(|band| band.index <= row.index) {
                if let Some(band) = bands.next() {
                    children.push(Arc::new(self.heading_node(band, enabled)));
                }
            }
            let inputs = self.row_access_inputs(row, enabled, count);
            reached = reached.saturating_add(1);
            if let Some(kept_row) = kept
                .rows
                .get_mut(&row.key)
                .filter(|kept_row| kept_row.inputs == inputs)
            {
                kept_row.walk = walk;
                children.push(Arc::clone(&kept_row.node));
                continue;
            }
            // The row was built anew, or something it shows moved: the node it
            // makes now stands in for the last one only where they agree.
            let node = self.row_node(row, enabled);
            kept.builds = kept.builds.saturating_add(1);
            if let Some(kept_row) = kept.rows.get_mut(&row.key)
                && *kept_row.node == node
            {
                kept_row.inputs = inputs;
                kept_row.walk = walk;
                children.push(Arc::clone(&kept_row.node));
                continue;
            }
            let node = Arc::new(node);
            children.push(Arc::clone(&node));
            kept.rows
                .insert(row.key.clone(), KeptRow { inputs, node, walk });
        }
        for band in bands {
            children.push(Arc::new(self.heading_node(band, enabled)));
        }
        // The rows' menu stands over them while it is up.
        children.extend(self.menu_node().map(Arc::new));
        if kept.rows.len() != reached {
            kept.rows.retain(|_, row| row.walk == walk);
        }
        children
    }

    /// Everything the node of `row` is built from, short of building it
    /// ([`RowAccessInputs`]).
    fn row_access_inputs(
        &self,
        row: &MountedRow<M::Key>,
        enabled: bool,
        count: usize,
    ) -> RowAccessInputs {
        let (level, folded) = match &self.tree {
            Some(marks) => (
                Some(marks.depth(&row.key).saturating_add(1) as usize),
                marks
                    .is_branch(&row.key)
                    .then(|| !marks.is_folded(&row.key)),
            ),
            None => (None, None),
        };
        let expanded = if self.standing_open(row) {
            Some(true)
        } else {
            folded
        };
        RowAccessInputs {
            stamp: row.stamp,
            index: row.index,
            count,
            bounds: self.row_box(row.index),
            focused: self.is_focused && self.focused_key.as_ref() == Some(&row.key),
            selected: self.selection.is_selected(&row.key),
            menu: self.menus.offers_row_menu(),
            disabled: !enabled,
            level,
            expanded,
            semantic: self.semantic_key.as_ref().map(|encode| encode(&row.key)),
            state: RowStateStamp::from(&row.state),
            renaming: self
                .rename
                .as_ref()
                .filter(|field| field.key == row.key)
                .map(|field| field.stamp()),
        }
    }

    /// The list's own node, without its children: what focus capture and the
    /// focus order read of it.
    pub(super) fn access_root(&self, enabled: bool) -> AccessNode {
        let role = if self.tree.is_some() {
            Role::Tree
        } else {
            Role::List
        };
        AccessNode::new(role)
            .with_optional_name(
                self.accessible_name
                    .as_deref()
                    .map(crate::i18n::I18nManager::lookup_plain),
            )
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.is_focused)
                    .disabled(!enabled),
            )
    }

    /// How many headings the tree stands before the mounted row `child_index`: the index of
    /// the row's own node among the list's children.
    pub(super) fn headings_before(&self, child_index: usize) -> usize {
        let Some(row) = self.mounted.get(child_index) else {
            return 0;
        };
        self.tree_bands()
            .iter()
            .filter(|band| band.index <= row.index)
            .count()
    }

    /// The bands the last layout placed, in the order they head the rows.
    fn tree_bands(&self) -> Vec<&Band> {
        let mut bands: Vec<&Band> = self.groups.bands.iter().collect();
        bands.sort_by_key(|band| band.index);
        bands
    }

    /// The heading's node in the accessibility tree: a disclosure saying whether its group
    /// shows and how many rows it holds.
    fn heading_node(&self, band: &Band, enabled: bool) -> AccessNode {
        let focused = self
            .focused_group
            .as_ref()
            .is_some_and(|(heading, _)| *heading == band.heading);
        AccessNode::new(Role::Button)
            .with_name(band.heading.clone())
            .with_value(self.heading_rows(band).to_string())
            .with_action_target(format!("{HEADING_TARGET}{}", band.heading))
            .with_bounds(self.band_rect(band.top))
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.is_focused && focused)
                    .disabled(!enabled)
                    .expanded(!band.closed),
            )
    }

    /// How many rows stand under the band's heading: its group's, and each row after them that
    /// names no group of its own — one whose item has not arrived yet stands there too, under
    /// the heading the person reads it by — until the next heading. The count is what the
    /// heading shows over, never a row less.
    pub(super) fn heading_rows(&self, band: &Band) -> usize {
        let len = self.model.len();
        let tail = self.tail_title.as_deref();
        let group = |index: usize| super::groups::group_of(&self.model, index, tail);
        let same =
            |index: usize| group(index).is_some_and(|group: Group| group.heading == band.heading);
        let mut start = band.index.min(len);
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        let mut end = start;
        while end < len && group(end).is_none_or(|group: Group| group.heading == band.heading) {
            end += 1;
        }
        end - start
    }
}

/// Whether `widget` holds a control a reader operates below it: a widget that takes keyboard
/// focus (a field, a box, a button). Words, and the boxes that only hold them, are the row's
/// own — the row already says them.
fn holds_controls(widget: &dyn Widget) -> bool {
    let mut holds = false;
    widget.for_each_child(&mut |child| {
        holds = holds || child.is_focusable() || holds_controls(child);
    });
    holds
}

#[cfg(test)]
mod tests {
    use super::super::open::first_page_title;
    use crate::tokens::TextRole;
    use crate::widget::arrange::Row;
    use crate::widget::{Element, Text};

    /// A document titles itself: the title its words state is the item's, not a name the first
    /// field it happens to begin with gives it.
    #[test]
    fn a_documents_title_is_the_title_it_states() {
        let document: Element = Box::new(Row::row().push(Box::new(Text::new("Title"))).push(
            Box::new(Text::new("ring the plumber").with_role(TextRole::Title)),
        ));
        assert_eq!(
            first_page_title(document.as_ref()).as_deref(),
            Some("ring the plumber")
        );
        let untitled: Element = Box::new(Text::new("Title"));
        assert_eq!(first_page_title(untitled.as_ref()), None);
    }
}
