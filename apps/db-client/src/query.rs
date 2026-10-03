//! The Query tab: a toolbar, the SQL editor, and under it the result of the
//! last run. The editor and the result share the tab's height and a person
//! moves the divider between them.

use std::cell::Cell as Flag;
use std::rc::Rc;
use std::sync::Arc;

use ui::Shortcut;
use ui::TextRole;
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Button, ButtonRole, CodeEditor, Dropdown, EventCtx, Language, ResizableSplit, Text, Widget,
};
use winit::event::WindowEvent;

use crate::doc::EditorDoc;
use crate::frame::{Frame, FrameStyle, Place, Slot, frame_methods};
use crate::link::{Intent, Link};
use crate::results::ResultsTable;
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        Widget::handle_event(&mut self.editor, event, ctx);
        self.link.set_sql(self.editor.value());
    }

    fn name(&self) -> &'static str {
        "EditorPane"
    }
}

/// Run and the starter queries.
pub struct QueryToolbar {
    link: Link,
    run: Button,
    snippets: Dropdown,
    /// A snippet was just chosen: the dropdown goes back to its label.
    chosen: Rc<Flag<bool>>,
    /// How wide the starter-queries menu is allowed to be.
    menu_width: f32,
    frame: Frame,
    structure_dirty: bool,
}

impl QueryToolbar {
    pub fn new(link: &Link) -> Self {
        let run_link = link.clone();
        let run = Button::new("db.query.run", "Run", None)
            .role(ButtonRole::Primary)
            .shortcut_hint(Shortcut::command("Enter"))
            .on_click(move || {
                let sql = run_link.sql();
                run_link.ask(Intent::Run(sql));
            });
        let names: Vec<String> = sample::starter_queries()
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        let chosen = Rc::new(Flag::new(false));
        let pick_link = link.clone();
        let pick_flag = Rc::clone(&chosen);
        let state = ui::LocalState::new(String::new());
        let snippets = Dropdown::from_state(names.clone(), &state)
            .with_id("db.query.snippets")
            .with_accessible_name("Starter queries")
            .with_placeholder("Starter queries")
            .on_change(move |name| {
                if let Some(index) = names.iter().position(|candidate| candidate == name) {
                    pick_link.ask(Intent::LoadSnippet(index));
                    pick_flag.set(true);
                }
            });
        Self {
            link: link.clone(),
            run,
            snippets,
            chosen,
            menu_width: 0.0,
            frame: Frame::default(),
            structure_dirty: false,
        }
    }

    fn slots(&mut self) -> Vec<Slot<'_>> {
        vec![
            Slot::new(&mut self.run, Place::Natural),
            Slot::new(&mut self.snippets, Place::Capped(self.menu_width)),
        ]
    }

    fn children_ref(&self) -> Vec<&dyn Widget> {
        vec![&self.run, &self.snippets]
    }

    fn frame_style(&mut self, theme: &ui::theme::Theme) -> FrameStyle {
        self.menu_width = theme.tokens.metrics.panel_width_utility;
        FrameStyle {
            gap: theme.tokens.spacing.s2,
            row: true,
            centred: true,
            ..FrameStyle::default()
        }
    }
}

impl Widget for QueryToolbar {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let enabled = self.link.read(|model| model.connected && !model.busy);
        self.run.set_enabled(enabled);
        if self.chosen.take() {
            self.snippets.set_selected("");
        }
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

    frame_methods!();

    fn name(&self) -> &'static str {
        "QueryToolbar"
    }
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

/// The result of the last run: a line saying what it returned, and the rows.
pub struct ResultsPane {
    link: Link,
    strip: Text,
    table: ResultsTable,
    /// Shown instead of the table when the statement returned no columns.
    message: Text,
    bound: Option<usize>,
    has_columns: bool,
    frame: Frame,
    structure_dirty: bool,
}

impl ResultsPane {
    pub fn new(link: &Link) -> Self {
        let strip_link = link.clone();
        let message_link = link.clone();
        Self {
            link: link.clone(),
            strip: Text::dynamic(move || strip_text(&strip_link))
                .with_role(TextRole::Caption)
                .dimmed(),
            table: ResultsTable::new("db.query.results"),
            message: Text::dynamic(move || {
                message_link.read(|model| match &model.query {
                    Some(result) if result.is_select => "The query returned no columns.".to_owned(),
                    Some(_) => "The statement ran.".to_owned(),
                    None => String::new(),
                })
            })
            .with_role(TextRole::Body)
            .dimmed(),
            bound: None,
            has_columns: false,
            frame: Frame::default(),
            structure_dirty: false,
        }
    }

    /// The table showing the result, for tests.
    pub fn table(&self) -> &ResultsTable {
        &self.table
    }

    fn slots(&mut self) -> Vec<Slot<'_>> {
        let mut slots = vec![Slot::new(&mut self.strip, Place::Natural)];
        if self.has_columns {
            slots.push(Slot::new(&mut self.table, Place::Grow));
        } else {
            slots.push(Slot::new(&mut self.message, Place::Natural));
        }
        slots
    }

    fn children_ref(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.strip];
        if self.has_columns {
            children.push(&self.table);
        } else {
            children.push(&self.message);
        }
        children
    }

    #[allow(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "every frame-based view gives its style through `frame_methods!`"
    )]
    fn frame_style(&mut self, theme: &ui::theme::Theme) -> FrameStyle {
        FrameStyle {
            gap: theme.tokens.spacing.s2,
            ..FrameStyle::default()
        }
    }

    fn refresh(&mut self) {
        let (generation, result): (usize, Option<Arc<crate::model::QueryResult>>) = self
            .link
            .read(|model| (model.query_generation, model.query.clone()));
        if self.bound != Some(generation) {
            self.bound = Some(generation);
            self.has_columns = result
                .as_ref()
                .is_some_and(|result| !result.columns.is_empty());
            self.table.bind(result);
            self.structure_dirty = true;
        }
    }
}

impl Widget for ResultsPane {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.refresh();
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

    frame_methods!();

    fn name(&self) -> &'static str {
        "ResultsPane"
    }
}

/// The whole tab.
pub struct QueryPane {
    toolbar: QueryToolbar,
    split: ResizableSplit<EditorPane, ResultsPane>,
    frame: Frame,
    structure_dirty: bool,
}

impl QueryPane {
    pub fn new(link: &Link) -> Self {
        Self {
            toolbar: QueryToolbar::new(link),
            split: ResizableSplit::new(
                EditorPane::new(link),
                ResultsPane::new(link),
                SplitOrientation::Vertical,
            )
            .with_split_position(0.3),
            frame: Frame::default(),
            structure_dirty: false,
        }
    }

    fn slots(&mut self) -> Vec<Slot<'_>> {
        vec![
            Slot::new(&mut self.toolbar, Place::Natural),
            Slot::new(&mut self.split, Place::Grow),
        ]
    }

    fn children_ref(&self) -> Vec<&dyn Widget> {
        vec![&self.toolbar, &self.split]
    }

    #[allow(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "every frame-based view gives its style through `frame_methods!`"
    )]
    fn frame_style(&mut self, theme: &ui::theme::Theme) -> FrameStyle {
        FrameStyle {
            gap: theme.tokens.spacing.s3,
            ..FrameStyle::default()
        }
    }
}

impl Widget for QueryPane {
    frame_methods!();

    fn name(&self) -> &'static str {
        "QueryPane"
    }
}
