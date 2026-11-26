//! A call to an app that keeps a long-lived connection to an outside service (a chat bridge's
//! gateway), run where that connection runs.
//!
//! One device of the workspace has the connection's lease and runs the app's process
//! ([`tilo_workspace_service::work::Turn::Connection`]); every other device sends a call to the app's actions
//! there, and never starts the app itself, so the connection never runs twice. The call travels
//! as its caller signed it: the device that has the lease decides it as the agent, visitor or
//! person who made it and records this device as the one that brought it. The person at this
//! device needs no signature of their own: this device's call is theirs. A call of anyone else
//! that carries no signature of theirs stays here, refused, since the other device would run it
//! as nobody. The caller is answered once the call settled there. A call that reaches a device
//! which lost the lease meanwhile is refused there, never passed along again.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, Completer, InvocationReceipt, InvokeRequest, Pending,
    application_provider, pending,
};

use super::super::AppBrokerState;
use super::super::published_invocation::InvocationCaller;
use super::own_device::is_person_here;
use crate::services::ConnectionActions;

impl AppBrokerState {
    /// Place `request`, a call to an app, and run it there: here when this device runs the app's
    /// connection or the app keeps none, on the device that has the connection's lease
    /// otherwise ([`tilo_workspace_service::work::connection_call`]), and nowhere, with why, while no device has
    /// it. This is the one place a call to an app is placed.
    pub(super) fn invoke_application(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if self.services.hosted_workspaces.is_none() {
            return self.invoke_application_here(caller, origin, request);
        }
        let runtime = match super::super::launchable::delivery_runtime() {
            Ok(runtime) => runtime,
            Err(error) => {
                return Pending::ready(Err(ActionCallError::Unavailable {
                    reason: format!("the call could not be placed: {error}"),
                }));
            }
        };
        let (done, answer) = pending();
        let (state, caller, origin, request) = (
            Arc::clone(self),
            caller.clone(),
            origin.cloned(),
            request.clone(),
        );
        runtime.spawn(async move {
            match state.placed(&caller, origin.as_ref(), &request).await {
                Ok(None) => {
                    state.record_brought(&caller, &request);
                    let here = state.invoke_application_here(&caller, origin.as_ref(), &request);
                    done.complete(here.await.unwrap_or_else(|| {
                        Err(ActionCallError::Unavailable {
                            reason: "the app's answer was dropped before it arrived".into(),
                        })
                    }));
                }
                Ok(Some(there)) => run_there(there, caller, request, done).await,
                Err(error) => done.complete(Err(error)),
            }
        });
        answer
    }

    /// `request`, a call the app `caller` names makes while it runs the invocation its
    /// `on_behalf_of` names, carrying that invocation as its proof when this device took it and
    /// the invocation called that app: the device's word is which app made the call, and its
    /// signature on the request that leaves it says so. Any other call leaves as it is.
    pub(super) fn with_served_proof(
        &self,
        caller: &Caller,
        mut request: InvokeRequest,
    ) -> InvokeRequest {
        let (Some(served), None, Some(app)) =
            (request.on_behalf_of, &request.signed, caller.acting().app())
        else {
            return request;
        };
        let proof = self.remote_invocations.lock().ok().and_then(|log| {
            log.request_of(&served)
                .and_then(|original| original.signed.clone())
                .filter(|signed| {
                    signed.statement().invocation == served
                        && signed
                            .called_app()
                            .is_some_and(|(_, called)| called == app.as_str())
                })
        });
        request.signed = proof;
        request
    }

    /// Keep a call another device brought here in this device's record of the invocations it
    /// was asked for: the record its status is read from, which its completion and its
    /// cancellation end, and what proves a call its app makes for it still serves a live one.
    fn record_brought(self: &Arc<Self>, caller: &Caller, request: &InvokeRequest) {
        let (Some(records), Some(_)) = (self.invocations.get(), caller.forwarder()) else {
            return;
        };
        let route: Arc<dyn ActionProvider + Send + Sync> =
            Arc::new(super::AppCalls::new(Arc::downgrade(self)));
        records.record(
            caller,
            caller,
            request,
            caller.workspace(),
            (route, caller.clone()),
        );
    }

    /// The device that runs the connection of `request`'s app, when another device than this
    /// one does; `None` to run it here.
    ///
    /// # Errors
    ///
    /// No device runs the connection; this device lost it after another device sent the call
    /// here; or the call carries no signature of a caller other than the person at this device.
    async fn placed(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        request: &InvokeRequest,
    ) -> Result<Option<Arc<dyn ActionProvider + Send + Sync>>, ActionCallError> {
        let (Some(hosted), Some((workspace, app))) = (
            self.services.hosted_workspaces.as_ref(),
            application_provider(&request.target.provider),
        ) else {
            return Ok(None);
        };
        match hosted.connection_actions(workspace, app).await {
            ConnectionActions::Here => Ok(None),
            ConnectionActions::Unplaced(why) => Err(ActionCallError::Unavailable { reason: why }),
            // A call another device brought here because this one had the lease goes no further
            // once it lost it: the device that sent it asks again where the lease is now.
            ConnectionActions::On(_) if caller.forwarder().is_some() => {
                Err(ActionCallError::Refused {
                    reason: "this device no longer runs this app's connection".into(),
                })
            }
            ConnectionActions::On(_)
                if request.signed.is_none() && !is_person_here(caller, origin) =>
            {
                Err(ActionCallError::Refused {
                    reason: "this app's connection runs on another device, and the call carries \
                             no signature of its caller's to run there"
                        .into(),
                })
            }
            ConnectionActions::On(actions) => Ok(Some(actions)),
        }
    }
}

/// Send `request` to `actions`, the device that runs its app's connection, and complete `done`
/// with that device's receipt once the call settled there.
async fn run_there(
    actions: Arc<dyn ActionProvider + Send + Sync>,
    caller: Caller,
    request: InvokeRequest,
    done: Completer<Result<InvocationReceipt, ActionCallError>>,
) {
    match actions.invoke(&caller, request).await {
        Some(Ok(receipt)) => super::hosted::follow(actions, caller, receipt, done),
        Some(Err(error)) => done.complete(Err(error)),
        None => done.complete(Err(ActionCallError::Unavailable {
            reason: "the device that runs this app's connection did not answer".into(),
        })),
    }
}
