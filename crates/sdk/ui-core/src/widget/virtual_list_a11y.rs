//! The accessibility tree of a [`super::VirtualList`]: its mounted rows, and the heading standing
//! before each group of them.
use crate::i18n::widget_text;
use std::collections::HashMap;
use std::sync::Arc;

use super::edit::RenameStamp;
use super::groups::{Band, HEADING_TARGET};
use super::open::first_page_title;
use super::{ListModel, MountedRow, MountedRowState, VirtualList};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::widget::Widget;
use vello::kurbo::Rect;

pub(super) struct KeptAccessRows<K> {
    rows: HashMap<K, KeptRow>,
    walk: u64,
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
    walk: u64,
}

#[derive(PartialEq)]
struct RowAccessInputs {
    stamp: u64,
    index: usize,
    count: usize,
    bounds: Option<Rect>,
    focused: bool,
    selected: bool,
    menu: bool,
    disabled: bool,
    level: Option<usize>,
    expanded: Option<bool>,
    semantic: Option<String>,
    /// What the row is showing instead of the delegate's own row, if anything.
    state: RowStateStamp,
    renaming: Option<RenameStamp>,
}

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

    fn standing_open(&self, row: &MountedRow<M::Key>) -> bool {
        self.open.as_ref() == Some(&row.key)
    }

    pub(super) fn row_node(
        &self,
        row: &MountedRow<M::Key>,
        enabled: bool,
    ) -> crate::a11y::AccessNode {
        let count = self.model.len();
        let child = crate::a11y::stated_node(row.widget.as_ref());
        // The item standing open in its row is still the row: it is named by the title its
        // document states, never by the name of the field the document happens to begin with.
        let name = self
            .standing_open(row)
            .then(|| first_page_title(row.widget.as_ref()))
            .flatten()
            .or_else(|| child.as_ref().and_then(|node| node.name.clone()))
            .or_else(|| {
                matches!(&row.state, MountedRowState::Pending)
                    .then(|| widget_text("arriving-loading"))
            });
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
                .context_menu(self.menus.row_offers(&self.model, &row.key))
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
        if let Some(own) = child.as_ref().filter(|node| node.level.is_some()) {
            item.role = Role::TreeItem;
            item.level = own.level;
            item.state.expanded = own.state.expanded;
            item.value.clone_from(&own.value);
        }
        if let Some(marks) = &self.tree {
            item = item.with_level(marks.depth(&row.key).saturating_add(1) as usize);
            if marks.is_branch(&row.key) {
                item.state.expanded = Some(!marks.is_folded(&row.key));
            }
        }
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
        let owns_the_row = child.as_ref().is_some_and(|node| node.level.is_some());
        if (owns_the_row || self.semantic_key.is_none())
            && let Some(semantic) = child.as_ref().and_then(|node| node.semantic_key.clone())
        {
            item.key = Some(crate::a11y::hash_key(&semantic));
            item.semantic_key = Some(semantic);
        }
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
        children.extend(self.menu_node().map(Arc::new));
        if kept.rows.len() != reached {
            kept.rows.retain(|_, row| row.walk == walk);
        }
        children
    }

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
            menu: self.menus.row_offers(&self.model, &row.key),
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

    pub(super) fn headings_before(&self, child_index: usize) -> usize {
        let Some(row) = self.mounted.get(child_index) else {
            return 0;
        };
        self.tree_bands()
            .iter()
            .filter(|band| band.index <= row.index)
            .count()
    }

    fn tree_bands(&self) -> Vec<&Band> {
        let mut bands: Vec<&Band> = self.groups.bands.iter().collect();
        bands.sort_by_key(|band| band.index);
        bands
    }

    fn heading_node(&self, band: &Band, enabled: bool) -> AccessNode {
        let focused = self
            .focused_group
            .as_ref()
            .is_some_and(|(heading, _)| *heading == band.heading);
        AccessNode::new(Role::Button)
            .with_name(band.heading.clone())
            .with_value(self.heading_open_rows(band).to_string())
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
}

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
