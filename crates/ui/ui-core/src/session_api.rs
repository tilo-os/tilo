//! Opening another app, the one way every host does it. An app offers to open another
//! installed app as an action of its own ([`ApplicationLaunch`]). Run, that action
//! becomes the workspace's `open` of the app ([`ApplicationLaunch::target`]), asked of
//! the session's action API: the window never starts anything itself.

#[cfg(feature = "model")]
use collab_types::app_actions::ApplicationLaunch;
use tilo_actions::remote::{ActionCallError, InvokeRequest};
use tilo_actions::{ActionOutput, ApiError, InvocationId, InvocationUpdate};

#[cfg(feature = "model")]
use crate::{remote_actions::InvocationCall, remote_actions::RemoteActions};
#[cfg(feature = "model")]
use tilo_ui_base::arriving::Arrival;
use tilo_ui_base::widget::ResourceOperationStart;

/// Where a host sends what an action asks of the session beyond its app.
pub trait SessionApi {
    /// The workspace this window was admitted to, which a request addresses when the
    /// action's own route names none.
    fn workspace(&self) -> Option<String>;

    /// Hand `request` to the session for the operation `operation`, which the session's
    /// broker admitted when `admitted`. Why the session did not take it.
    fn invoke(
        &mut self,
        operation: InvocationId,
        admitted: bool,
        request: InvokeRequest,
    ) -> Result<ResourceOperationStart, ApiError>;
}

/// The launch `payload` encodes, checked. A payload that is not a launch, or names an app
/// or content out of bounds.
#[cfg(feature = "model")]
pub(crate) fn decode_application_launch(payload: &[u8]) -> Result<ApplicationLaunch, ApiError> {
    let launch: ApplicationLaunch = serde_json::from_slice(payload)
        .map_err(|_| ApiError::Failed("invalid application launch payload".into()))?;
    if !launch.valid() {
        return Err(ApiError::Failed(
            "application launch payload is out of bounds".into(),
        ));
    }
    Ok(launch)
}

/// Open the app `payload` names, through `api`, in the workspace of the action's `route`
/// (else the API's own). The request continues the invocation the broker delivered for
/// it (`operation`, when `admitted`; else the one being handled here), so the session
/// decides it as that invocation's caller; without one, only the person's own press in
/// this app is behind it. An invalid launch, no API or workspace to open it in, or the
/// API's refusal.
#[cfg(feature = "model")]
pub(crate) fn start_application_launch(
    api: Option<&mut dyn SessionApi>,
    route: Option<&tilo_ui_base::collaboration::RouteSpec>,
    payload: &[u8],
    operation: InvocationId,
    admitted: bool,
) -> Result<ResourceOperationStart, ApiError> {
    let launch = decode_application_launch(payload)?;
    let Some(api) = api else {
        return Err(ApiError::Unavailable {
            code: "authoritative_host_unavailable".into(),
            reason: "Opening an application requires the workspace action broker".into(),
        });
    };
    let Some(workspace) = route
        .filter(|route| !route.cluster_id.is_nil())
        .map(|route| route.cluster_id.to_string())
        .or_else(|| api.workspace())
    else {
        return Err(ApiError::Unavailable {
            code: "authoritative_host_unavailable".into(),
            reason: "Opening an application requires an admitted workspace route".into(),
        });
    };
    let on_behalf_of = if admitted {
        Some(operation)
    } else {
        crate::invocation_scope::delivered_invocation()
    };
    api.invoke(
        operation,
        admitted,
        InvokeRequest {
            asks: false,
            on_behalf_of,
            invocation: InvocationId::new(),
            target: launch.target(&workspace),
            argument: launch.argument(),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        },
    )
}

/// What a refusal from the session says to the person who asked.
#[must_use]
pub fn api_error(error: &ActionCallError) -> ApiError {
    match error {
        ActionCallError::Unavailable { reason }
        | ActionCallError::Refused { reason }
        | ActionCallError::InvalidRequest { reason } => ApiError::Unavailable {
            code: error.code().into(),
            reason: reason.clone(),
        },
        ActionCallError::ConfirmationRequired { .. } => ApiError::ConfirmationRequired,
        ActionCallError::NotPermitted => ApiError::Unavailable {
            code: error.code().into(),
            reason: "You may not do that here".into(),
        },
        other => ApiError::Unavailable {
            code: other.code().into(),
            reason: "It isn't available to you here".into(),
        },
    }
}

#[cfg(feature = "model")]
fn finished(
    call: &Arrival<InvocationUpdate, ActionCallError>,
) -> Option<Result<ActionOutput, ApiError>> {
    match call {
        Arrival::Failed(error) => Some(Err(api_error(error))),
        Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => Some(Ok(output.clone())),
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
            Some(Err(ApiError::Failed(error.message.clone())))
        }
        Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => Some(Err(ApiError::Failed(
            message.clone().unwrap_or_else(|| "It was cancelled".into()),
        ))),
        Arrival::Pending { .. } | Arrival::Ready(_) => None,
    }
}

/// A [`SessionApi`] served by a [`RemoteActions`] client: the session of a host that
/// serves its windows in process, such as a page.
#[cfg(feature = "model")]
pub struct ClientApi {
    client: RemoteActions,
    workspace: Option<String>,
    running: Vec<(InvocationId, InvocationCall)>,
}

#[cfg(feature = "model")]
impl ClientApi {
    /// Requests go to `client`, addressed to `workspace` unless an action's route names
    /// its own.
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

    /// The operations whose calls finished since this was last asked, and how each ended.
    pub fn take_finished(&mut self) -> Vec<(InvocationId, Result<ActionOutput, ApiError>)> {
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

#[cfg(feature = "model")]
impl SessionApi for ClientApi {
    fn workspace(&self) -> Option<String> {
        self.workspace.clone()
    }

    fn invoke(
        &mut self,
        operation: InvocationId,
        _admitted: bool,
        request: InvokeRequest,
    ) -> Result<ResourceOperationStart, ApiError> {
        let call = self.client.invoke_continuing(
            request.on_behalf_of,
            request.invocation,
            request.target,
            request.argument,
            request.confirmation,
        );
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
