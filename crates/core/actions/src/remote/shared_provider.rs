//! A shared provider is the provider ([`ActionProvider`] for `Arc<H>`).

use super::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ContentGets,
    ContentPuts, InvocationId, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    ResourceChanges, ResourceReads,
};

/// A shared provider is the provider: what lets one be handed to [`super::serve`], which
/// keeps a handle for answers that wait.
impl<H: ActionProvider + ?Sized> ActionProvider for std::sync::Arc<H> {
    fn retire_execution(&self, caller: &Caller) {
        (**self).retire_execution(caller);
    }

    fn serves(&self, provider: &str) -> bool {
        (**self).serves(provider)
    }

    fn introduced(&self, caller: &Caller, client: &str) {
        (**self).introduced(caller, client);
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        (**self).query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        (**self).invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        (**self).observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        (**self).next_change(caller, id)
    }

    fn session_state(&self, caller: &Caller) -> Option<super::SessionState> {
        (**self).session_state(caller)
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        (**self).session_state_changed(caller)
    }

    fn ended(&self, caller: &Caller) -> Option<String> {
        (**self).ended(caller)
    }

    fn changed(
        &self,
        caller: &Caller,
        scope: super::WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        (**self).changed(caller, scope)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        (**self).cancel(caller, id)
    }

    fn content_puts(&self) -> Option<std::sync::Arc<dyn ContentPuts>> {
        (**self).content_puts()
    }

    fn content_gets(&self) -> Option<std::sync::Arc<dyn ContentGets>> {
        (**self).content_gets()
    }

    fn resource_reads(&self) -> Option<std::sync::Arc<dyn ResourceReads>> {
        (**self).resource_reads()
    }

    fn resource_changes(&self) -> Option<std::sync::Arc<dyn ResourceChanges>> {
        (**self).resource_changes()
    }

    fn resource_search(&self) -> Option<std::sync::Arc<dyn super::ResourceSearch>> {
        (**self).resource_search()
    }

    fn change_feed(&self) -> Option<std::sync::Arc<dyn super::ChangeFeed>> {
        (**self).change_feed()
    }

    fn here(&self, caller: &Caller) -> Option<Vec<super::Here>> {
        (**self).here(caller)
    }

    fn installed_surface(&self, app_id: &str) -> Pending<Option<Vec<u8>>> {
        (**self).installed_surface(app_id)
    }
}
