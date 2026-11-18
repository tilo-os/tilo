//! A Notes that keeps no search of its own, for what the agent surface finds in an app that
//! answers no search.
use super::*;

/// [`Notes`] publishing everything it does, and no search of its own: a
/// provider that holds no records to search.
#[derive(Clone)]
pub(super) struct NoSearch(Notes);

impl NoSearch {
    pub(super) fn new(notes: &Notes) -> Self {
        Self(notes.clone())
    }
}

impl ActionProvider for NoSearch {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.0.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.0.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.0.observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.0.cancel(caller, id)
    }
}
