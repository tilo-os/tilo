//! An app's own connection to the daemon, resolving the calls it names by address.
//!
//! An app that calls another app's action writes the settled address of it
//! (`so.tilo.notes/notes/n1#find`), never a provider it chose: which window of the app
//! answers, and which resource the names walk to, is the dispatch's to find. It walks the
//! address with the same tree an agent's call and the command line use
//! ([`tilo_actions::tree::Tree::target_at`]) and calls what that found as the app, so every
//! way in reaches one resource and one action.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, CataloguePage, CatalogueQuery, ChangeFeed,
    ContentGets, ContentPuts, Here, InvocationReceipt, InvocationState, InvokeRequest, LiveView,
    Pending, RemoteActionTarget, ResourceReads, RunningTool, SessionState, StoreCommands,
    WatchScope,
};
use tilo_actions::tree::{Pause, Tree};

/// The provider an app's line is served by, with the walk of an address over it.
pub(crate) struct Paths {
    inner: Arc<dyn ActionProvider + Send + Sync>,
    workspace: String,
}

impl Paths {
    /// `inner`, the workspace's action provider, answering the addresses a request names.
    pub(crate) fn wrapping(
        inner: Arc<dyn ActionProvider + Send + Sync>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            workspace: workspace.into(),
        }
    }
}

impl ActionProvider for Paths {
    fn serves(&self, provider: &str) -> bool {
        self.inner.serves(provider)
    }

    fn published(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.inner.published()
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.inner.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        mut request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if request.target.provider != tilo_actions::remote::PATH_PROVIDER {
            return self.inner.invoke(caller, request);
        }
        let path = format!(
            "{}#{}",
            request.target.address.resource, request.target.address.action
        );
        let workspace = caller
            .workspace()
            .map_or_else(|| self.workspace.clone(), str::to_owned);
        let tree = Tree::new(
            Arc::clone(&self.inner),
            caller.clone(),
            workspace,
            Arc::new(Paused(std::time::Instant::now())),
        );
        // The walk may ask the workspace where the app runs and wait on a directory that is
        // being read: it runs to its answer here, on the connection's own thread, before the
        // action it found is admitted as the app.
        let arguments = match &request.argument {
            tilo_actions::remote::InvokeArgument::Json(arguments) => arguments.clone(),
            _ => serde_json::Map::new(),
        };
        let found = async move { tree.target_at(&path, &arguments).await };
        match super::workspaces::block_on_owned(found) {
            Ok(target) => {
                request.target = target;
                self.inner.invoke(caller, request)
            }
            Err(reason) => Pending::ready(Err(ActionCallError::Refused { reason })),
        }
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.inner.observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.inner.cancel(caller, id)
    }

    fn retire_execution(&self, caller: &Caller) {
        self.inner.retire_execution(caller);
    }

    fn observed(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
        state: Option<&InvocationState>,
    ) {
        self.inner.observed(caller, id, state);
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.inner.next_change(caller, id)
    }

    fn session_state(&self, caller: &Caller) -> Option<SessionState> {
        self.inner.session_state(caller)
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        self.inner.session_state_changed(caller)
    }

    fn changed(
        &self,
        caller: &Caller,
        scope: WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.inner.changed(caller, scope)
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        self.inner.content_puts()
    }

    fn content_puts_for(
        &self,
        target: Option<&RemoteActionTarget>,
    ) -> Option<Arc<dyn ContentPuts>> {
        self.inner.content_puts_for(target)
    }

    fn content_gets(&self) -> Option<Arc<dyn ContentGets>> {
        self.inner.content_gets()
    }

    fn here(&self, caller: &Caller) -> Option<Vec<Here>> {
        self.inner.here(caller)
    }

    fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
        self.inner.resource_reads()
    }

    fn store_commands(&self) -> Option<Arc<dyn StoreCommands>> {
        self.inner.store_commands()
    }

    fn change_feed(&self) -> Option<Arc<dyn ChangeFeed>> {
        self.inner.change_feed()
    }

    fn installed_surface(&self, app_id: &str) -> Pending<Option<Vec<u8>>> {
        self.inner.installed_surface(app_id)
    }

    fn invocation(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<tilo_actions::api::ActionDescriptor> {
        self.inner.invocation(address, argument)
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.inner.run(caller, request, admitted)
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.inner.asks_first(caller, request, admitted)
    }

    fn watch(&self, caller: &Caller, request: &InvokeRequest) -> Result<LiveView, String> {
        self.inner.watch(caller, request)
    }

    fn tool_started(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
        tool: &str,
    ) -> Option<RunningTool> {
        self.inner.tool_started(caller, id, tool)
    }
}

/// The walk's clock: tokio's, as the waits of the daemon the connection is served by are.
struct Paused(std::time::Instant);

impl Pause for Paused {
    fn pause(
        &self,
        how_long: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(how_long))
    }

    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}
