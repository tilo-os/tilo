//! Database client as a [`tilo_resource::Resource`]: connection profiles and
//! saved queries sync (passwords stay local); connect / run SQL are Effects.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    DB_ACTION_TYPE, DbAction, document_bound_actions, document_children, register,
    root_local_operations,
};
use crate::app::DbClientApp;
use crate::doc::{ConnectionsDoc, EditorDoc, SavedQueriesDoc, SavedQuery};
use crate::model::ConnectionProfile;

/// Shared catalogue projection (no passwords, no live session).
#[derive(Clone, Debug, Default)]
pub struct DbState {
    pub connections: Vec<ConnectionProfile>,
    pub saved_queries: Vec<SavedQuery>,
    /// Collaborative SQL buffer — binds "Save current query".
    pub editor_sql: String,
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
            editor_sql: EditorDoc::read_sql(doc),
        })
    }

    fn bound_actions(state: &DbState) -> Vec<ActionSpec> {
        document_bound_actions(state)
    }

    fn children(state: &DbState) -> Vec<PublishedResource> {
        document_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Run SQL is an Effect with no document delta. Connect rides each
        // connection child from [`children`].
        root_local_operations()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &DbState) -> Vec<SearchEntry> {
        let connections = state.connections.iter().map(|profile| SearchEntry {
            anchor: Some(format!("connection:{}", profile.id)),
            title: profile.name.clone(),
            text: format!("{} {}", profile.engine.as_key(), profile.host),
            action: None,
        });
        let queries = state.saved_queries.iter().map(|query| SearchEntry {
            anchor: Some(format!("saved-query:{}", query.id)),
            title: query.name.clone(),
            text: query.sql.clone(),
            action: None,
        });
        connections.chain(queries).collect()
    }
}

/// Hosted-only extras that are not document state: an in-flight job and the
/// current query result. The workspace door stays on [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &DbClientApp) -> Vec<PublishedResource> {
    let mut extras = Vec::new();
    if let Some(label) = app.live_job_label() {
        extras.push(PublishedResource::new(
            "database.job",
            "database.job",
            label,
        ));
    }
    if let Some(result) = app.live_query_result_resource() {
        extras.push(result);
    }
    extras
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
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<DbClient>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<DbClient>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
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

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

/// Live launch through the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    // Window title / capture protection live in main.rs via configure_app
    // (native App only). The browser mounts the definition without that path.
    ui::framework::launch_spec::<DbClientShell>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{add_connection_action, provide_resources, save_query_action};
    use crate::model::Engine;
    use tilo_actions::Action;
    use ui::widget::Widget;
    use uuid::Uuid;

    #[test]
    fn resource_publish_owns_workspace_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        let mut profile = ConnectionProfile::new("Analytics", Engine::Postgres);
        profile.host = "db.internal".into();
        add_connection_action(&profile).apply(&mut doc).unwrap();
        save_query_action(Uuid::new_v4(), "Daily totals", "SELECT 1")
            .apply(&mut doc)
            .unwrap();
        EditorDoc::set_sql(&mut doc, "SELECT * FROM customers");
        doc.commit();

        let state = DbClient::read(&doc).expect("seeded db state");
        let published = ui::tilo_resource::publish::<DbClient>("database", "Database", &state);
        assert_eq!(published.kind, "database.workspace");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "save_query"),
            "save_query bound_action must be on the framework resource"
        );
        assert!(
            published.actions.iter().any(|action| action.id == "query"),
            "query Effect must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "database.connection"),
            "connection children must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "database.saved-query"),
            "saved-query children must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .all(|child| child.kind != "database.query-result"),
            "shared projection must not carry live query results"
        );

        let mut app = DbClientApp::new();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone[0].kind, "database.workspace");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "database.connection"),
            "standalone publishes connection children"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "database.workspace"),
            "hosted must not republish database.workspace beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "database.workspace");
    }
}
