//! A call to an app that keeps a long-lived connection to an outside service (a chat bridge's
//! gateway), run where that connection runs.
//!
//! One device of the workspace has the connection's lease and runs the app's process
//! ([`crate::work::Turn::Connection`]); every other device sends a call to the app's actions
//! there, and never starts the app itself, so the connection never runs twice. The call travels
//! as its caller signed it: the device that has the lease decides it as the agent, visitor or
//! person who made it and records this device as the one that brought it. The person at this
//! device needs no signature of their own: this device's call is theirs. A call of anyone else
//! that carries no signature of theirs stays here, refused, since the other device would run it
//! as nobody. The caller is answered once the call settled there. A call that reaches a device
//! which lost the lease meanwhile is refused there, never passed along again.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, Completer, InvocationReceipt, InvokeRequest,
};

use super::super::AppBrokerState;
use super::super::published_invocation::InvocationCaller;
use super::LaunchDestination;
use super::own_device::is_person_here;
use crate::apps::services::ConnectionActions;

impl AppBrokerState {
    /// The device that runs `destination`'s app's connection, when another device than this one
    /// does: `request` goes there instead of starting the app here. `None` to run it here.
    ///
    /// # Errors
    ///
    /// No device runs the connection, or the call carries no signature of a caller other than
    /// the person at this device.
    pub(super) async fn connection_forward(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        destination: &LaunchDestination,
        request: &InvokeRequest,
    ) -> Result<Option<Arc<dyn ActionProvider + Send + Sync>>, ActionCallError> {
        let Some(hosted) = self.services.hosted_workspaces.as_ref() else {
            return Ok(None);
        };
        let workspace = destination.workspace.id().to_string();
        match hosted
            .connection_actions(&workspace, &destination.app_id)
            .await
        {
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
pub(super) async fn run_there(
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
