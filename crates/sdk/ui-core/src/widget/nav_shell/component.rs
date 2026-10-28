//! [`NavShell`] as a described component.

use crate::component::{Component, Described, Event, PropType, SlotViews, Type, Value};
use crate::widget::Element;

use super::{Column, ContentKind, NavPane, NavShell};

/// The one window shell: a sidebar of places, an overview list, the content and an inspector,
/// side by side while they fit and one at a time, with Back, when they do not. The header of
/// each column holds its title, its count and its actions.
#[derive(Component, Default)]
#[component(name = "NavShell", role = "Window", label = "title")]
pub struct NavShellProps {
    /// What the content is, which sets the narrowest it still works at: `page` unless said.
    pub content_kind: ContentKind,
    /// The content's title, in its header.
    pub title: Option<String>,
    /// How many things the content lists, beside its title.
    pub count: Option<i64>,
    /// The overview column's title.
    pub overview_title: Option<String>,
    /// How many things the overview lists, beside its title.
    pub overview_count: Option<i64>,
    /// The sidebar's title: what the places are of, the app's root.
    pub sidebar_title: Option<String>,
    /// Whether the content's Back goes back now (a drill-down the app keeps).
    pub can_go_back: bool,
    /// The places of the app: a navigation list.
    #[component(slot)]
    pub sidebar: Option<Element>,
    /// The list the content is chosen from.
    #[component(slot)]
    pub overview: Option<Element>,
    /// What the window is for.
    #[component(slot(default))]
    pub content: Option<Element>,
    /// Details of what the content shows, beside it.
    #[component(slot)]
    pub inspector: Option<Element>,
    /// The content's actions, in its header.
    #[component(slot)]
    pub actions: Vec<Element>,
    /// The overview's actions, in its header: what the list's owner offers.
    #[component(slot)]
    pub overview_actions: Vec<Element>,
    /// The column in front of a narrow window.
    #[component(state)]
    pub pane: Option<NavPane>,
    /// The content's Back was pressed.
    #[component(event)]
    pub back: Event,
}

impl PropType for ContentKind {
    fn ty() -> Type {
        Type::Choice {
            name: "ContentKind".into(),
            cases: ["list", "reading", "page", "grid", "document", "canvas"]
                .map(String::from)
                .to_vec(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let (Value::Choice(case) | Value::Text(case)) = value else {
            return Err(format!("takes ContentKind, not {}", value.kind()));
        };
        Ok(match case.as_str() {
            "list" => Self::List,
            "reading" => Self::Reading,
            "page" => Self::Page,
            "grid" => Self::Grid,
            "document" => Self::Document,
            "canvas" => Self::Canvas,
            other => return Err(format!("has no case `{other}`")),
        })
    }
}

impl PropType for NavPane {
    fn ty() -> Type {
        Type::Choice {
            name: "NavPane".into(),
            cases: ["sidebar", "overview", "content"]
                .map(String::from)
                .to_vec(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let (Value::Choice(case) | Value::Text(case)) = value else {
            return Err(format!("takes NavPane, not {}", value.kind()));
        };
        Ok(match case.as_str() {
            "sidebar" => Self::Sidebar,
            "overview" => Self::Overview,
            "content" => Self::Content,
            other => return Err(format!("has no case `{other}`")),
        })
    }
}

fn count_of(count: Option<i64>) -> Option<usize> {
    count.map(|count| usize::try_from(count.max(0)).unwrap_or(usize::MAX))
}

impl Described for NavShell {
    type Props = NavShellProps;

    fn from_props(key: &str, props: NavShellProps) -> Self {
        let NavShellProps {
            content_kind,
            title,
            count,
            overview_title,
            overview_count,
            sidebar_title,
            can_go_back,
            sidebar,
            overview,
            content,
            inspector,
            actions,
            overview_actions,
            pane,
            back,
        } = props;
        let mut shell = Self::new().with_id(key).content_kind(content_kind);
        if let Some(sidebar) = sidebar {
            shell = shell.sidebar(sidebar);
        }
        if let Some(overview) = overview {
            shell = shell.overview(overview);
        }
        if let Some(content) = content {
            shell = shell.content(content);
        }
        if let Some(inspector) = inspector {
            shell = shell.inspector(inspector);
        }
        if let Some(title) = sidebar_title {
            shell = shell.sidebar_header(title, Vec::new());
        }
        if overview_title.is_some() || !overview_actions.is_empty() {
            shell = shell.overview_header(overview_title.unwrap_or_default(), overview_actions);
        }
        if let Some(count) = count_of(overview_count) {
            shell = shell.overview_count(count);
        }
        if title.is_some() || !actions.is_empty() {
            shell = shell.content_header(title.unwrap_or_default(), actions);
        }
        if let Some(count) = count_of(count) {
            shell = shell.content_count(count);
        }
        if let Some(mut go_back) = back.handler() {
            let can = std::rc::Rc::new(std::cell::Cell::new(can_go_back));
            shell.can_back = Some(std::rc::Rc::clone(&can));
            shell = shell.on_back(move || can.get(), move || go_back());
        }
        if let Some(pane) = pane {
            shell.show(pane);
            shell.snap();
        }
        shell
    }

    /// Titles, counts and whether Back goes back change in place; the columns and header
    /// actions change through their slots. Another kind of content builds the shell again.
    fn set_props(&mut self, props: NavShellProps) -> bool {
        if self.inputs.borrow().kind != props.content_kind {
            return false;
        }
        let back_given = !props.back.is_none();
        if back_given != self.can_back.is_some() {
            return false;
        }
        self.set_sidebar_title(props.sidebar_title.unwrap_or_default());
        self.set_overview_title(props.overview_title.unwrap_or_default());
        self.set_overview_count(count_of(props.overview_count));
        self.set_content_title(props.title.unwrap_or_default());
        self.set_content_count(count_of(props.count));
        if let Some(can) = &self.can_back {
            can.set(props.can_go_back);
        }
        self.set_can_go_back(props.can_go_back);
        true
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        self.finish_slot_edit();
        let column = match slot {
            "sidebar" => Some(Column::Sidebar),
            "overview" => Some(Column::Overview),
            "content" => Some(Column::Content),
            "inspector" => Some(Column::Inspector),
            _ => None,
        };
        if let Some(column) = column {
            self.slot_edit = Some(SlotEdit::Column(column));
            return Some(self);
        }
        let header = match slot {
            "actions" => Column::Content,
            "overview_actions" => Column::Overview,
            _ => return None,
        };
        let staged = matches!(self.pending.actions.get(header.index()), Some(Some(_)));
        let actions = self.take_header_actions(header);
        self.slot_edit = Some(SlotEdit::Actions(
            header,
            actions,
            ActionsEdit {
                staged,
                changed: false,
            },
        ));
        Some(self)
    }
}

/// A change of one slot under way ([`Described::slot_views`]).
pub(super) enum SlotEdit {
    /// The body of a column: one view, or none.
    Column(Column),
    /// A header's actions, out of the header until the change is done.
    Actions(Column, Vec<Element>, ActionsEdit),
}

/// Where a header's actions under change came from, and whether the change touched the row.
#[derive(Clone, Copy)]
pub(super) struct ActionsEdit {
    /// They were waiting for the next build, not shown.
    staged: bool,
    /// An action came, went, moved or was built anew: the row changes.
    changed: bool,
}

impl NavShell {
    /// The actions of `column`'s header, out of it: the ones waiting to be shown, or the ones
    /// shown.
    fn take_header_actions(&mut self, column: Column) -> Vec<Element> {
        if let Some(Some(staged)) = self.pending.actions.get_mut(column.index()) {
            return std::mem::take(staged);
        }
        self.headers
            .get_mut(column.index())
            .map(|header| std::mem::take(&mut header.actions))
            .unwrap_or_default()
    }

    /// Put the actions of the change under way back. A change that touched the row stages it
    /// for the next build; one whose actions each took their change in place (a label, whether
    /// one is enabled) puts them back where they were, and the row stays as it is.
    pub(super) fn finish_slot_edit(&mut self) {
        let Some(SlotEdit::Actions(column, actions, edit)) = self.slot_edit.take() else {
            return;
        };
        if !edit.changed && !edit.staged {
            if let Some(header) = self.headers.get_mut(column.index()) {
                header.actions = actions;
            }
            return;
        }
        if let Some(slot) = self.pending.actions.get_mut(column.index()) {
            *slot = Some(actions);
        }
        if edit.changed {
            self.pending_changed();
        }
    }

    /// Note that the actions under change touched the row.
    fn actions_changed(&mut self) {
        if let Some(SlotEdit::Actions(_, _, edit)) = &mut self.slot_edit {
            edit.changed = true;
        }
    }

    /// Whether `column` has a body at the next build.
    fn has_column_body(&self, column: Column) -> bool {
        self.has_body(column, super::at(&self.pending.bodies, column).as_ref())
    }

    /// The body of `column` as the shell holds it: the one waiting to be shown, if a swap is
    /// pending.
    fn column_body_mut(&mut self, column: Column) -> Option<&mut Element> {
        if super::at(&self.pending.bodies, column).is_some() {
            return super::at_mut(&mut self.pending.bodies, column)
                .as_mut()
                .and_then(|super::Staged(pending)| pending.as_mut());
        }
        match self.body_slot(column) {
            super::nothing_listed::BodySlot::Optional(slot) => slot.as_mut(),
            super::nothing_listed::BodySlot::Required(slot) => Some(slot),
        }
    }

    fn edit_actions(&mut self) -> Option<&mut Vec<Element>> {
        match &mut self.slot_edit {
            Some(SlotEdit::Actions(_, actions, _)) => Some(actions),
            _ => None,
        }
    }

    fn edit_column(&self) -> Option<Column> {
        match &self.slot_edit {
            Some(SlotEdit::Column(column)) => Some(*column),
            _ => None,
        }
    }
}

/// The views of the slot the shell is changing: a column's body, or a header's actions.
impl SlotViews for NavShell {
    fn len(&self) -> usize {
        match &self.slot_edit {
            Some(SlotEdit::Actions(_, actions, _)) => actions.len(),
            Some(SlotEdit::Column(column)) => usize::from(self.has_column_body(*column)),
            None => 0,
        }
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        match &self.slot_edit {
            Some(SlotEdit::Column(column)) => {
                let column = *column;
                if index == 0 {
                    self.column_body_mut(column)
                } else {
                    None
                }
            }
            Some(SlotEdit::Actions(..)) => self.edit_actions()?.get_mut(index),
            None => None,
        }
    }

    fn insert(&mut self, index: usize, view: Element) {
        if let Some(actions) = self.edit_actions() {
            actions.insert(index.min(actions.len()), view);
            self.actions_changed();
            return;
        }
        if let Some(column) = self.edit_column() {
            self.stage_body(column, Some(view));
        }
    }

    fn remove(&mut self, index: usize) {
        if let Some(SlotEdit::Actions(_, actions, edit)) = &mut self.slot_edit {
            if index < actions.len() {
                let gone = actions.remove(index);
                self.retired_actions.push(gone);
                edit.changed = true;
            }
            return;
        }
        if let Some(column) = self.edit_column() {
            if column == Column::Content {
                self.stage_body(column, Some(Box::new(crate::widget::Spacer::new())));
            } else {
                self.stage_body(column, None);
            }
        }
    }

    fn move_view(&mut self, from: usize, to: usize) {
        if let Some(actions) = self.edit_actions()
            && from < actions.len()
        {
            let moved = actions.remove(from);
            actions.insert(to.min(actions.len()), moved);
            self.actions_changed();
        }
    }

    fn replace(&mut self, index: usize, view: Element) {
        if let Some(SlotEdit::Actions(_, actions, edit)) = &mut self.slot_edit {
            if let Some(held) = actions.get_mut(index) {
                let gone = std::mem::replace(held, view);
                self.retired_actions.push(gone);
                edit.changed = true;
            }
            return;
        }
        if let Some(column) = self.edit_column() {
            self.stage_body(column, Some(view));
        }
    }
}
