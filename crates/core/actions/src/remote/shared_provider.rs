//! A shared provider is the provider ([`ActionProvider`] for `Arc<H>`).

use super::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ContentGets,
    ContentPuts, InvocationId, InvocationReceipt, InvocationState, InvokeRequest, LiveView,
    Pending, ResourceChanges, ResourceReads,
};

/// A shared provider is the provider: what lets one be handed to [`super::serve`], which
/// keeps a handle for answers that wait.
///
/// Every method is forwarded, [`Admitted`](super::Admitted) included: a handle kept to a provider
/// must not be a provider that lost what the dispatch told it about the call, or a call made
/// through the handle would start without the caller's cancellation and session.
impl<H: ActionProvider + ?Sized> ActionProvider for std::sync::Arc<H> {
    fn retire_execution(&self, caller: &Caller) {
        (**self).retire_execution(caller);
    }

    fn serves(&self, provider: &str) -> bool {
        (**self).serves(provider)
    }

    fn published(&self) -> Vec<crate::ResourceSpec> {
        (**self).published()
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

    fn observed(&self, caller: &Caller, id: &InvocationId, state: Option<&InvocationState>) {
        (**self).observed(caller, id, state);
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

    fn invocation(
        &self,
        address: &crate::ActionAddress,
        argument: Option<&str>,
    ) -> Option<crate::api::ActionDescriptor> {
        (**self).invocation(address, argument)
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: super::Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        (**self).run(caller, request, admitted)
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &super::Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        (**self).asks_first(caller, request, admitted)
    }

    fn watch(&self, caller: &Caller, request: &InvokeRequest) -> Result<LiveView, String> {
        (**self).watch(caller, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider that publishes one resource, and answers nothing.
    struct Publishes;

    impl ActionProvider for Publishes {
        fn published(&self) -> Vec<crate::ResourceSpec> {
            vec![crate::ResourceSpec::new("photos.library", "photos.library", "Photos")]
        }

        fn query(
            &self,
            _caller: &Caller,
            _query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::default()))
        }

        fn invoke(
            &self,
            _caller: &Caller,
            _request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }

        fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(
            &self,
            _caller: &Caller,
            _id: &InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownInvocation))
        }
    }

    /// A registration keeps a provider behind an `Arc`: what it publishes is
    /// still found there, so a call naming one of its addresses reaches it.
    #[test]
    fn a_shared_provider_publishes_what_it_publishes() {
        let shared: std::sync::Arc<dyn ActionProvider + Send + Sync> = std::sync::Arc::new(Publishes);
        assert_eq!(shared.published().len(), 1);
        assert!(!shared.serves("photos.library"));
    }
}
