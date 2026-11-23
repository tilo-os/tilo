//! A person's answer to an asked invocation record (`<record>#resolve`), and the call it
//! answers coming back to this dispatch.
//!
//! The record's store admits the answer ([`AskedStore::resolve`]): only from the person it
//! asks, only while it is asked, and once. An approve comes back here
//! ([`AskedDispatch::readmit`]), the only place a call carrying the person's approve is
//! admitted: every caller's own request is admitted without one
//! ([`InvokeRequest::strip_approval`]). It runs again under its id for the caller who asked,
//! under that caller's grants as they stand now.

use std::sync::{Arc, Weak};

use tilo_actions::remote::asked::{self, AskedDispatch, AskedStore, Resolve};
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionMessage,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_server_protocol::WorkspaceId;

use super::{CatalogueFactory, Shared, WorkspaceActions};

/// This dispatch, as an asked record's store hands it back the call it keeps: held weakly, so
/// the store keeps nothing of the service alive.
struct Dispatch {
    shared: Weak<Shared>,
    catalogue: CatalogueFactory,
}

impl Dispatch {
    fn actions(&self) -> Result<WorkspaceActions, ActionCallError> {
        self.shared
            .upgrade()
            .map(|shared| WorkspaceActions {
                shared,
                catalogue: self.catalogue.clone(),
            })
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "the host's workspace actions are not running".into(),
            })
    }
}

impl AskedDispatch for Dispatch {
    fn readmit(&self, caller: &Caller, request: InvokeRequest) -> Result<(), ActionCallError> {
        self.actions()?.readmit(caller, request)
    }

    fn declined(&self, caller: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        self.actions()?.declined(caller, invocation)
    }
}

/// What a call that no longer waits in its record is refused another answer with.
fn not_waiting() -> ActionCallError {
    ActionCallError::Refused {
        reason: "The call no longer waits for this answer".into(),
    }
}

impl WorkspaceActions {
    /// Keep every invocation that waits for a person's yes as its asked record in `store`,
    /// which hands a person's answer to one back to this dispatch.
    pub fn keep_asked_in(&self, store: &Arc<AskedStore>) {
        let dispatch: Arc<dyn AskedDispatch> = Arc::new(Dispatch {
            shared: Arc::downgrade(&self.shared),
            catalogue: self.catalogue.clone(),
        });
        let records = store.for_dispatch(Arc::downgrade(&dispatch));
        if self.shared.asked_records.set(records).is_ok() {
            let _ = self.shared.asked_dispatch.set(dispatch);
            let _ = self.shared.asked_store.set(Arc::clone(store));
        }
    }

    /// Answer `request`, a person's `<record>#resolve` in `workspace_id`, as `caller`: its
    /// receipt once the call it answers is admitted again or ended.
    pub(super) fn resolve_request(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<RemoteActionMessage> {
        let invocation = request.invocation;
        let receipt = move |result: Result<(), ActionCallError>| {
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: match result {
                    Ok(()) => ReceiptStatus::Completed {
                        output: ActionOutput::default(),
                    },
                    Err(error) => ReceiptStatus::Rejected { error },
                },
            })
        };
        let arguments = match &request.argument {
            tilo_actions::remote::InvokeArgument::Json(arguments) => arguments.clone(),
            tilo_actions::remote::InvokeArgument::None
            | tilo_actions::remote::InvokeArgument::Text(_) => serde_json::Map::new(),
        };
        let answer = serde_json::from_value::<Resolve>(serde_json::Value::Object(arguments))
            .map_err(|error| ActionCallError::InvalidRequest {
                reason: error.to_string(),
            });
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => return Pending::ready(receipt(Err(error))),
        };
        let Some(store) = self.shared.asked_store.get() else {
            return Pending::ready(receipt(Err(ActionCallError::UnknownAction)));
        };
        // The record is answered only in the workspace it was asked in.
        let at = store
            .record(&request.target.address.resource)
            .filter(|at| WorkspaceId::parse(&at.workspace) == Some(workspace_id));
        let Some(at) = at else {
            return Pending::ready(receipt(Err(ActionCallError::Refused {
                reason: asked::NOTHING_ASKED.into(),
            })));
        };
        store.resolve(&at, caller, &answer).map(receipt)
    }

    /// Admit `request` again under its id, carrying the person's approve: only where the
    /// invocation `caller` asked for still waits in its asked record for the same call.
    fn readmit(&self, caller: &Caller, request: InvokeRequest) -> Result<(), ActionCallError> {
        let invocation = request.invocation;
        let mut log = self.log();
        let entry = log
            .entries
            .get_mut(&invocation)
            .ok_or(ActionCallError::UnknownInvocation)?;
        let theirs = caller.continues(&entry.caller) || entry.caller.continues(caller);
        if !entry.asked
            || entry.update.is_terminal()
            || !theirs
            || request.approval().is_none()
            || !asked::same_call(&entry.request, &request)
        {
            return Err(not_waiting());
        }
        let workspace_id = entry.workspace.ok_or_else(not_waiting)?;
        entry.asked = false;
        entry.request = request;
        entry.update = InvocationUpdate::Accepted { invocation };
        log.moved();
        drop(log);
        // It runs for its caller, who follows it under its id: the receipt is theirs to read.
        self.run_admitted(workspace_id, invocation).then(|_| {});
        Ok(())
    }

    /// End `invocation`, which waits in its asked record, as declined by the person.
    fn declined(&self, caller: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        let mut log = self.log();
        let entry = log
            .entries
            .get_mut(&invocation)
            .ok_or(ActionCallError::UnknownInvocation)?;
        let theirs = caller.continues(&entry.caller) || entry.caller.continues(caller);
        if !entry.asked || entry.update.is_terminal() || !theirs {
            return Err(not_waiting());
        }
        entry.asked = false;
        entry.update = InvocationUpdate::Cancelled {
            invocation,
            message: Some(asked::DECLINED.to_owned()),
        };
        log.stamp_ended();
        log.moved();
        Ok(())
    }
}
