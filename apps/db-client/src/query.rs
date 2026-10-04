//! The Query tab: a toolbar, the SQL editor, and under it the result of the
//! last run. The editor and the result share the tab's height and a person
//! moves the divider between them.

use std::cell::Cell as Flag;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use taffy::{Dimension, NodeId, TaffyTree};
use ui::Shortcut;
use ui::TextRole;
use ui::theme::Theme;
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Button, ButtonRole, CodeEditor, Column, Dropdown, Element, EventCtx, Language, ResizableSplit,
    Row, Text, Widget,
};
use winit::event::WindowEvent;

use crate::doc::EditorDoc;
use crate::link::{Intent, Link};
use crate::model::QueryResult;
use crate::results::{CaptionLine, ResultsTable, fill_remaining_height};
use crate::sample;
use crate::session::RunStatus;

/// The editor, bound to the shared SQL buffer in the document.
pub struct EditorPane {
    editor: CodeEditor,
    link: Link,
}

impl EditorPane {
    pub fn new(link: &Link) -> Self {
        let mut editor = CodeEditor::local()
            .with_language(Language::Sql)
            .with_tab_width(2)
            .with_placeholder("Write a SQL query")
            .with_id("sql-editor");
        // The client opens ready to type a query.
        editor.set_focused(true);
        Self {
            editor,
            link: link.clone(),
        }
    }

    /// The statement as typed.
    pub fn sql(&self) -> &str {
        self.editor.value()
    }
}

impl ui::widget::Decorator for EditorPane {
    type Inner = CodeEditor;

    fn inner(&self) -> &CodeEditor {
        &self.editor
    }

    fn inner_mut(&mut self) -> &mut CodeEditor {
        &mut self.editor
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (doc, _) = ui::doc::document(state, path);
        let text = EditorDoc::sql_text(doc);
        self.editor.bind_text(text);
        self.editor.pull(doc);
        self.link.set_sql(self.editor.value());
    }

    /// The editor takes the height its pane has, not just its lines.
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.editor, taffy, theme, node_map);
        fill_remaining_height(taffy, node);
        node
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        Widget::handle_event(&mut self.editor, event, ctx);
        self.link.set_sql(self.editor.value());
    }

    fn name(&self) -> &'static str {
        "EditorPane"
    }
}

/// Run, which waits for a connection that is not busy.
struct RunButton {
    link: Link,
    button: Button,
}

impl RunButton {
    fn new(link: &Link) -> Self {
        let run_link = link.clone();
        let button = Button::new("db.query.run", "Run", None)
            .role(ButtonRole::Primary)
            .shortcut_hint(Shortcut::command("Enter"))
            .on_effect(move || {
                let sql = run_link.sql();
                run_link.ask(Intent::Run(sql));
            });
        Self {
            link: link.clone(),
            button,
        }
    }
}

impl ui::widget::Decorator for RunButton {
    type Inner = Button;

    fn inner(&self) -> &Button {
        &self.button
    }

    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let enabled = self.link.read(|model| model.connected && !model.busy);
        self.button.set_enabled(enabled);
        Widget::sync(&mut self.button, state, path);
    }

    fn name(&self) -> &'static str {
        "RunButton"
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
            .with_accessible_name("Starter queries")
            .with_placeholder("Starter queries")
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

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
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

/// Run and the starter queries.
fn toolbar(link: &Link) -> Row {
    Row::new()
        .no_wrap()
        .push(Box::new(RunButton::new(link)))
        .push(Box::new(StarterQueries::new(link)))
}

/// What the strip above the result says.
fn strip_text(link: &Link) -> String {
    link.read(|model| match &model.status {
        RunStatus::Idle => "Ready".to_owned(),
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
        let mut table = ResultsTable::new("db.query.results");
        table.bind(result);
        Box::new(table)
    } else {
        let message = match &result {
            Some(result) if result.is_select => "The query returned no columns.",
            Some(_) => "The statement ran.",
            None => "",
        };
        Box::new(CaptionLine::under_divider(
            Text::new(message).with_role(TextRole::Body).dimmed(),
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

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
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
