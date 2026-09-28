//! Database client as a [`tilo_resource::Resource`]: connection profiles and
//! saved queries sync (passwords stay local); connect / run SQL are Effects.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::{Element, Widget};

use crate::actions::{DB_ACTION_TYPE, DbAction, register};
use crate::app::DbClientApp;
use crate::doc::{ConnectionsDoc, SavedQueriesDoc, SavedQuery};
use crate::model::ConnectionProfile;

/// Shared catalogue projection (no passwords, no live session).
#[derive(Clone, Debug, Default)]
pub struct DbState {
    pub connections: Vec<ConnectionProfile>,
    pub saved_queries: Vec<SavedQuery>,
}

/// The database workspace resource.
pub struct DbClient;

impl Resource for DbClient {
    const KIND: &'static str = "database.workspace";
    const LABEL: &'static str = "Database";
    const ACTION_TYPE: &'static str = DB_ACTION_TYPE;

    type State = DbState;
    type Action = DbAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<DbState, ModelError> {
        Ok(DbState {
            connections: ConnectionsDoc::read(doc),
            saved_queries: SavedQueriesDoc::read(doc),
        })
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Connect / run-query Effects from the live catalogue door. There is
        // no outbox: a run is not undone by an inverse document action; the
        // SQL text and saved profiles are what sync.
        let app = DbClientApp::new();
        let doc = ui::automerge::AutoCommit::new();
        app.resources(&doc)
            .into_iter()
            .flat_map(|resource| resource.actions)
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn children(state: &DbState) -> Vec<tilo_actions::Resource> {
        let _ = state;
        // Nested connection / schema / query rows stay on the surface's
        // Widget::resources builders until each becomes its own Resource type.
        Vec::new()
    }

    fn search(state: &DbState) -> Vec<SearchEntry> {
        let connections = state.connections.iter().map(|profile| SearchEntry {
            anchor: Some(format!("database.connection:{}", profile.id)),
            title: profile.name.clone(),
            text: format!("{} {}", profile.engine.as_key(), profile.host),
        });
        let queries = state.saved_queries.iter().map(|query| SearchEntry {
            anchor: Some(format!("database.saved-query:{}", query.id)),
            title: query.name.clone(),
            text: query.sql.clone(),
        });
        connections.chain(queries).collect()
    }
}

/// Framework app: the database client widget over [`DbClient`].
pub struct DbClientShell;

impl ResourceApp for DbClientShell {
    type Resource = DbClient;
    const ID: &'static str = "so.tilo.db-client";
    const TITLE: &'static str = "Database Client";
    const ICON: Option<&'static str> = Some("db-client");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<DbClient>) -> Element {
        cx.surface(DbClientApp::new())
    }
}

impl ResourceSurface<DbClient> for DbClientApp {
    fn present(&mut self, state: &Rc<DbState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<DbClient>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for saved connections and saved queries.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<DbClient>>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<DbClient>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &DbAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        let _ = self.try_dispatch_with_context(doc, type_id, action, context);
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &DbAction,
        context: ui::tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => dispatcher.apply_now_with(doc, action, context),
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    /// Hosted commits are recorded by the dispatcher. Standalone tests still
    /// read the local host's rising edge.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }
}

/// Live launch through the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<DbClientShell>().configure_app(|app| {
        app.set_window_title(Some("Database Client".into()));
    })
}
