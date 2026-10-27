//! The Query tab: a toolbar, the SQL editor, and under it the result of the
//! last run. The editor and the result share the tab's height and a person
//! moves the divider between them.

use std::cell::Cell as Flag;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use taffy::{Dimension, NodeId, TaffyTree};
use ui::TextRole;
use ui::framework::TextControl;
use ui::theme::Theme;
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    CodeEditor, Column, Dropdown, Element, EventCtx, Language, ResizableSplit, Row, Text, Widget,
};
use winit::event::WindowEvent;

use crate::i18n::t;
use crate::link::{Intent, Link};
use crate::model::QueryResult;
use crate::record::Workspace as Record;
use crate::results::{CaptionLine, ResultsTable, fill_remaining_height};
use crate::sample;
use crate::session::RunStatus;

/// The editor over the shared SQL buffer: it shows the text the document
/// holds and submits every edit as a splice of it through the workspace's
/// dispatcher ([`TextControl`]). Until that dispatcher is attached it only
/// shows the text.
pub struct EditorPane {
    text: TextControl<CodeEditor, Record>,
    /// Whether `text` edits through the dispatcher yet.
    attached: bool,
    link: Link,
}

impl EditorPane {
    pub fn new(link: &Link) -> Self {
        let mut editor = CodeEditor::local()
            .with_language(Language::Sql)
            .with_tab_width(2)
            .with_placeholder(&t("db-sql-placeholder"))
            .with_id("sql-editor");
        // The client opens ready to type a query.
        editor.set_focused(true);
        Self {
            text: TextControl::view_only_at(editor, Record::paths().sql),
            attached: false,
            link: link.clone(),
        }
    }

    /// The statement as typed.
    pub fn sql(&self) -> &str {
        self.text.control().value()
    }
}

impl ui::widget::Decorator for EditorPane {
    type Inner = TextControl<CodeEditor, Record>;

    fn inner(&self) -> &TextControl<CodeEditor, Record> {
        &self.text
    }

    fn inner_mut(&mut self) -> &mut TextControl<CodeEditor, Record> {
        &mut self.text
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if !self.attached
            && let Some(dispatcher) = self.link.sql_edits()
        {
            self.text.attach(dispatcher);
            self.attached = true;
        }
        Widget::sync(&mut self.text, state, path);
        self.link.set_sql(self.sql());
    }

    /// The editor takes the height its pane has, not just its lines.
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.text, taffy, theme, node_map);
        fill_remaining_height(taffy, node);
        // The pane is what the node stands for: an action addressed to the
        // editor reaches the text control, which submits the edit.
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        Widget::handle_event(&mut self.text, event, ctx);
        self.link.set_sql(self.sql());
    }

    fn name(&self) -> &'static str {
        "EditorPane"
    }
}

/// The starter queries: choosing one loads it into the editor, and the
/// dropdown goes back to its label.
struct StarterQueries {
    menu: Dropdown,
    /// A starter was just chosen.
    chosen: Rc<Flag<bool>>,
}

impl StarterQueries {
    fn new(link: &Link) -> Self {
        let names: Vec<String> = sample::starter_queries()
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        let chosen = Rc::new(Flag::new(false));
        let pick_link = link.clone();
        let pick_flag = Rc::clone(&chosen);
        let state = ui::LocalState::new(String::new());
        let menu = Dropdown::from_state(names.clone(), &state)
            .with_id("db.query.snippets")
            .with_accessible_name(&t("db-starter-queries"))
            .with_placeholder(&t("db-starter-queries"))
            .on_change(move |name| {
                if let Some(index) = names.iter().position(|candidate| candidate == name) {
                    pick_link.ask(Intent::LoadSnippet(index));
                    pick_flag.set(true);
                }
            });
        Self { menu, chosen }
    }
}

impl ui::widget::Decorator for StarterQueries {
    type Inner = Dropdown;

    fn inner(&self) -> &Dropdown {
        &self.menu
    }

    fn inner_mut(&mut self) -> &mut Dropdown {
        &mut self.menu
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.chosen.take() {
            self.menu.set_selected("");
        }
        Widget::sync(&mut self.menu, state, path);
    }

    /// The menu is as wide as a utility panel, not the whole toolbar.
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.menu, taffy, theme, node_map);
        if let Ok(mut style) = taffy.style(node).cloned() {
            style.max_size.width = Dimension::length(theme.tokens.metrics.panel_width_utility);
            let _ = taffy.set_style(node, style);
        }
        node
    }

    fn name(&self) -> &'static str {
        "StarterQueries"
    }
}

/// The starter queries. Run is the window bar's.
fn toolbar(link: &Link) -> Row {
    Row::new()
        .no_wrap()
        .push(Box::new(StarterQueries::new(link)))
}

/// What the strip above the result says.
fn strip_text(link: &Link) -> String {
    link.read(|model| match &model.status {
        RunStatus::Idle => t("db-ready"),
        RunStatus::Busy(message) | RunStatus::Ok(message) => message.clone(),
        RunStatus::Error(_) => model
            .query
            .as_ref()
            .map_or_else(String::new, |result| result.message()),
    })
}

/// What shows a result: a line saying what it returned, and the rows, or a
/// line saying so when the statement returned none.
fn results_content(link: &Link, result: Option<Arc<QueryResult>>) -> Element {
    let strip_link = link.clone();
    let strip = Text::dynamic(move || strip_text(&strip_link))
        .with_role(TextRole::Caption)
        .dimmed();
    let has_columns = result
        .as_ref()
        .is_some_and(|result| !result.columns.is_empty());
    let body: Element = if has_columns {
        let mut table = ResultsTable::new("db.query.results").reporting_to(link);
        table.bind(result.clone());
        // A cut result says so, and says what to do about it, above the grid
        // rather than through a "+" nobody can read.
        let cut = result.as_ref().and_then(|result| result.cut_note());
        match cut {
            Some(note) => Box::new(
                Column::new()
                    .with_no_padding()
                    .push(Box::new(CaptionLine::under_divider(
                        Text::new(&note).with_role(TextRole::Body).dimmed(),
                    )))
                    .push(Box::new(table)),
            ),
            None => Box::new(table),
        }
    } else {
        let message = match &result {
            Some(result) if result.is_select => t("db-query-no-columns"),
            Some(_) => t("db-statement-ran"),
            None => String::new(),
        };
        Box::new(CaptionLine::under_divider(
            Text::new(&message).with_role(TextRole::Body).dimmed(),
        ))
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .push(Box::new(CaptionLine::under_divider(strip)))
            .push(body),
    )
}

/// The result of the last run, rebuilt when a fresh one lands.
pub struct ResultsPane {
    link: Link,
    content: Element,
    bound: Option<usize>,
    structure_dirty: bool,
}

impl ResultsPane {
    pub fn new(link: &Link) -> Self {
        Self {
            link: link.clone(),
            content: results_content(link, None),
            bound: None,
            structure_dirty: false,
        }
    }
}

impl ui::widget::Decorator for ResultsPane {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (generation, result) = self
            .link
            .read(|model| (model.query_generation, model.query.clone()));
        if self.bound != Some(generation) {
            self.bound = Some(generation);
            self.content = results_content(&self.link, result);
            self.structure_dirty = true;
        }
        self.content.sync(state, path);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let rebuilt = std::mem::take(&mut self.structure_dirty);
        self.content.take_structure_dirty() || rebuilt
    }

    fn name(&self) -> &'static str {
        "ResultsPane"
    }
}

/// The whole tab: the toolbar over the editor and its result.
pub fn query_pane(link: &Link) -> Element {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_fill_remaining_height()
            .push(Box::new(toolbar(link)))
            .push(Box::new(
                ResizableSplit::new(
                    EditorPane::new(link),
                    ResultsPane::new(link),
                    SplitOrientation::Vertical,
                )
                .with_split_position(0.3),
            )),
    )
}
