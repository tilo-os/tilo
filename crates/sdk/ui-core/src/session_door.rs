//! Opening another app, the one way every host does it.
//!
//! An app offers to open another installed app as an action of its own
//! ([`ApplicationLaunch`]). Run, that action becomes the workspace's `open`
//! of the app ([`ApplicationLaunch::target`]), asked of the session's action
//! door: the window never starts anything itself. `start_application_launch`
//! is that step for every host; a host supplies only its door
//! ([`SessionDoor`]). A native window's door is its session broker; a page's
//! is the page's own session, served to it as a [`RemoteActions`] client
//! ([`ClientDoor`]).

use tilo_actions::remote::{ActionHostError, ApplicationLaunch, InvokeRequest};
use tilo_actions::{ActionOutput, DoorError, InvocationId, InvocationUpdate};

use crate::arriving::Arrival;
use crate::remote_actions::{InvocationCall, RemoteActions};
use crate::widget::ResourceOperationStart;

/// Where a host sends what an action asks of the session beyond its app.
pub trait SessionDoor {
    /// The workspace this window was admitted to, which a request addresses
    /// when the action's own route names none.
    fn workspace(&self) -> Option<String>;

    /// Hand `request` to the session for the operation `operation`, which
    /// the session's broker admitted when `admitted`. The answer is the
    /// operation's start: done, or accepted and finished later by the host.
    ///
    /// # Errors
    /// Why the session did not take it.
    fn invoke(
        &mut self,
        operation: InvocationId,
        admitted: bool,
        request: InvokeRequest,
    ) -> Result<ResourceOperationStart, DoorError>;
}

/// The launch `payload` encodes, checked.
///
/// # Errors
/// A payload that is not a launch, or names an app or content out of bounds.
pub(crate) fn decode_application_launch(payload: &[u8]) -> Result<ApplicationLaunch, DoorError> {
    let launch: ApplicationLaunch = serde_json::from_slice(payload)
        .map_err(|_| DoorError::Failed("invalid application launch payload".into()))?;
    if !launch.valid() {
        return Err(DoorError::Failed(
            "application launch payload is out of bounds".into(),
        ));
    }
    Ok(launch)
}

/// Open the app `payload` names, through `door`, in the workspace of the
/// action's `route` (else the door's own).
///
/// The request continues the invocation the broker delivered for it
/// (`operation`, when `admitted`; else the one being handled here), so the
/// session decides it as that invocation's caller; without one, only the
/// person's own press in this app is behind it.
///
/// # Errors
/// An invalid launch, no door or workspace to open it in, or the door's
/// refusal.
pub(crate) fn start_application_launch(
    door: Option<&mut dyn SessionDoor>,
    route: Option<&crate::collaboration::RouteSpec>,
    payload: &[u8],
    operation: InvocationId,
    admitted: bool,
) -> Result<ResourceOperationStart, DoorError> {
    let launch = decode_application_launch(payload)?;
    let Some(door) = door else {
        return Err(DoorError::Unavailable {
            code: "authoritative_host_unavailable".into(),
            reason: "Opening an application requires the workspace action broker".into(),
        });
    };
    let Some(room) = route
        .filter(|route| !route.cluster_id.is_nil())
        .map(|route| route.cluster_id.to_string())
        .or_else(|| door.workspace())
    else {
        return Err(DoorError::Unavailable {
            code: "authoritative_host_unavailable".into(),
            reason: "Opening an application requires an admitted workspace route".into(),
        });
    };
    let on_behalf_of = if admitted {
        Some(operation)
    } else {
        crate::invocation_scope::delivered_invocation()
    };
    door.invoke(
        operation,
        admitted,
        InvokeRequest {
            on_behalf_of,
            invocation: InvocationId::new(),
            target: launch.target(&room),
            argument: launch.argument(),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        },
    )
}

/// What a refusal from the session says to the person who asked.
#[must_use]
pub fn door_error(error: &ActionHostError) -> DoorError {
    match error {
        ActionHostError::Unavailable { reason } | ActionHostError::Refused { reason } => {
            DoorError::Unavailable {
                code: error.code().into(),
                reason: reason.clone(),
            }
        }
        ActionHostError::ConfirmationRequired => DoorError::ConfirmationRequired,
        ActionHostError::NotPermitted => DoorError::Unavailable {
            code: error.code().into(),
            reason: "You may not do that here".into(),
        },
        other => DoorError::Unavailable {
            code: other.code().into(),
            reason: "It isn't available to you here".into(),
        },
    }
}

/// How a finished call ended for the operation that made it.
fn finished(
    call: &Arrival<InvocationUpdate, ActionHostError>,
) -> Option<Result<ActionOutput, DoorError>> {
    match call {
        Arrival::Failed(error) => Some(Err(door_error(error))),
        Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => Some(Ok(output.clone())),
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
            Some(Err(DoorError::Failed(error.message.clone())))
        }
        Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => Some(Err(
            DoorError::Failed(message.clone().unwrap_or_else(|| "It was cancelled".into())),
        )),
        // Still running: its end arrives later.
        Arrival::Pending { .. } | Arrival::Ready(_) => None,
    }
}

/// A [`SessionDoor`] served by a [`RemoteActions`] client: the session of a
/// host that serves its windows in process, such as a page.
pub struct ClientDoor {
    client: RemoteActions,
    workspace: Option<String>,
    running: Vec<(InvocationId, InvocationCall)>,
}

impl ClientDoor {
    /// Requests go to `client`, addressed to `workspace` unless an action's
    /// route names its own.
    #[must_use]
    pub fn new(client: RemoteActions, workspace: Option<String>) -> Self {
        Self {
            client,
            workspace,
            running: Vec::new(),
        }
    }

    /// Whether an operation handed over is still waiting for its answer.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// The operations whose calls finished since this was last asked, and
    /// how each ended.
    pub fn take_finished(&mut self) -> Vec<(InvocationId, Result<ActionOutput, DoorError>)> {
        let mut done = Vec::new();
        self.running
            .retain(|(operation, call)| match call.with(finished) {
                Some(result) => {
                    done.push((*operation, result));
                    false
                }
                None => true,
            });
        done
    }
}

impl SessionDoor for ClientDoor {
    fn workspace(&self) -> Option<String> {
        self.workspace.clone()
    }

    fn invoke(
        &mut self,
        operation: InvocationId,
        _admitted: bool,
        request: InvokeRequest,
    ) -> Result<ResourceOperationStart, DoorError> {
        let call = self.client.invoke_continuing(
            request.on_behalf_of,
            request.invocation,
            request.target,
            request.argument,
            request.confirmation,
        );
        // A session served in process often answers at once.
        match call.with(finished) {
            Some(Ok(output)) => Ok(ResourceOperationStart::Completed(output)),
            Some(Err(error)) => Err(error),
            None => {
                self.running.push((operation, call));
                Ok(ResourceOperationStart::Accepted)
            }
        }
    }
}
