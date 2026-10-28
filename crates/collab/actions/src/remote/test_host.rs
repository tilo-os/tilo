//! An [`ActionHost`] for tests, built from closures: what stands in for a
//! session or a peer when a test only needs it to answer one way and to
//! remember what it was asked.

use std::sync::Mutex;

use tilo_lock::MutexRecover as _;

use super::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus,
};
use crate::outcome::{ActionOutput, InvocationId};

type Catalogue =
    dyn Fn(&Caller, CatalogueQuery) -> Result<CataloguePage, ActionHostError> + Send + Sync;
type Answer = dyn Fn(&Caller, &InvokeRequest) -> Pending<Result<InvocationReceipt, ActionHostError>>
    + Send
    + Sync;

/// A host that keeps every request it is asked to invoke, and every caller
/// that asked anything, and answers from closures. Unless told otherwise it
/// offers an empty catalogue and completes every invocation at once with an
/// empty output. It observes nothing (each answer is final) and cancels
/// anything.
pub struct TestHost {
    catalogue: Box<Catalogue>,
    answer: Box<Answer>,
    asked: Mutex<Vec<InvokeRequest>>,
    callers: Mutex<Vec<Caller>>,
}

impl Default for TestHost {
    fn default() -> Self {
        Self::new()
    }
}

impl TestHost {
    /// A host offering nothing that completes every invocation.
    #[must_use]
    pub fn new() -> Self {
        Self {
            catalogue: Box::new(|_, _| Ok(CataloguePage::default())),
            answer: Box::new(|_, request| {
                Pending::ready(Ok(InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Completed {
                        output: ActionOutput::default(),
                    },
                }))
            }),
            asked: Mutex::new(Vec::new()),
            callers: Mutex::new(Vec::new()),
        }
    }

    /// Answer a catalogue query with what `catalogue` makes of it.
    #[must_use]
    pub fn with_catalogue(
        mut self,
        catalogue: impl Fn(&Caller, CatalogueQuery) -> Result<CataloguePage, ActionHostError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.catalogue = Box::new(catalogue);
        self
    }

    /// Complete each invocation at once with the output `output` makes of
    /// its request.
    #[must_use]
    pub fn completing(
        self,
        output: impl Fn(&InvokeRequest) -> ActionOutput + Send + Sync + 'static,
    ) -> Self {
        self.answering(move |_, request| {
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: output(request),
                },
            }))
        })
    }

    /// Answer each invocation with what `answer` makes of it: a refusal, a
    /// failure, or an answer the test hands over later.
    #[must_use]
    pub fn answering(
        mut self,
        answer: impl Fn(&Caller, &InvokeRequest) -> Pending<Result<InvocationReceipt, ActionHostError>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.answer = Box::new(answer);
        self
    }

    /// The requests asked so far, oldest first.
    pub fn asked(&self) -> Vec<InvokeRequest> {
        self.asked.lock_recover().clone()
    }

    /// The requests asked so far, forgetting them.
    pub fn take_asked(&self) -> Vec<InvokeRequest> {
        std::mem::take(&mut *self.asked.lock_recover())
    }

    /// Who asked, query or invocation, oldest first.
    pub fn callers(&self) -> Vec<Caller> {
        self.callers.lock_recover().clone()
    }
}

impl ActionHost for TestHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.callers.lock_recover().push(caller.clone());
        Pending::ready((self.catalogue)(caller, query))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.callers.lock_recover().push(caller.clone());
        let answer = (self.answer)(caller, &request);
        self.asked.lock_recover().push(request);
        answer
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        Pending::ready(Ok(()))
    }
}
