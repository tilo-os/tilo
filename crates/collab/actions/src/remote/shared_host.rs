//! A shared host is the host ([`ActionHost`] for `Arc<H>`).

use super::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    ContentPuts, InvocationId, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    RecordReads,
};

/// A shared host is the host: what lets one be handed to [`super::serve`], which
/// keeps a handle for answers that wait.
impl<H: ActionHost + ?Sized> ActionHost for std::sync::Arc<H> {
    fn retire_execution(&self, caller: &Caller) {
        (**self).retire_execution(caller);
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        (**self).query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        (**self).invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        (**self).observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        (**self).next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        (**self).cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionHostError>> {
        (**self).confirm(caller, request)
    }

    fn content_puts(&self) -> Option<std::sync::Arc<dyn ContentPuts>> {
        (**self).content_puts()
    }

    fn record_reads(&self) -> Option<std::sync::Arc<dyn RecordReads>> {
        (**self).record_reads()
    }
}
