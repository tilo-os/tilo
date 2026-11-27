//! The dispatch a turn of a workspace's agent run on this device calls through: the device's
//! broker, with every call of the agent's going where the calls of its other runs go. A call
//! that stops for its person's yes, when it is asked or after an app took it, waits as their
//! question ([`DeviceAsked`]), is recorded as the device's invocation, and is followed there.

use std::sync::Arc;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, CataloguePage, CatalogueQuery,
    InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_app_host::collab_broker::AppActionBroker;
use tilo_invocations::Invocations;

use super::asked::{DeviceAsked, Run};

/// The device's broker, as an agent's hosted turn calls it.
pub(super) struct TurnDispatch {
    broker: AppActionBroker,
    host: Arc<dyn ActionProvider + Send + Sync>,
    asked: Arc<DeviceAsked>,
    invocations: Arc<Invocations>,
}

impl TurnDispatch {
    pub(super) fn new(
        broker: AppActionBroker,
        asked: Arc<DeviceAsked>,
        invocations: Arc<Invocations>,
    ) -> Self {
        Self {
            host: broker.device_host(),
            broker,
            asked,
            invocations,
        }
    }

    /// The run `caller` makes its calls in, when it is an agent's in a workspace.
    fn agents_run(caller: &Caller) -> Option<Run> {
        Some(Run::Agent {
            workspace: caller.workspace()?.to_owned(),
            agent: caller.acting().agent_id()?.to_owned(),
            name: caller.agent_called()?.to_owned(),
        })
    }
}

impl ActionProvider for TurnDispatch {
    fn serves(&self, provider: &str) -> bool {
        self.host.serves(provider)
    }

    fn published(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.host.published()
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.host.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // Answering a question is never asked itself: the person's answer decides it.
        if tilo_actions::remote::asked::is_resolve(&request.target) {
            return self.host.invoke(caller, request);
        }
        match Self::agents_run(caller) {
            Some(run) => self.asked.send(
                run,
                caller,
                (Arc::clone(&self.host), caller.clone()),
                request,
            ),
            None => self.host.invoke(caller, request),
        }
    }

    fn tool_started(
        &self,
        caller: &Caller,
        id: &InvocationId,
        tool: &str,
    ) -> Option<tilo_actions::remote::RunningTool> {
        self.host.tool_started(caller, id, tool)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.invocations
            .observe(caller, id)
            .or_else(|| self.host.observe(caller, id))
    }

    fn observed(&self, caller: &Caller, id: &InvocationId, state: Option<&InvocationState>) {
        self.host.observed(caller, id, state);
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if self.invocations.get(caller, id).is_some() {
            return self.invocations.next_change(caller, id);
        }
        self.host.next_change(caller, id)
    }

    fn session_state(&self, caller: &Caller) -> Option<tilo_actions::remote::SessionState> {
        self.host.session_state(caller)
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        self.host.session_state_changed(caller)
    }

    fn changed(
        &self,
        caller: &Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.host.changed(caller, scope)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        let workspace = caller.workspace().unwrap_or_default();
        let state = self.broker.state();
        let may_write =
            |caller: &Caller, origin: &tilo_action_services::workspace_invocations::Origin| {
                state.may_write_origin(caller, workspace, origin)
            };
        if let Some(stopping) = self.invocations.cancel(caller, id, &may_write) {
            return stopping;
        }
        self.host.cancel(caller, id)
    }

    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.host.content_puts()
    }

    fn content_puts_for(
        &self,
        target: Option<&tilo_actions::remote::RemoteActionTarget>,
    ) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.host.content_puts_for(target)
    }

    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        self.host.content_gets()
    }

    fn introduced(&self, caller: &Caller, client: &str) {
        self.host.introduced(caller, client);
    }

    fn here(&self, caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        self.host.here(caller)
    }

    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.host.resource_reads()
    }

    fn store_commands(&self) -> Option<Arc<dyn tilo_actions::remote::StoreCommands>> {
        self.host.store_commands()
    }

    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.host.change_feed()
    }

    fn installed_surface(&self, app_id: &str) -> Pending<Option<Vec<u8>>> {
        self.host.installed_surface(app_id)
    }

    fn invocation(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<tilo_actions::api::ActionDescriptor> {
        self.host.invocation(address, argument)
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.host.run(caller, request, admitted)
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.host.asks_first(caller, request, admitted)
    }

    fn watch(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::LiveView, String> {
        self.host.watch(caller, request)
    }
}
