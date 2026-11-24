//! The actions this session answers by asking the device's daemon over its shell link: an app's
//! action on another of the person's devices, and where an offloadable invocation runs.
//!
//! The daemon holds what these need — its endpoint and the link between the person's devices —
//! and answers them on its one dispatch. The session forwards each request to that dispatch as the
//! person on the link it was started with (the same way it reaches the daemon's actions itself),
//! and follows it until it settles, so the app observes the answer as its own invocation's.

use std::time::Duration;

use tilo_actions::offload::{OffloadFuture, OffloadRequest, Offloaded};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest,
    Outcome, Pending, ReceiptStatus, offload_place, own_device,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_broker_core::services::{OwnDeviceActions, OwnDeviceOutcome};

/// Where a forward runs: the runtime the caller is on, else the session's own.
fn runtime() -> tokio::runtime::Handle {
    tokio::runtime::Handle::try_current().unwrap_or_else(|_| tilo_rt::app().handle().clone())
}

/// The daemon's one dispatch, as this session reaches it on its shell link.
#[derive(Clone)]
pub(crate) struct Ask(runtime_collab::ShellClient);

impl Ask {
    /// The daemon's dispatch on `link`.
    pub(crate) fn over(link: &runtime_collab::ShellClient) -> Self {
        Self(link.clone())
    }

    /// `request` asked of the daemon's dispatch as `caller`, followed until it settles: its
    /// terminal update, or why it was refused, failed or lost.
    pub(crate) async fn answer(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Option<Result<InvocationUpdate, ActionCallError>> {
        // Made here, on the runtime the call runs on, as the dispatch carries its own waits.
        let dispatch = self.0.dispatch();
        let invocation = request.invocation;
        let receipt = dispatch.invoke(caller, request).await?;
        let receipt = match receipt {
            Ok(receipt) => receipt,
            Err(error) => return Some(Err(error)),
        };
        match receipt.status {
            ReceiptStatus::Accepted => {}
            ReceiptStatus::Rejected { error } => return Some(Err(error)),
            ReceiptStatus::Completed { output } => {
                return Some(Ok(InvocationUpdate::Succeeded { invocation, output }));
            }
            ReceiptStatus::Failed { error } => {
                return Some(Ok(InvocationUpdate::Failed { invocation, error }));
            }
            ReceiptStatus::Cancelled { message } => {
                return Some(Ok(InvocationUpdate::Cancelled {
                    invocation,
                    message,
                }));
            }
        }
        loop {
            let change = dispatch.next_change(caller, &invocation)?;
            change.await;
            let state = dispatch.observe(caller, &invocation)?;
            if state.update.is_terminal() {
                return Some(Ok(state.update));
            }
        }
    }

    /// [`Self::answer`] as an app observes it: the output the action gave, or the failure it
    /// ended with.
    pub(crate) async fn outcome(&self, caller: &Caller, request: InvokeRequest) -> Outcome {
        outcome_of(self.answer(caller, request).await)
    }

    /// [`Self::answer`]'s output value, or why it answered none: what a caller that reads the
    /// answer's fields asks.
    pub(crate) async fn value(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<serde_json::Value, String> {
        match self.answer(caller, request).await {
            Some(Ok(InvocationUpdate::Succeeded { output, .. })) => {
                Ok(output.value.unwrap_or_default())
            }
            Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(error.message),
            Some(Ok(InvocationUpdate::Cancelled { message, .. })) => {
                Err(message.unwrap_or_else(|| "the action was cancelled".to_owned()))
            }
            Some(Ok(_)) => Err("the action did not finish".to_owned()),
            Some(Err(error)) => Err(error.to_string()),
            None => Err("the device's daemon did not answer".to_owned()),
        }
    }
}

/// What an answer of the daemon's that settled comes to, as the app observes it.
fn outcome_of(answer: Option<Result<InvocationUpdate, ActionCallError>>) -> Outcome {
    let failure = |code: &str, message: String| {
        Box::new(ActionFailure {
            code: code.to_owned(),
            message,
            details: None,
            retryable: true,
        })
    };
    match answer {
        Some(Ok(InvocationUpdate::Succeeded { output, .. })) => Ok(output),
        Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(Box::new(error)),
        Some(Ok(InvocationUpdate::Cancelled { message, .. })) => Err(failure(
            "cancelled",
            message.unwrap_or_else(|| "the action was cancelled".into()),
        )),
        Some(Ok(update)) => Err(failure(
            "unsettled",
            format!("the action did not settle: {update:?}"),
        )),
        Some(Err(error)) => Err(failure("own_device_refused", error.to_string())),
        None => Err(failure(
            "own_device_abandoned",
            "the device's answer was dropped before it arrived".into(),
        )),
    }
}

/// An app's action on another of the person's own devices, carried to the daemon's dispatch,
/// which forwards it and follows it there.
pub(crate) struct AnotherDevice {
    ask: Ask,
}

impl AnotherDevice {
    /// The person's own-device actions, forwarded to the daemon over `link`.
    pub(crate) fn over(link: &runtime_collab::ShellClient) -> Self {
        Self {
            ask: Ask::over(link),
        }
    }
}

impl OwnDeviceActions for AnotherDevice {
    fn invoke(
        &self,
        caller: &Caller,
        _checked: &own_device::Checked,
        request: &InvokeRequest,
    ) -> Result<Pending<OwnDeviceOutcome>, ActionCallError> {
        // The daemon decides the device and carries the request; the one it is sent is the
        // person's own-device invocation, in the workspace the app acts in.
        let Some(workspace) = caller.workspace() else {
            return Err(ActionCallError::NotPermitted);
        };
        let caller = caller.clone().in_workspace(workspace.to_owned());
        let request = request.clone();
        let ask = self.ask.clone();
        let (complete, outcome) = tilo_actions::remote::pending();
        runtime().spawn(async move {
            complete.complete(ask.outcome(&caller, request).await);
        });
        Ok(outcome)
    }

    fn ask_person(
        &self,
        _caller: &Caller,
        _question: &tilo_actions::remote::ConfirmationQuestion,
    ) -> Result<Pending<Result<tilo_actions::remote::Confirmation, ActionCallError>>, ActionCallError>
    {
        // A person's own word is asked of them where they are, by the device in front of them.
        Err(ActionCallError::Unavailable {
            reason: "this device does not ask the person on another of their devices".into(),
        })
    }
}

/// An app's offloadable invocation, placed by the device's daemon: the daemon holds the endpoint
/// and the link to the workspace's other devices, while this session does not. The answer is
/// another device's output, or that the invocation runs here, where the broker hands it to the
/// app's own handler.
pub(crate) struct Offload {
    ask: Ask,
}

impl Offload {
    /// The daemon's placement, asked on `link`.
    pub(crate) fn over(link: &runtime_collab::ShellClient) -> Self {
        Self {
            ask: Ask::over(link),
        }
    }

    /// Where `request`, the invocation `invocation`, runs.
    fn place(&self, request: OffloadRequest, invocation: InvocationId) -> OffloadFuture<'_> {
        let ask = self.ask.clone();
        Box::pin(async move {
            // The daemon reads the workspace and admits there as its own person; the name here
            // only says which workspace the call is in.
            let caller = Caller::local_person("session").in_workspace(request.workspace.clone());
            let placed = offload_place::Ask {
                workspace: request.workspace.clone(),
                app_id: request.app_id.clone(),
                resource: request.address.resource.clone(),
                action: request.address.action.clone(),
                offload: request.offload.clone(),
                inputs: request.inputs.clone(),
                invocation,
            };
            let argument = match serde_json::to_value(placed) {
                Ok(serde_json::Value::Object(argument)) => InvokeArgument::Json(argument),
                _ => {
                    return Offloaded::RunHere {
                        reason: "the placement does not encode".to_owned(),
                    };
                }
            };
            let invoke = InvokeRequest {
                on_behalf_of: None,
                invocation: InvocationId::new(),
                target: offload_place::target(),
                argument,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            match ask.value(&caller, invoke).await {
                Ok(value) => match serde_json::from_value::<offload_place::Placed>(value) {
                    Ok(offload_place::Placed::Ran {
                        output,
                        device,
                        elapsed_ms,
                    }) => Offloaded::Ran {
                        output,
                        device,
                        elapsed: Duration::from_millis(elapsed_ms),
                    },
                    Ok(offload_place::Placed::Here { reason }) => Offloaded::RunHere { reason },
                    Err(error) => Offloaded::RunHere {
                        reason: format!("the daemon's placement does not read: {error}"),
                    },
                },
                Err(reason) => Offloaded::RunHere { reason },
            }
        })
    }
}

impl tilo_actions::offload::ActionOffload for Offload {
    fn offload(&self, request: OffloadRequest) -> OffloadFuture<'_> {
        self.place(request, InvocationId::new())
    }

    fn offload_invocation(
        &self,
        request: OffloadRequest,
        invocation: InvokeRequest,
    ) -> OffloadFuture<'_> {
        self.place(request, invocation.invocation)
    }
}
