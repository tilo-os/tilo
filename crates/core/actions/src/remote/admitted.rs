//! What a device's dispatch hands a provider registered on it for a call it already admitted,
//! beyond the call itself ([`ActionProvider::run`](super::ActionProvider::run)), and the live
//! result such a provider may serve ([`ActionProvider::watch`](super::ActionProvider::watch)).
//!
//! The dispatch decides the caller's grant, asks the person where the action's contract says so,
//! and keeps the invocation's record; the provider only carries the call out. Most providers need
//! nothing beyond the call. A provider whose call makes calls of its own (an agent session's
//! programs) makes them through [`Admitted::through`], so its session admits each in turn; one
//! whose contract says it may be cancelled stops when [`Admitted::stop`] is set.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueRow,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, ReceiptStatus,
};
use crate::api::{ActionDescriptor, PlacedAction};
use crate::{ActionAddress, ActionFailure, ActionOutput, InvocationId};

/// How a call a registered provider carried out ended: its output, or why it failed.
pub type Outcome = Result<ActionOutput, Box<ActionFailure>>;

/// What the dispatch knows of a call it admitted for a provider registered on the device.
#[derive(Clone)]
pub struct Admitted {
    /// The provider the call's own further calls go through: an agent session's, when the call is
    /// one of its programs, so the session admits what the program does as it admits the agent.
    pub through: Option<Arc<dyn ActionProvider + Send + Sync>>,
    /// Set when the caller cancels a call whose contract says it may be
    /// ([`crate::ExecutionMode::cancellable`]): the provider stops, and the call ends cancelled.
    pub stop: Arc<AtomicBool>,
}

impl Default for Admitted {
    fn default() -> Self {
        Self {
            through: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// A provider that only the device's dispatch calls, once it admitted the call: what it offers,
/// and how it carries a call out. The rest of [`ActionProvider`] follows from these: a call ends in
/// its receipt, so there is no invocation left to observe or cancel once it answers.
pub trait Admits {
    /// The provider id its rows name.
    const PROVIDER: &'static str;
    /// The application its rows are offered for, whose grants decide them.
    const APP: &'static str;

    /// The actions it offers.
    fn rows(&self) -> Vec<PlacedAction>;

    /// Start `request`, which the dispatch admitted for `caller`.
    ///
    /// # Errors
    ///
    /// Why the call is refused before it starts.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String>;

    /// The action `address` is with `argument`, where it differs from its row
    /// ([`ActionProvider::invocation`]).
    fn described(
        &self,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        let _ = (address, argument);
        None
    }

    /// Follow an admitted call of a live action ([`ActionProvider::watch`]).
    ///
    /// # Errors
    ///
    /// The action has no live result here, or `caller` may not follow it.
    fn live(&self, caller: &Caller, request: &InvokeRequest) -> Result<LiveView, String> {
        let _ = (caller, request);
        Err("this action has no live result".into())
    }
}

impl<T: Admits> ActionProvider for T {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        offered(T::PROVIDER, T::APP, self.rows(), &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.run(caller, request, Admitted::default())
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    fn invocation(
        &self,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        self.described(address, argument)
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        settled(request.invocation, self.start(caller, &request, &admitted))
    }

    fn watch(&self, caller: &Caller, request: &InvokeRequest) -> Result<LiveView, String> {
        self.live(caller, request)
    }
}

/// A result that stays current ([`crate::ExecutionMode::Live`]), as a provider serves it.
pub struct LiveView {
    /// The whole result as it is now. A provider that reads it on a thread of its own answers
    /// when it has; one that has it at hand answers at once ([`Pending::ready`]).
    pub read: Box<dyn FnMut() -> Pending<LiveRead> + Send>,
    /// Asked before each read: resolves once the result may have changed after it. A provider that
    /// stopped serving the result drops it unanswered, which ends the view.
    pub changes: Box<dyn FnMut() -> Pending<()> + Send>,
}

/// One read of a [`LiveView`]: the result now, or why it failed for good.
pub type LiveRead = Result<serde_json::Value, Box<ActionFailure>>;

/// The receipt of `invocation` once `started` ends: the call's outcome, or the refusal that kept it
/// from starting.
#[must_use]
pub fn settled(
    invocation: InvocationId,
    started: Result<Pending<Outcome>, String>,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    match started {
        Err(reason) => Pending::ready(Err(ActionCallError::Refused { reason })),
        Ok(started) => started.map(move |outcome| {
            Ok(InvocationReceipt {
                invocation,
                status: match outcome {
                    Ok(output) => ReceiptStatus::Completed { output },
                    Err(error) => ReceiptStatus::Failed { error: *error },
                },
            })
        }),
    }
}

/// The page `query` asks of `rows`, the actions `provider` offers for the application `app_id`.
#[must_use]
pub fn offered(
    provider: &str,
    app_id: &str,
    rows: Vec<PlacedAction>,
    query: &CatalogueQuery,
) -> Pending<Result<CataloguePage, ActionCallError>> {
    let rows: Vec<CatalogueRow> = rows
        .into_iter()
        .map(|placed| CatalogueRow::for_placed_action(provider, app_id, placed))
        .collect();
    let revision = rows.len().to_string();
    Pending::ready(Ok(CataloguePage::from_rows(revision, rows, query)))
}

/// Every action of `actions` as a top-level row.
#[must_use]
pub fn top_level(actions: Vec<ActionDescriptor>) -> Vec<PlacedAction> {
    actions
        .into_iter()
        .map(|action| PlacedAction {
            parent: String::new(),
            has_children: false,
            action,
        })
        .collect()
}
