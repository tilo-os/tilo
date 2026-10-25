//! The window's content: the tabs of the open connection, the view each one
//! shows, and the dialogs over them.
//!
//! The workspace is one toolkit [`Column`] whose children follow the model: the
//! tab strip, a banner while an action has failed, the view the tab shows, and
//! a dialog while one is open. The views that hold state (the tab strip and the
//! three tabs) are kept while they are out of sight, so a tab comes back as it
//! was left.

use std::collections::HashMap;
use std::sync::Arc;

use taffy::{NodeId, TaffyTree};
use ui::TextRole;
use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::{
    AppModel, Banner, Button, Column, ControlSize, Element, EmptyState, ModelPath, Row, Spacer,
    Text, Widget,
};

use crate::connect_modal::{confirm_dialog, connect_dialog};
use crate::i18n::{t, t_args};
use crate::link::{Dialog, Intent, Link};
use crate::model::QueryResult;
use crate::query::query_pane;
use crate::results::{CaptionLine, ResultsTable, fill_remaining_height};
use crate::session::{MainTab, RunStatus};
use crate::structure::StructureView;

/// A statement that a view has nothing to show yet, or, with an action, that
/// the app has nothing at all. It takes the room its parent has left.
struct Placeholder {
    empty: EmptyState,
}

impl Placeholder {
    /// "No table selected" and what to do about it.
    fn no_table(message: &str) -> Self {
        Self {
            empty: EmptyState::new(&t("db-no-table")).with_message(message),
        }
    }

    /// The first run: no connections, and the one way to add one.
    fn first_run(link: &Link) -> Self {
        let link = link.clone();
        Self {
            empty: EmptyState::new(&t("db-no-connections"))
                .with_icon(Icon::HardDrive)
                .with_message(&t("db-no-connections-hint"))
                .with_action(&t("db-new-connection"))
                .with_action_id("db.new-connection")
                .on_view(move || link.ask(Intent::NewConnection)),
        }
    }
}

impl ui::widget::Decorator for Placeholder {
    type Inner = EmptyState;

    fn inner(&self) -> &EmptyState {
        &self.empty
    }

    fn inner_mut(&mut self) -> &mut EmptyState {
        &mut self.empty
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.empty, taffy, theme, node_map);
        fill_remaining_height(taffy, node);
        // The placeholder is the thing the node stands for: it is what the
        // action is addressed through.
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn name(&self) -> &'static str {
        "Placeholder"
    }
}

/// The failure the last action ended with, until it is dismissed.
struct ErrorBanner {
    link: Link,
    banner: Banner,
}

impl ErrorBanner {
    fn new(link: &Link, message: &str) -> Self {
        Self {
            link: link.clone(),
            banner: Banner::error(message).dismissible(),
        }
    }
}

impl ui::widget::Decorator for ErrorBanner {
    type Inner = Banner;

    fn inner(&self) -> &Banner {
        &self.banner
    }

    fn inner_mut(&mut self) -> &mut Banner {
        &mut self.banner
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.banner.take_dismissed() {
            self.link.ask(Intent::DismissStatus);
        }
        Widget::sync(&mut self.banner, state, path);
    }

    fn name(&self) -> &'static str {
        "ErrorBanner"
    }
}

/// "Rows 1 to 25 of 347".
fn caption_text(link: &Link) -> String {
    link.read(|model| {
        let Some(data) = &model.data else {
            return String::new();
        };
        let total = model.data_total.max(0);
        let shown = i64::try_from(data.rows.len()).unwrap_or(i64::MAX);
        if shown == 0 {
            return t("db-no-rows");
        }
        let from = i64::try_from(model.data_offset)
            .unwrap_or(0)
            .saturating_add(1);
        let to = from
            .saturating_add(shown)
            .saturating_sub(1)
            .min(total.max(shown));
        t_args(
            "db-rows-range",
            &[
                ("from", ui::i18n::ArgValue::from(from)),
                ("to", ui::i18n::ArgValue::from(to)),
                ("total", ui::i18n::ArgValue::from(total.max(shown))),
            ],
        )
    })
}

/// A page control: it is there when there is a page to go to.
struct PageButton {
    link: Link,
    button: Button,
    forward: bool,
}

impl PageButton {
    fn new(link: &Link, forward: bool) -> Self {
        let ask = link.clone();
        let (id, icon, label, intent) = if forward {
            (
                "db.data.next",
                Icon::ChevronRight,
                &t("db-next-rows"),
                Intent::NextPage,
            )
        } else {
            (
                "db.data.previous",
                Icon::ChevronLeft,
                &t("db-previous-rows"),
                Intent::PreviousPage,
            )
        };
        Self {
            link: link.clone(),
            button: Button::icon_only(id, icon, label)
                .control_size(ControlSize::Small)
                .on_view(move || ask.ask(intent.clone())),
            forward,
        }
    }
}

impl ui::widget::Decorator for PageButton {
    type Inner = Button;

    fn inner(&self) -> &Button {
        &self.button
    }

    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let available = self.link.read(|model| {
            if self.forward {
                let shown = model.data.as_ref().map_or(0, |data| data.rows.len());
                let total = usize::try_from(model.data_total.max(0)).unwrap_or(usize::MAX);
                shown > 0 && model.data_offset.saturating_add(shown) < total
            } else {
                model.data_offset > 0
            }
        });
        self.button.set_enabled(available);
        Widget::sync(&mut self.button, state, path);
    }

    fn name(&self) -> &'static str {
        "PageButton"
    }
}

/// Which rows of the table are shown, and the page controls.
fn data_header(link: &Link) -> Row {
    let caption_link = link.clone();
    Row::new()
        .no_wrap()
        .push(Box::new(CaptionLine::new(
            Text::dynamic(move || caption_text(&caption_link))
                .with_role(TextRole::Caption)
                .dimmed(),
        )))
        .push(Box::new(Spacer::new()))
        .push(Box::new(PageButton::new(link, false)))
        .push(Box::new(PageButton::new(link, true)))
}

/// What the Data tab shows: the header over the rows, or the statement that
/// there are none.
fn data_content(link: &Link, data: Option<Arc<QueryResult>>) -> Element {
    let has_rows = data.as_ref().is_some_and(|data| !data.columns.is_empty());
    if !has_rows {
        return Box::new(Placeholder::no_table(&t("db-choose-table-rows")));
    }
    let mut table = ResultsTable::new("db.data.table");
    table.bind(data);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_fill_remaining_height()
            .push(Box::new(data_header(link)))
            .push(Box::new(table)),
    )
}

/// The rows of the selected table, rebuilt when a new page lands.
struct DataPane {
    link: Link,
    content: Element,
    bound: Option<usize>,
    structure_dirty: bool,
}

impl DataPane {
    fn new(link: &Link) -> Self {
        Self {
            link: link.clone(),
            content: data_content(link, None),
            bound: None,
            structure_dirty: false,
        }
    }
}

impl ui::widget::Decorator for DataPane {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (generation, data) = self
            .link
            .read(|model| (model.data_generation, model.data.clone()));
        if self.bound != Some(generation) {
            self.bound = Some(generation);
            self.content = data_content(&self.link, data);
            self.structure_dirty = true;
        }
        self.content.sync(state, path);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let rebuilt = std::mem::take(&mut self.structure_dirty);
        self.content.take_structure_dirty() || rebuilt
    }

    fn name(&self) -> &'static str {
        "DataPane"
    }
}

/// What the workspace's column holds, in order.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Part {
    /// The message of the failure it reports.
    Banner(String),
    FirstRun,
    Query,
    Data,
    Structure,
    /// The Data or Structure tab with no table open.
    NoTable(MainTab),
    Dialog(Dialog),
}

impl Part {
    /// A part that holds what a person did, and so waits out of sight rather
    /// than being rebuilt.
    fn is_kept(&self) -> bool {
        matches!(self, Self::Query | Self::Data | Self::Structure)
    }
}

/// The content column of the window.
pub struct Workspace {
    link: Link,
    column: Column,
    /// What `column` holds now.
    shown: Vec<Part>,
    /// The parts that hold state, while they are not in the column.
    kept: HashMap<Part, Element>,
    structure_dirty: bool,
}

impl Workspace {
    pub fn new(link: &Link) -> Self {
        let mut workspace = Self {
            link: link.clone(),
            column: Column::new(),
            shown: Vec::new(),
            kept: HashMap::new(),
            structure_dirty: false,
        };
        workspace.arrange();
        workspace
    }

    /// The parts the model asks for.
    fn wanted(&self) -> Vec<Part> {
        self.link.read(|model| {
            let mut parts = Vec::new();
            if model.connections.is_empty() {
                parts.push(Part::FirstRun);
            } else {
                if let RunStatus::Error(message) = &model.status {
                    parts.push(Part::Banner(message.clone()));
                }
                parts.push(match model.tab {
                    MainTab::Query => Part::Query,
                    MainTab::Data if model.table.is_some() => Part::Data,
                    MainTab::Structure if model.table.is_some() => Part::Structure,
                    tab => Part::NoTable(tab),
                });
            }
            parts.extend(model.dialog.map(Part::Dialog));
            parts
        })
    }

    fn make(&self, part: &Part) -> Element {
        let link = &self.link;
        match part {
            Part::Banner(message) => Box::new(ErrorBanner::new(link, message)),
            Part::FirstRun => Box::new(Placeholder::first_run(link)),
            Part::Query => query_pane(link),
            Part::Data => Box::new(DataPane::new(link)),
            Part::Structure => Box::new(StructureView::new(link)),
            Part::NoTable(MainTab::Data) => {
                Box::new(Placeholder::no_table(&t("db-choose-table-rows")))
            }
            Part::NoTable(_) => Box::new(Placeholder::no_table(&t("db-choose-table-structure"))),
            Part::Dialog(Dialog::Connect) => Box::new(connect_dialog(link)),
            Part::Dialog(Dialog::ConfirmRun) => Box::new(confirm_dialog(link)),
        }
    }

    /// Put the column's children in line with the model.
    fn arrange(&mut self) {
        let wanted = self.wanted();
        if wanted == self.shown {
            return;
        }
        let old = self.column.take_children();
        for (part, element) in std::mem::take(&mut self.shown).into_iter().zip(old) {
            if part.is_kept() {
                self.kept.insert(part, element);
            }
        }
        let children = wanted
            .iter()
            .map(|part| self.kept.remove(part).unwrap_or_else(|| self.make(part)))
            .collect();
        self.column.set_children(children);
        self.shown = wanted;
        self.structure_dirty = true;
    }
}

impl ui::widget::Decorator for Workspace {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.column
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.column
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.arrange();
        Widget::sync(&mut self.column, state, path);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let rebuilt = std::mem::take(&mut self.structure_dirty);
        Widget::take_structure_dirty(&mut self.column) || rebuilt
    }

    fn name(&self) -> &'static str {
        "Workspace"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::{ConnectionRow, ViewModel};
    use crate::model::Engine;
    use ui::headless::FrameHarness;
    use ui::theme::Theme;
    use uuid::Uuid;

    fn connection() -> ConnectionRow {
        ConnectionRow {
            id: Uuid::new_v4(),
            name: "Chinook sample".into(),
            engine: Engine::Sqlite,
            address: String::new(),
            removable: false,
        }
    }

    fn open_model(tab: MainTab) -> ViewModel {
        ViewModel {
            connections: vec![connection()],
            connected: true,
            tab,
            ..ViewModel::default()
        }
    }

    fn shown(harness: &FrameHarness) -> Vec<Part> {
        harness.root::<Workspace>().shown.clone()
    }

    /// The column holds what the model asks for: the first run with no connections, then the tabs over the tab's view, a banner while an action has failed, and a dialog while one is open.
    #[test]
    fn the_workspace_holds_what_the_model_asks_for() {
        let link = Link::new();
        let mut harness = FrameHarness::for_root(
            ui::State::new(),
            Workspace::new(&link),
            (900, 700),
            Theme::dark(),
        );

        // --- phase: no_connections_is_the_first_run ---
        {
            harness.relayout();
            assert_eq!(shown(&harness), [Part::FirstRun]);
        }

        // --- phase: a_connection_brings_the_query_view ---
        {
            link.publish(open_model(MainTab::Query));
            harness.relayout();
            assert_eq!(shown(&harness), [Part::Query]);
        }

        // --- phase: a_tab_without_a_table_says_so ---
        {
            link.publish(open_model(MainTab::Data));
            harness.relayout();
            assert_eq!(shown(&harness), [Part::NoTable(MainTab::Data)]);
        }

        // --- phase: a_failure_shows_a_banner_above_the_view ---
        {
            link.publish(ViewModel {
                status: RunStatus::Error("Connection refused".into()),
                ..open_model(MainTab::Query)
            });
            harness.relayout();
            assert_eq!(
                shown(&harness),
                [Part::Banner("Connection refused".into()), Part::Query]
            );
        }

        // --- phase: a_dialog_comes_after_the_view_and_goes_when_closed ---
        {
            link.publish(ViewModel {
                dialog: Some(Dialog::Connect),
                ..open_model(MainTab::Query)
            });
            harness.relayout();
            assert_eq!(
                shown(&harness),
                [Part::Query, Part::Dialog(Dialog::Connect)]
            );
            link.publish(open_model(MainTab::Query));
            harness.relayout();
            assert_eq!(shown(&harness), [Part::Query]);
        }
    }

    /// A tab that is not showing keeps what a person did in it: the query view comes back as it was left.
    #[test]
    fn a_tab_comes_back_as_it_was_left() {
        let link = Link::new();
        link.publish(open_model(MainTab::Query));
        let mut harness = FrameHarness::for_root(
            ui::State::new(),
            Workspace::new(&link),
            (900, 700),
            Theme::dark(),
        );
        harness.relayout();
        let before = harness.targets()["db.query.snippets"];

        link.publish(open_model(MainTab::Data));
        harness.relayout();
        assert!(
            harness.root::<Workspace>().kept.contains_key(&Part::Query),
            "the query view waits out of sight"
        );
        assert!(!harness.targets().contains_key("db.query.snippets"));

        link.publish(open_model(MainTab::Query));
        harness.relayout();
        assert_eq!(harness.targets()["db.query.snippets"], before);
        assert!(!harness.root::<Workspace>().kept.contains_key(&Part::Query));
    }
}
