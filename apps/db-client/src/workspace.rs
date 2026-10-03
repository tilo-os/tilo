//! The window's content: the tabs of the open connection, the view each one
//! shows, and the dialogs over them.

use std::sync::Arc;

use ui::TextRole;
use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    AppModel, Banner, Button, Column, ControlSize, EmptyState, ModalDialog, ModelPath,
    SegmentedStyle, Text, ToggleGroup, Widget,
};

use crate::connect_modal::{ConnectDialog, confirm_dialog};
use crate::frame::{Frame, FrameStyle, Place, Slot, frame_methods};
use crate::link::{Dialog, Intent, Link};
use crate::model::QueryResult;
use crate::query::QueryPane;
use crate::results::ResultsTable;
use crate::session::{MainTab, RunStatus};
use crate::structure::StructureView;

/// A quiet statement that a tab has nothing to show yet.
fn placeholder(title: &str, message: &str) -> EmptyState {
    EmptyState::new(title).with_message(message)
}

/// Which rows of the table are shown, and the page controls.
pub struct DataHeader {
    link: Link,
    caption: Text,
    previous: Button,
    next: Button,
    frame: Frame,
    structure_dirty: bool,
}

impl DataHeader {
    pub fn new(link: &Link) -> Self {
        let caption_link = link.clone();
        let back = link.clone();
        let forward = link.clone();
        Self {
            link: link.clone(),
            caption: Text::dynamic(move || caption_text(&caption_link))
                .with_role(TextRole::Caption)
                .dimmed(),
            previous: Button::icon_only("db.data.previous", Icon::ChevronLeft, "Previous rows")
                .control_size(ControlSize::Small)
                .on_view(move || back.ask(Intent::PreviousPage)),
            next: Button::icon_only("db.data.next", Icon::ChevronRight, "Next rows")
                .control_size(ControlSize::Small)
                .on_view(move || forward.ask(Intent::NextPage)),
            frame: Frame::default(),
            structure_dirty: false,
        }
    }

    fn slots(&mut self) -> Vec<Slot<'_>> {
        vec![
            Slot::new(&mut self.caption, Place::Grow),
            Slot::new(&mut self.previous, Place::Natural),
            Slot::new(&mut self.next, Place::Natural),
        ]
    }

    fn children_ref(&self) -> Vec<&dyn Widget> {
        vec![&self.caption, &self.previous, &self.next]
    }

    #[allow(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "every frame-based view gives its style through `frame_methods!`"
    )]
    fn frame_style(&mut self, theme: &Theme) -> FrameStyle {
        FrameStyle {
            gap: theme.tokens.spacing.s1,
            row: true,
            centred: true,
            ..FrameStyle::default()
        }
    }
}

impl Widget for DataHeader {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (has_previous, has_next) = self.link.read(|model| {
            let shown = model.data.as_ref().map_or(0, |data| data.rows.len());
            let total = usize::try_from(model.data_total.max(0)).unwrap_or(usize::MAX);
            (
                model.data_offset > 0,
                shown > 0 && model.data_offset.saturating_add(shown) < total,
            )
        });
        self.previous.set_enabled(has_previous);
        self.next.set_enabled(has_next);
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

    frame_methods!();

    fn name(&self) -> &'static str {
        "DataHeader"
    }
}

/// The rows of the selected table.
pub struct DataPane {
    link: Link,
    header: DataHeader,
    table: ResultsTable,
    nothing: EmptyState,
    bound: Option<usize>,
    has_rows: bool,
    frame: Frame,
    structure_dirty: bool,
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
            return "No rows".to_owned();
        }
        let from = i64::try_from(model.data_offset)
            .unwrap_or(0)
            .saturating_add(1);
        let to = from
            .saturating_add(shown)
            .saturating_sub(1)
            .min(total.max(shown));
        format!("Rows {from} to {to} of {}", total.max(shown))
    })
}

impl DataPane {
    pub fn new(link: &Link) -> Self {
        Self {
            link: link.clone(),
            header: DataHeader::new(link),
            table: ResultsTable::new("db.data.table"),
            nothing: placeholder("No table selected", "Choose a table to see its rows."),
            bound: None,
            has_rows: false,
            frame: Frame::default(),
            structure_dirty: false,
        }
    }

    fn slots(&mut self) -> Vec<Slot<'_>> {
        if self.has_rows {
            vec![
                Slot::new(&mut self.header, Place::Natural),
                Slot::new(&mut self.table, Place::Grow),
            ]
        } else {
            vec![Slot::new(&mut self.nothing, Place::Grow)]
        }
    }

    fn children_ref(&self) -> Vec<&dyn Widget> {
        if self.has_rows {
            vec![&self.header, &self.table]
        } else {
            vec![&self.nothing]
        }
    }

    #[allow(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "every frame-based view gives its style through `frame_methods!`"
    )]
    fn frame_style(&mut self, theme: &Theme) -> FrameStyle {
        FrameStyle {
            gap: theme.tokens.spacing.s2,
            ..FrameStyle::default()
        }
    }

    fn refresh(&mut self) {
        let (generation, data): (usize, Option<Arc<QueryResult>>) = self
            .link
            .read(|model| (model.data_generation, model.data.clone()));
        let has_rows = data.as_ref().is_some_and(|data| !data.columns.is_empty());
        if self.bound != Some(generation) || has_rows != self.has_rows {
            self.bound = Some(generation);
            self.has_rows = has_rows;
            self.table.bind(data);
            self.structure_dirty = true;
        }
    }
}

impl Widget for DataPane {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.refresh();
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

    frame_methods!();

    fn name(&self) -> &'static str {
        "DataPane"
    }
}

fn tab_index(tab: MainTab) -> usize {
    MainTab::all()
        .iter()
        .position(|candidate| *candidate == tab)
        .unwrap_or(0)
}

/// The content column of the window.
pub struct Workspace {
    link: Link,
    tabs: ToggleGroup,
    query: QueryPane,
    data: DataPane,
    structure: StructureView,
    nothing_to_inspect: EmptyState,
    no_connections: EmptyState,
    connect: ConnectDialog,
    confirm: ModalDialog<Column>,
    /// The error the last action ended with, and the banner showing it.
    banner: Option<(String, Banner)>,
    tab: MainTab,
    has_connections: bool,
    has_table: bool,
    frame: Frame,
    structure_dirty: bool,
}

impl Workspace {
    pub fn new(link: &Link) -> Self {
        let pick = link.clone();
        let tabs = ToggleGroup::new(
            MainTab::all()
                .iter()
                .map(|tab| Segment::new(tab.label()))
                .collect(),
            0,
        )
        .with_style(SegmentedStyle::Tabs)
        .with_id("db.tabs")
        .on_change(move |index| {
            if let Some(tab) = MainTab::all().get(index) {
                pick.ask(Intent::SelectTab(*tab));
            }
        });
        Self {
            link: link.clone(),
            tabs,
            query: QueryPane::new(link),
            data: DataPane::new(link),
            structure: StructureView::new(),
            nothing_to_inspect: placeholder(
                "No table selected",
                "Choose a table to see its structure.",
            ),
            no_connections: EmptyState::new("No connections yet")
                .with_icon(Icon::HardDrive)
                .with_message(
                    "Add a PostgreSQL, MySQL or SQLite database to browse its tables and run queries.",
                )
                .with_action("New connection")
                .with_action_id("db.new-connection"),
            connect: ConnectDialog::new(link),
            confirm: confirm_dialog(link),
            banner: None,
            tab: MainTab::Query,
            has_connections: false,
            has_table: false,
            frame: Frame::default(),
            structure_dirty: false,
        }
    }

    fn body_ref(&self) -> &dyn Widget {
        if !self.has_connections {
            return &self.no_connections;
        }
        match self.tab {
            MainTab::Query => &self.query,
            MainTab::Data => &self.data,
            MainTab::Structure if self.has_table => &self.structure,
            MainTab::Structure => &self.nothing_to_inspect,
        }
    }

    fn slots(&mut self) -> Vec<Slot<'_>> {
        let has_connections = self.has_connections;
        let connect_open = self.connect.dialog.is_open();
        let confirm_open = self.confirm.is_open();
        let mut slots = Vec::new();
        if connect_open {
            slots.push(Slot::new(&mut self.connect.dialog, Place::Overlay));
        }
        if confirm_open {
            slots.push(Slot::new(&mut self.confirm, Place::Overlay));
        }
        if has_connections {
            slots.push(Slot::new(&mut self.tabs, Place::Start));
        }
        if let Some((_, banner)) = self.banner.as_mut() {
            slots.push(Slot::new(banner, Place::Natural));
        }
        // The body is borrowed last: it is a different field from the rest,
        // but reaches them through `self`.
        let tab = self.tab;
        let has_table = self.has_table;
        let body: &mut dyn Widget = if has_connections {
            match tab {
                MainTab::Query => &mut self.query,
                MainTab::Data => &mut self.data,
                MainTab::Structure if has_table => &mut self.structure,
                MainTab::Structure => &mut self.nothing_to_inspect,
            }
        } else {
            &mut self.no_connections
        };
        slots.push(Slot::new(body, Place::Grow));
        slots
    }

    fn children_ref(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::new();
        if self.connect.dialog.is_open() {
            children.push(&self.connect.dialog);
        }
        if self.confirm.is_open() {
            children.push(&self.confirm);
        }
        if self.has_connections {
            children.push(&self.tabs);
        }
        if let Some((_, banner)) = self.banner.as_ref() {
            children.push(banner);
        }
        children.push(self.body_ref());
        children
    }

    #[allow(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "every frame-based view gives its style through `frame_methods!`"
    )]
    fn frame_style(&mut self, theme: &Theme) -> FrameStyle {
        FrameStyle {
            gap: theme.tokens.spacing.s3,
            padding: theme.tokens.spacing.s6,
            ..FrameStyle::default()
        }
    }

    /// Bring the view up to the published model.
    fn refresh(&mut self) {
        let (tab, has_connections, has_table, dialog) = self.link.read(|model| {
            (
                model.tab,
                !model.connections.is_empty(),
                model.table.is_some(),
                model.dialog,
            )
        });
        if tab != self.tab || has_connections != self.has_connections || has_table != self.has_table
        {
            self.tab = tab;
            self.has_connections = has_connections;
            self.has_table = has_table;
            self.structure_dirty = true;
        }
        if self.tabs.selected() != tab_index(tab) {
            self.tabs.set_selected(tab_index(tab));
        }
        let table = self.link.read(|model| model.table.clone());
        self.structure.show(table.as_ref());

        let connect_open = self.connect.dialog.is_open();
        match dialog {
            Some(Dialog::Connect) if !connect_open => {
                self.connect.open();
                self.structure_dirty = true;
            }
            Some(Dialog::ConfirmRun) if !self.confirm.is_open() => {
                self.confirm.set_open(true);
                self.structure_dirty = true;
            }
            _ => {}
        }
        if dialog != Some(Dialog::Connect) && connect_open {
            self.connect.close();
            self.structure_dirty = true;
        }
        if dialog != Some(Dialog::ConfirmRun) && self.confirm.is_open() {
            self.confirm.set_open(false);
            self.structure_dirty = true;
        }
        let error = self.link.read(|model| match &model.status {
            RunStatus::Error(message) => Some(message.clone()),
            _ => None,
        });
        match (&error, &self.banner) {
            (Some(message), Some((shown, _))) if message == shown => {}
            (Some(message), _) => {
                self.banner = Some((
                    message.clone(),
                    Banner::error(message.clone()).dismissible(),
                ));
                self.structure_dirty = true;
            }
            (None, Some(_)) => {
                self.banner = None;
                self.structure_dirty = true;
            }
            (None, None) => {}
        }
        if let Some((_, banner)) = self.banner.as_mut()
            && banner.take_dismissed()
        {
            self.link.ask(Intent::DismissStatus);
        }
        if !has_connections && self.no_connections.take_action() {
            self.link.ask(Intent::NewConnection);
        }
    }
}

impl Widget for Workspace {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.refresh();
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

    frame_methods!();

    fn name(&self) -> &'static str {
        "Workspace"
    }
}
