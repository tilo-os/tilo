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
    ActionCallError, ActionProvider, CataloguePage, CatalogueQuery, CatalogueRow,
    InvocationReceipt, Pending, ReceiptStatus,
};
use crate::{ActionFailure, ActionOutput, InvocationId};

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
    rows: Vec<crate::api::PlacedAction>,
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
pub fn top_level(actions: Vec<crate::api::ActionDescriptor>) -> Vec<crate::api::PlacedAction> {
    actions
        .into_iter()
        .map(|action| crate::api::PlacedAction {
            parent: String::new(),
            has_children: false,
            action,
        })
        .collect()
}
