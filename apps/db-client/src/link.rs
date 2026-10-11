//! The seam between the app's controller and its views.
//!
//! [`DbClientApp`](crate::app::DbClientApp) owns the session and every effect
//! on it. The views the window shell hosts (the navigator in the sidebar and
//! the workspace beside it) own their widgets and nothing else: each frame the
//! controller publishes a [`ViewModel`] describing what is on screen, and the
//! views answer with [`Intent`]s describing what the person asked for. Neither
//! side reaches into the other's state.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use crate::record::Workspace as Record;
use ui::framework::Dispatcher;
use uuid::Uuid;

use crate::model::{
    ConnectionProfile, DatabaseSchema, Engine, QueryResult, RelationRef, TableInfo,
};
use crate::session::{MainTab, RunStatus};

/// One saved connection as the navigator lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionRow {
    pub id: Uuid,
    pub name: String,
    pub engine: Engine,
    /// Where it connects: `host:port`, or the database file's name; empty
    /// for the bundled sample.
    pub address: String,
    /// The bundled sample cannot be removed.
    pub removable: bool,
}

/// A dialog the workspace presents over the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dialog {
    /// The new-connection form.
    Connect,
    /// Confirmation before a statement that changes data runs.
    ConfirmRun,
}

/// What is on screen, published by the controller.
#[derive(Clone)]
pub struct ViewModel {
    pub connections: Vec<ConnectionRow>,
    pub active: Option<Uuid>,
    /// The active connection has a live engine.
    pub connected: bool,
    /// A connect or query is in flight.
    pub busy: bool,
    pub tab: MainTab,
    pub schema: Rc<DatabaseSchema>,
    /// Bumped whenever `schema` changes shape.
    pub schema_revision: u64,
    pub selected_relation: Option<RelationRef>,
    pub status: RunStatus,
    pub query: Option<Arc<QueryResult>>,
    /// Bumped whenever a fresh query result lands.
    pub query_generation: usize,
    pub data: Option<Arc<QueryResult>>,
    pub data_generation: usize,
    /// Rows in the selected table, and the window of them `data` holds.
    pub data_total: i64,
    pub data_offset: usize,
    pub data_page_size: usize,
    pub table: Option<Rc<TableInfo>>,
    pub dialog: Option<Dialog>,
    /// The statement waiting for confirmation.
    pub pending_sql: String,
    /// Name of the active connection, for the window header.
    pub active_name: String,
}

impl Default for ViewModel {
    fn default() -> Self {
        Self {
            connections: Vec::new(),
            active: None,
            connected: false,
            busy: false,
            tab: MainTab::Query,
            schema: Rc::new(DatabaseSchema::default()),
            schema_revision: 0,
            selected_relation: None,
            status: RunStatus::Idle,
            query: None,
            query_generation: 0,
            data: None,
            data_generation: 0,
            data_total: 0,
            data_offset: 0,
            data_page_size: 100,
            table: None,
            dialog: None,
            pending_sql: String::new(),
            active_name: String::new(),
        }
    }
}

/// What a view asks the controller to do.
#[derive(Clone, Debug)]
pub enum Intent {
    /// Make a saved connection the active one and open it.
    Connect(Uuid),
    /// Open a saved connection or saved query a search hit or an agent
    /// names.
    OpenItem(Uuid),
    RemoveConnection(Uuid),
    /// Show the new-connection form.
    NewConnection,
    SaveConnection(Box<ConnectionProfile>),
    /// Open a table or view in the Data tab.
    OpenRelation(RelationRef),
    SelectTab(MainTab),
    /// Run the statement in the editor (asks first when it changes data).
    Run(String),
    /// Run the statement the confirmation holds.
    ConfirmRun,
    /// Close whichever dialog is open.
    CloseDialog,
    /// Replace the editor's text with a starter query.
    LoadSnippet(usize),
    /// Clear the error the last action ended with.
    DismissStatus,
    /// Show the previous or next page of the open table's rows.
    PreviousPage,
    NextPage,
}

#[derive(Default)]
struct Inner {
    model: ViewModel,
    intents: VecDeque<Intent>,
    /// The editor's text, mirrored by the workspace so the controller can run
    /// the statement a shortcut was pressed over.
    sql: String,
    /// Where the editor submits its edits of the shared SQL, once the
    /// framework dispatcher is attached.
    sql_edits: Option<Dispatcher<Record>>,
}

/// A shared handle on the model and the intent queue.
#[derive(Clone, Default)]
pub struct Link {
    inner: Rc<RefCell<Inner>>,
}

impl Link {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a new model.
    pub fn publish(&self, model: ViewModel) {
        self.inner.borrow_mut().model = model;
    }

    /// Read the model.
    pub fn read<R>(&self, read: impl FnOnce(&ViewModel) -> R) -> R {
        read(&self.inner.borrow().model)
    }

    /// Ask the controller for something.
    pub fn ask(&self, intent: Intent) {
        self.inner.borrow_mut().intents.push_back(intent);
    }

    /// Everything asked since the last call.
    pub fn take_intents(&self) -> Vec<Intent> {
        self.inner.borrow_mut().intents.drain(..).collect()
    }

    pub fn set_sql(&self, sql: &str) {
        let mut inner = self.inner.borrow_mut();
        if inner.sql != sql {
            sql.clone_into(&mut inner.sql);
        }
    }

    pub fn sql(&self) -> String {
        self.inner.borrow().sql.clone()
    }

    /// Hand the editor the dispatcher its edits of the SQL go through.
    pub fn attach_sql_edits(&self, dispatcher: Dispatcher<Record>) {
        self.inner.borrow_mut().sql_edits = Some(dispatcher);
    }

    /// The dispatcher the editor's edits go through, once attached.
    pub fn sql_edits(&self) -> Option<Dispatcher<Record>> {
        self.inner.borrow().sql_edits.clone()
    }
}
