//! The broker's answers to the remote-action vocabulary.
//!
//! Every caller that is not the application itself — a peer on the shell
//! network, an app or agent on this machine, the MCP bridge, the palette —
//! reaches published actions through these methods, wrapped as an
//! [`ActionHost`](tilo_actions::remote::ActionHost) by
//! the host's `BrokerActionHost`.
//! Authority is decided from the transport-supplied [`Caller`] alone, with
//! the same capability checks the admission path has always applied.

use std::sync::Arc;

use tilo_desktop_model::Workspace;

use tilo_actions::remote::{
    APPLICATION_CLOSE_ACTION, APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_KIND,
    APPLICATION_RESOURCE_PREFIX, ActionHostError, Caller, CallerPrincipal, CataloguePage,
    CatalogueProvider, CatalogueQuery, CatalogueRow, Completer, Confirmation, InvocationReceipt,
    InvocationState, InvokeRequest, OWN_PROVIDER, Pending, ReceiptStatus, Reservation,
    application_provider, application_provider_record, pending, provider_workspace,
    workspace_provider,
};
use tilo_actions::{ActionAddress, ActionDescriptor, ExecutionLocus, InputSpec};

use super::launchable::{is_launchable_provider, launchable_provider};
use super::{
    AppBrokerState, BrokerActor, DIRECTORY_MAX_PARAMS_BYTES, InvocationInputs, LaunchScope,
    PublishedAction, PublishedSurface, app_node, confirmation_satisfied,
};
use crate::automation;

use crate::services::LaunchPlace;
use tilo_automation::SHORTCUTS_APP_ID;

#[cfg(test)]
#[path = "agent_app_list_tests.rs"]
mod agent_app_list_tests;
#[cfg(test)]
#[path = "governance_witness_tests.rs"]
mod governance_witness_tests;

mod app_admission;
mod directory;
mod hosted;
mod launch_child;
mod own_device;
mod publishers;
mod readiness;
mod readmit;
mod shortcut_run;
mod workspace_nodes;
use app_admission::{invitation_request, own_view_lookup, reference_request};
use directory::bridge_error;

/// The longest a waiter on a running invocation sleeps without a signalled
/// change, so a change nothing signals (an idle expiry) costs latency, never
/// a stalled shortcut run.
pub(super) const CHANGE_SAFETY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Why an action's arguments are too large to pass, in words a caller can
/// act on: how large they are, the most one action takes, and to split.
fn oversized_argument(argument: &str) -> Option<String> {
    (argument.len() > DIRECTORY_MAX_PARAMS_BYTES).then(|| {
        format!(
            "the arguments are {} bytes, over the {} one action takes: split them over several calls of the action",
            argument.len(),
            DIRECTORY_MAX_PARAMS_BYTES
        )
    })
}

/// A retired transport run cannot use a retained caller clone. Legacy callers
/// without an execution retain their existing behavior.
pub(super) fn check_execution(caller: &Caller) -> Result<(), ActionHostError> {
    if caller.execution_id().is_some() && !caller.execution_is_live() {
        return Err(ActionHostError::Unavailable {
            reason: "execution session ended".into(),
        });
    }
    Ok(())
}

pub(super) struct LaunchDestination {
    pub(super) workspace: Workspace,
    pub(super) app_id: String,
    pub(super) record: Option<String>,
    /// The resource the request names under its app's id now, when it was written under an id
    /// the app had before (`tl:<old>:<id>`); delivered as that, the request staying as written.
    pub(super) resource: Option<String>,
}

/// The broker as the [`ActionHost`](tilo_actions::remote::ActionHost) an
/// admitted application or agent connection reaches over its channel's
/// `remoteAction` door. [`OWN_PROVIDER`] names that connection itself.
#[derive(Clone)]
pub(super) struct ConnectionActionHost {
    pub(super) state: Arc<AppBrokerState>,
    pub(super) own_peer: u64,
    /// The connection's revalidated admission: an application's own effect,
    /// directed back to it, runs under this original caller's custody.
    pub(super) origin: super::published_invocation::InvocationCaller,
}

impl tilo_actions::remote::ActionHost for ConnectionActionHost {
    fn record_reads(&self) -> Option<std::sync::Arc<dyn tilo_actions::remote::RecordReads>> {
        self.state.record_reads()
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.state.action_query(caller, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        // A channel connection carries no launcher-authenticated scope, so a
        // content launch by hash is refused rather than widened.
        self.state.action_invoke_with_origin(
            caller,
            Some(self.own_peer),
            None,
            Some(&self.origin),
            &request,
        )
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        // What this connection admitted outlives the ledger that answered it
        // while it ran: the journal still holds it, and says so after the
        // application, the connection or this process is gone.
        self.state.action_observe(caller, id).or_else(|| {
            self.state
                .retained_observation(caller, &self.origin, *id)
                .ok()
                .flatten()
        })
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        // A connection interleaves many invocations, so only a change after
        // this call counts; the safety interval bounds a missed one.
        self.state
            .action_next_change(caller, id, self.state.invocation_changes())
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        // What the connection's own person or agent invoked is owned under
        // its certificate, not its actor name: cancel it as that owner.
        if let super::published_invocation::InvocationCaller::Local { admitted, .. } = &self.origin
            && let Some(certificate) = admitted.certificate.as_ref()
            && let Some(job) = self.state.remote_job(caller, id)
        {
            let owner = super::InvocationOwner::AuthenticatedLocal {
                root: certificate.user_public_key,
                device: certificate.device_public_key,
                agent: admitted.agent_id.clone(),
            };
            if self.state.can_cancel_for_owner(job, &owner) {
                return answered_when_durable(
                    self.state
                        .request_cancellation_for_owner(job, &owner)
                        .map_err(|reason| ActionHostError::NotCancellable { reason }),
                );
            }
        }
        self.state.action_cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<tilo_actions::remote::ConfirmedInvocation, ActionHostError>> {
        self.state.request_witness(caller, None, request)
    }
}

/// The broker's own door, as the [`ActionHost`](tilo_actions::remote::ActionHost)
/// a shortcut run invokes its steps through. It stands for no connection:
/// every step is admitted for the run's caller under its retained original
/// custody, with the run's grants and confirmations still checked per step.
pub(super) struct BrokerDoor {
    state: Arc<AppBrokerState>,
    /// Protected dispatch custody, scoped to this run rather than a connection.
    origin: Option<super::InvocationCaller>,
    /// The invocation changes the run had seen when it last observed, so
    /// a change between that observation and its next wait is not missed.
    seen: std::sync::Mutex<tokio::sync::watch::Receiver<u64>>,
}

impl BrokerDoor {
    pub(super) fn new(state: Arc<AppBrokerState>) -> Self {
        let seen = std::sync::Mutex::new(state.invocation_changes());
        Self {
            state,
            origin: None,
            seen,
        }
    }

    pub(super) fn for_run(
        state: Arc<AppBrokerState>,
        origin: Option<super::InvocationCaller>,
    ) -> Self {
        Self {
            origin,
            ..Self::new(state)
        }
    }

    fn step_origin(
        &self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<super::InvocationCaller> {
        self.origin.clone().map(|mut origin| {
            // Only the protected native variant binds a gesture to a UUID.
            // Rebind that evidence for this step; never manufacture custody
            // from the Caller or change a signed caller certificate.
            if let super::InvocationCaller::NativePerson { invocation: id, .. } = &mut origin {
                *id = invocation;
            }
            origin
        })
    }
}

impl tilo_actions::remote::ActionHost for BrokerDoor {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.state.action_query(caller, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let origin = self.step_origin(request.invocation);
        self.state
            .action_invoke_with_origin(caller, None, None, origin.as_ref(), &request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        // Marked before the snapshot: a change racing it wakes the next
        // wait (spuriously at worst) instead of slipping past it.
        if let Ok(mut seen) = self.seen.lock() {
            seen.mark_unchanged();
        }
        self.state.action_observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        let seen = self.seen.lock().ok()?.clone();
        self.state.action_next_change(caller, id, seen)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        self.state.action_cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<tilo_actions::remote::ConfirmedInvocation, ActionHostError>> {
        self.state.request_witness(caller, None, request)
    }
}

/// The host runs the action wherever its app lives; a published surface
/// carries no locus of its own.
fn descriptor(action: &PublishedAction) -> ActionDescriptor {
    action.descriptor(ExecutionLocus::default())
}

/// The action `offer` of the installed app `app` declares, as the catalogue
/// lists it while the app is closed. Its contract is what the signed
/// manifest says; once the app runs, its own published action admits the
/// invocation.
pub(super) fn offered_action(
    app: &crate::services::LaunchableApplication,
    offer: &tilo_app_package::manifest::OfferedAction,
) -> PublishedAction {
    use tilo_app_package::manifest::{OfferKind, OfferOperation};
    let kind = match offer.kind {
        OfferKind::Read => tilo_actions::ActionKind::Read,
        OfferKind::Undoable => tilo_actions::ActionKind::Undoable,
        OfferKind::Outside => tilo_actions::ActionKind::Outside,
    };
    let (risk, confirmation) = match (offer.confirm, offer.kind) {
        (true, _) => (
            tilo_actions::RiskLevel::Destructive,
            tilo_actions::ConfirmationPolicy::Confirm,
        ),
        (false, OfferKind::Read) => (
            tilo_actions::RiskLevel::None,
            tilo_actions::ConfirmationPolicy::None,
        ),
        (false, _) => (
            tilo_actions::RiskLevel::Low,
            tilo_actions::ConfirmationPolicy::None,
        ),
    };
    PublishedAction {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        resource: offer.resource.clone(),
        action: offer.action.clone(),
        resource_kind: offer.resource_kind().to_owned(),
        resource_label: app.display_name.clone(),
        action_label: offer.label.clone(),
        keywords: Vec::new(),
        icon: None,
        description: offer.description.clone(),
        operation_kind: match offer.operation {
            OfferOperation::Document => tilo_actions::OperationKind::Document,
            OfferOperation::Effect => tilo_actions::OperationKind::Effect,
            OfferOperation::View => tilo_actions::OperationKind::View,
        },
        params: Vec::new(),
        contract: tilo_actions::ActionContract {
            kind,
            risk,
            confirmation,
            ..tilo_actions::ActionContract::default()
        },
        view: None,
        input: super::PublishedInput::None,
    }
}

fn app_open_descriptor(app: &crate::services::LaunchableApplication) -> ActionDescriptor {
    let display_name = &app.display_name;
    let (resource_kind, description) = if app.unconfined {
        (
            "applications.host-app",
            "Launch this unconfined host application into the current workspace. It runs \
             outside every Tilo sandbox with the user's own files and publishes no typed \
             actions: drive its window with the computer tool",
        )
    } else {
        (
            APPLICATION_RESOURCE_KIND,
            "Launch this application into the current workspace",
        )
    };
    ActionDescriptor {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{}", app.package_id),
            APPLICATION_OPEN_ACTION,
        ),
        resource_kind: resource_kind.into(),
        resource_label: display_name.clone(),
        action_label: format!("Open {display_name}"),
        keywords: Vec::new(),
        icon: None,
        description: Some(description.into()),
        locus: ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        // It answers once the instance it started is up, naming it by
        // `peer_id` (see `workspace_open`).
        contract: tilo_actions::ActionContract {
            execution: tilo_actions::ExecutionMode::Background { cancellable: false },
            result: tilo_actions::ResultSpec::Job,
            ..tilo_actions::ActionContract::default()
        },
        // An invocation may carry the content hash of bytes to open
        // read-only as its text argument; without one the app opens empty.
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

/// A workspace's `start` of `app`: its instance with no window, where the host runs one,
/// answered with that instance as [`app_open_descriptor`]'s `open` is.
pub(super) fn app_start_descriptor(package_id: &str, display_name: &str) -> ActionDescriptor {
    let mut descriptor = app_open_descriptor(&crate::services::LaunchableApplication {
        package_id: package_id.to_owned(),
        display_name: display_name.to_owned(),
        unconfined: false,
        offers: Vec::new(),
        capabilities: Vec::new(),
    });
    descriptor.address.action = tilo_actions::remote::APPLICATION_START_ACTION.into();
    descriptor.action_label = format!("Start {display_name}");
    descriptor.description =
        Some("Start this application in the current workspace without showing it".into());
    descriptor
}

/// The address `request` is delivered at: on `resource`, the resource it names as its app
/// publishes it now, when it was written under an id the app had before.
fn current_address(resource: Option<&str>, request: &InvokeRequest) -> tilo_actions::ActionAddress {
    resource.map_or_else(
        || request.target.address.clone(),
        |resource| {
            tilo_actions::ActionAddress::new(resource, request.target.address.action.clone())
        },
    )
}

/// What launching an app for `request` needs of its person on the app:
/// leave to read it when the request shows something that exists in it (a
/// record, an item of one, such as a note shared with a Viewer), whose
/// window is then read-only to a person who may only read; leave to change
/// it for anything else, an app opened on nothing in particular included.
fn launch_kind(request: &InvokeRequest) -> tilo_action_policy::ActionKind {
    if request.target.address.action == APPLICATION_OPEN_ACTION
        && application_provider(&request.target.provider).is_some()
    {
        tilo_action_policy::ActionKind::Read
    } else {
        tilo_action_policy::ActionKind::Undoable
    }
}

/// Installed application `app` named to a caller that may not open it: its
/// resource with no action, so the caller learns the app is there and that
/// opening it is not theirs to do.
fn app_named_row(provider: &str, app: &crate::services::LaunchableApplication) -> CatalogueRow {
    let mut row = CatalogueRow::for_action(provider, &app.package_id, app_open_descriptor(app));
    row.action = None;
    row
}

fn app_close_descriptor(package_id: &str, display_name: &str) -> ActionDescriptor {
    ActionDescriptor {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{package_id}"),
            APPLICATION_CLOSE_ACTION,
        ),
        resource_kind: APPLICATION_RESOURCE_KIND.into(),
        resource_label: display_name.to_string(),
        action_label: format!("Close {display_name}"),
        keywords: Vec::new(),
        icon: None,
        description: Some(
            "Close this application's windows in the current workspace; opening it again \
             starts it with the permissions it holds now"
                .into(),
        ),
        locus: ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            risk: tilo_actions::RiskLevel::Low,
            retry: tilo_actions::RetryPolicy::Idempotent,
            ..tilo_actions::ActionContract::default()
        },
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

/// `(app, resource, action)` of every action retained for closed apps in
/// one workspace.
type RetainedAddresses = std::collections::HashSet<(String, String, String)>;

/// Which rows a [`AppBrokerState::eager_catalogue`] pass builds. Every
/// scope lists the same providers and puts each row it builds through the
/// same checks; a narrower one only skips rows nobody asked about — another
/// provider's, or another application's.
#[derive(Clone, Copy, Debug)]
pub(super) enum RowScope<'a> {
    /// Who publishes, and no rows.
    None,
    /// Every row the caller is offered.
    All,
    /// Every row of one provider.
    Provider(&'a str),
    /// Every row of one application, within `provider` when one is named.
    App {
        app_id: &'a str,
        provider: Option<&'a str>,
    },
}

impl RowScope<'_> {
    fn applications(
        self,
        launcher: &dyn crate::services::LauncherServices,
    ) -> Vec<crate::services::LaunchableApplication> {
        match self {
            Self::App { app_id, .. } => launcher.applications_for(app_id),
            Self::None => Vec::new(),
            Self::All | Self::Provider(_) => launcher.applications(),
        }
    }

    fn wants(self, provider: &str) -> bool {
        match self {
            Self::None => false,
            Self::All | Self::App { provider: None, .. } => true,
            Self::Provider(wanted)
            | Self::App {
                provider: Some(wanted),
                ..
            } => wanted == provider,
        }
    }

    /// Whether rows published by application `app_id` may be built. A
    /// provider publishing several applications (a workspace's) narrows to
    /// the one asked for; one application's own provider publishes its own
    /// rows either way.
    fn wants_app(self, app_id: &str) -> bool {
        match self {
            Self::None => false,
            Self::All | Self::Provider(_) => true,
            Self::App { app_id: wanted, .. } => wanted == app_id,
        }
    }

    /// Whether rows of an application provider (an installed app's actions
    /// offered while it is closed) may be built, so the installed
    /// applications need reading.
    fn wants_application_providers(self) -> bool {
        match self {
            Self::None => false,
            Self::All | Self::App { provider: None, .. } => true,
            Self::Provider(wanted)
            | Self::App {
                provider: Some(wanted),
                ..
            } => application_provider(wanted).is_some(),
        }
    }

    /// Whether rows of the live instance `peer_id` are built.
    fn wants_peer(self, peer_id: u64) -> bool {
        match self {
            Self::None => false,
            Self::All | Self::App { provider: None, .. } => true,
            Self::Provider(wanted)
            | Self::App {
                provider: Some(wanted),
                ..
            } => wanted == peer_id.to_string(),
        }
    }
}

/// The content an application's open action is asked to show. Its text
/// argument is a content hash; a typed caller (MCP) sends its argument map
/// in the canonical envelope, and an empty map is a plain open.
fn launch_content_hash(argument: Option<&str>) -> &str {
    let Some(argument) = argument else {
        return "";
    };
    match argument.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX) {
        Some(json)
            if serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                .is_ok_and(|map| map.is_empty()) =>
        {
            ""
        }
        _ => argument,
    }
}

/// Map a broker refusal onto the remote vocabulary. The broker words its
/// refusals for people; only the distinctions a caller can act on are
/// recovered, everything else stays a plain refusal.
/// What a start admitted: a host action already durable, or an app's
/// invocation nobody hears of before its admission is.
pub(super) enum Started {
    Now(u64),
    Staged(super::StagedJob),
}

impl Started {
    /// The job, once delivered: never before its admission is durable.
    async fn job(self, state: &AppBrokerState) -> Result<u64, ActionHostError> {
        match self {
            Self::Now(job) => Ok(job),
            Self::Staged(staged) => staged.start(state).await.map(|job| job.id).map_err(refusal),
        }
    }
}

/// Complete `reply` with `answer` once it is known.
pub(super) fn answer_with<T: Send + 'static>(
    answer: Pending<Result<T, ActionHostError>>,
    reply: Completer<Result<T, ActionHostError>>,
) {
    answer.then(move |answer| {
        reply.complete(answer.unwrap_or_else(|| {
            Err(ActionHostError::Unavailable {
                reason: "the admission was dropped".into(),
            })
        }));
    });
}

pub(super) fn refusal(reason: String) -> ActionHostError {
    if reason.starts_with("action_confirmation_required")
        || reason.contains("confirmation is required")
    {
        ActionHostError::confirmation_required()
    } else if reason.contains("not allowed") {
        ActionHostError::NotPermitted
    } else if reason.contains("no longer publishes")
        || reason.contains("unknown application action")
    {
        ActionHostError::UnknownAction
    } else {
        ActionHostError::Refused { reason }
    }
}

impl AppBrokerState {
    /// The caller an application connection's request is decided as.
    ///
    /// An application acts under its own identity: `person` is who admitted
    /// the connection, `app_id` the application it is. The app reaches what
    /// it owns — its own published surface, the launches the toolkit makes on
    /// its behalf, and actions published under its own id (the shortcut
    /// library is the shortcuts app's) — as that person, because those are
    /// the person's own clicks in it. Reading the catalogue needs the app to
    /// declare `[actions] query`; invoking another app's action needs
    /// `[actions] invoke` and is then decided for the app, which holds no
    /// role and has only what the workspace grants it. Sharing what an app
    /// shows is the workspace's invitation, which only its person makes: the
    /// app asks the desk for it as that person
    /// ([`app_admission::invitation_request`]), under their part in the workspace.
    pub(crate) fn app_door(
        &self,
        person: &Caller,
        app_id: &str,
        message: &tilo_actions::remote::RemoteActionMessage,
    ) -> Result<Caller, Box<tilo_actions::remote::RemoteActionMessage>> {
        use tilo_actions::remote::{RemoteActionMessage, refuse};
        let access = self.app_action_access(app_id);
        let mut as_app = Caller::local_app(app_id);
        if let Some(workspace) = person.workspace() {
            as_app = as_app.in_workspace(workspace);
        }
        let permitted = match message {
            RemoteActionMessage::CatalogueRequest { query, .. } => {
                (access.query || own_view_lookup(app_id, query)).then(|| person.clone())
            }
            // Another of the person's devices is reached only for the app's
            // own actions there, as the person using it; never under the
            // app's own identity, which holds no reach across devices.
            RemoteActionMessage::Invoke(request)
                if request.target.provider == tilo_actions::remote::own_device::PROVIDER =>
            {
                tilo_actions::remote::own_device::Invocation::from_argument(&request.argument)
                    .is_ok_and(|invocation| invocation.app == app_id)
                    .then(|| person.clone())
            }
            // A node of the person's own room asked about, shared or
            // unshared: what the person did in the app (a window shared to a
            // channel), which the workspace bounds by what the person holds there.
            // Never under the app's own identity, which the workspace knows not.
            RemoteActionMessage::Invoke(request)
                if workspace_nodes::node_ask(request)
                    .is_some_and(|(workspace, _)| person.workspace() == Some(workspace)) =>
            {
                Some(person.clone())
            }
            RemoteActionMessage::Invoke(request) => {
                if self.app_owns(person, app_id, request)
                    || reference_request(request)
                    || invitation_request(request)
                {
                    Some(person.clone())
                } else {
                    access.invoke.then_some(as_app)
                }
            }
            RemoteActionMessage::Observe { invocation, .. }
            | RemoteActionMessage::Cancel { invocation } => {
                Some(if self.remote_job(&as_app, invocation).is_some() {
                    as_app
                } else {
                    person.clone()
                })
            }
            // The app's own records it reads as the person using it; another
            // app's as itself, acting for them, by what the workspace grants it.
            RemoteActionMessage::RecordRead { read, .. } => Some(if read.app == app_id {
                person.clone()
            } else {
                match person.principal() {
                    tilo_actions::remote::CallerPrincipal::Person(id) => {
                        as_app.acting_for(id.clone())
                    }
                    _ => as_app,
                }
            }),
            RemoteActionMessage::FeedbackRequest { .. }
            // Questions travel between a person's own devices; an
            // application neither asks nor answers them.
            | RemoteActionMessage::ConfirmationRequest { .. }
            // A confirmation is asked for on the door that puts questions to
            // the person, never by an application.
            | RemoteActionMessage::Confirm { .. }
            // A retired verb: nothing asks for a reading this way, and
            // nothing is answered.
            | RemoteActionMessage::RetiredResourceWatch { .. }
            // An application puts content through its own store, not into
            // another device's over this door; nor does it read another
            // device's workspace content through it.
            | RemoteActionMessage::ContentPutRequest { .. }
            | RemoteActionMessage::ContentGetRequest { .. } => None,
            RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::RetiredRecordRequest
            | RemoteActionMessage::RetiredRecordResponse
            | RemoteActionMessage::RecordReading { .. }
            | RemoteActionMessage::FeedbackResponse { .. }
            | RemoteActionMessage::ConfirmationResponse { .. }
            | RemoteActionMessage::ConfirmResponse { .. }
            | RemoteActionMessage::RetiredResourceReading { .. }
            | RemoteActionMessage::ContentPutResponse { .. }
            | RemoteActionMessage::ContentGetResponse { .. }
            | RemoteActionMessage::CatalogueChanged { .. } => Some(person.clone()),
        };
        permitted.ok_or_else(|| {
            Box::new(refuse(message, ActionHostError::NotPermitted).unwrap_or(
                RemoteActionMessage::CatalogueResponse {
                    correlation: 0,
                    result: Err(ActionHostError::NotPermitted),
                },
            ))
        })
    }

    /// What application `app_id` declared it may do on the door.
    pub(crate) fn app_action_access(&self, app_id: &str) -> crate::app_actions::AppActionAccess {
        self.app_action_access
            .read()
            .map(|resolver| resolver(app_id))
            .unwrap_or_default()
    }

    /// Read declared access from `resolver` from now on.
    pub(crate) fn set_app_action_access(
        &self,
        resolver: crate::app_actions::AppActionAccessResolver,
    ) {
        if let Ok(mut current) = self.app_action_access.write() {
            *current = resolver;
        }
    }

    /// Whether `request` names something application `app_id` owns, as
    /// seen by the `person` using it.
    fn app_owns(&self, person: &Caller, app_id: &str, request: &InvokeRequest) -> bool {
        let target = &request.target;
        if target
            .provider
            .starts_with(super::canonical_route::PROVIDER_PREFIX)
        {
            return self
                .canonical_token_directory(&target.provider)
                .is_ok_and(|root| {
                    root.app_id == app_id
                        && self.directory_root_for(person, root.peer_id).as_ref() == Some(&root)
                });
        }
        if target.provider == OWN_PROVIDER
            || (provider_workspace(&target.provider).is_some()
                && target
                    .address
                    .resource
                    .starts_with(APPLICATION_RESOURCE_PREFIX)
                && target.address.action == APPLICATION_OPEN_ACTION)
        {
            return true;
        }
        if let Some(row) = self
            .eager_catalogue(person, RowScope::Provider(&target.provider))
            .0
            .into_iter()
            .find(|row| {
                row.provider == target.provider
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|descriptor| descriptor.address == target.address)
            })
        {
            return row.app_id == app_id;
        }
        let Ok(peer_id) = target.provider.parse::<u64>() else {
            return false;
        };
        self.published_surfaces()
            .iter()
            .any(|surface| surface.peer_id == peer_id && surface.app_id == app_id)
            || self
                .action_directory_roots()
                .iter()
                .any(|root| root.peer_id == peer_id && root.app_id == app_id)
    }

    /// Whether `caller` may act in `workspace`. A caller confined to a workspace
    /// was admitted to it by its connection; any other caller must be a
    /// member.
    fn may_act_in(caller: &Caller, workspace: &Workspace, now: u64) -> bool {
        match caller.workspace() {
            // Confinement says which workspace a connection was admitted to, not
            // that an agent still belongs to it: an agent whose membership
            // was removed must lose its catalogue and authority at once. It
            // takes part as the unconfined branch decides: an enrolled agent
            // through its own grant, an agent a person's device handed this
            // session to (`Caller::delegated_to`) through that person's.
            Some(confined) => {
                confined == workspace.id().to_string()
                    && match caller.principal() {
                        CallerPrincipal::Agent(_) => {
                            BrokerActor::for_caller(caller).reads_in(workspace, now)
                        }
                        CallerPrincipal::Person(_)
                        | CallerPrincipal::App(_)
                        | CallerPrincipal::Bot(_) => true,
                    }
            }
            None => match caller.principal() {
                // An application or a bot is always admitted into one workspace;
                // unconfined, it takes part in none.
                CallerPrincipal::App(_) | CallerPrincipal::Bot(_) => false,
                // Anyone else takes part in a workspace they may read; a foreign
                // device reads wherever its person does.
                CallerPrincipal::Person(_) | CallerPrincipal::Agent(_) => {
                    BrokerActor::for_caller(caller).reads_in(workspace, now)
                }
            },
        }
    }

    /// Why an agent confined to a workspace is served nothing there when the
    /// reason is not that the workspace holds nothing: this session's workspaces
    /// are not attached, the policy clock cannot be read, the workspace is
    /// gone, or the agent may no longer read in it. `None` for any other
    /// caller, and for an agent that may act in its workspace.
    fn agent_workspace_unreachable(&self, caller: &Caller) -> Option<String> {
        let workspace_id = caller.workspace()?;
        let CallerPrincipal::Agent(agent) = caller.principal() else {
            return None;
        };
        let Some(membership) = self.membership.as_ref() else {
            return Some("this session's workspaces are not attached to its actions".into());
        };
        let now = match self.policy_now_ns() {
            Ok(now) => now / 1_000_000_000,
            Err(error) => return Some(format!("the policy clock cannot be read: {error}")),
        };
        let found = membership.decide_in_tree(|tree| {
            tree.workspaces
                .iter()
                .find(|workspace| workspace.id().to_string() == workspace_id)
                .map(|workspace| {
                    (
                        workspace.name.clone(),
                        Self::may_act_in(caller, workspace, now),
                    )
                })
        });
        match found {
            Err(error) => Some(format!("this session's workspaces cannot be read: {error}")),
            Ok(None) => Some(format!(
                "the workspace agent {agent} was admitted to is no longer on this device"
            )),
            Ok(Some((name, false))) => Some(format!(
                "agent {agent} is no longer a member of workspace {name} who may read it"
            )),
            Ok(Some((_, true))) => None,
        }
    }

    /// One policy sample of `workspace` for `caller`: `decide` runs once on the
    /// borrowed committed workspace with the caller's policy actor there, or
    /// not at all (`None`) when `caller` may not act in `workspace`. Nothing is
    /// copied; `decide` must not block or call host services.
    pub(super) fn decide_for<R>(
        &self,
        caller: &Caller,
        workspace: &str,
        decide: impl FnOnce(&Workspace, &BrokerActor, u64) -> R,
    ) -> Option<R> {
        let membership = self.membership.as_ref()?;
        // An unavailable clock offers nothing rather than deciding at an
        // unknown time.
        let now = self.policy_now_ns().ok()? / 1_000_000_000;
        // The actor consults the certificate authority: asked before the
        // workspace is borrowed, never under it.
        let actor = self.caller_actor(caller, workspace);
        membership
            .decide_in_workspace(workspace, |workspace| {
                let workspace =
                    workspace.filter(|workspace| Self::may_act_in(caller, workspace, now))?;
                Some(decide(workspace, &actor, now))
            })
            .ok()
            .flatten()
    }

    /// The workspaces `caller` may act in, decided on one borrowed sample.
    fn workspace_ids_for(&self, caller: &Caller) -> Vec<String> {
        let (Some(membership), Ok(now)) = (
            self.membership.as_ref(),
            self.policy_now_ns().map(|now| now / 1_000_000_000),
        ) else {
            return Vec::new();
        };
        membership
            .decide_in_tree(|tree| {
                tree.workspaces
                    .iter()
                    .filter(|workspace| Self::may_act_in(caller, workspace, now))
                    .map(|workspace| workspace.id().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    /// Owned copies of the workspaces `caller` may act in, with the actor
    /// it is in each, all from one sample. Only for a pass that hands each
    /// workspace to host services (which read the host's authority
    /// themselves) and so cannot run under the borrow; only the caller's
    /// own workspaces are copied.
    pub(super) fn workspaces_for(&self, caller: &Caller) -> Vec<(Workspace, BrokerActor, u64)> {
        let Some(membership) = self.membership.as_ref() else {
            return Vec::new();
        };
        // One policy clock sample for the whole pass; an unavailable clock
        // offers nothing rather than deciding at an unknown time.
        let Ok(now) = self.policy_now_ns().map(|now| now / 1_000_000_000) else {
            return Vec::new();
        };
        let Ok(workspaces) = membership.decide_in_tree(|tree| {
            tree.workspaces
                .iter()
                .filter(|workspace| Self::may_act_in(caller, workspace, now))
                .cloned()
                .collect::<Vec<_>>()
        }) else {
            return Vec::new();
        };
        workspaces
            .into_iter()
            .map(|workspace| {
                let actor = self.caller_actor(caller, &workspace.id().to_string());
                (workspace, actor, now)
            })
            .collect()
    }

    /// Whether `actor` is offered the action `target` (resource kind,
    /// resource, action) the app `app` publishes under `contract`: it may
    /// take it, or would be asked first.
    pub(super) fn offered(
        workspace: &Workspace,
        actor: &BrokerActor,
        now: u64,
        app: &str,
        target: (&str, &str, &str),
        contract: &tilo_actions::ActionContract,
    ) -> bool {
        actor.action_answer_in(workspace, app, target, &contract.need(), now)
            != tilo_action_policy::Answer::Deny
    }

    /// Whether `actor` may take the action `target` now
    /// ([`tilo_action_policy::Answer::admits`]).
    pub(super) fn allowed(
        workspace: &Workspace,
        actor: &BrokerActor,
        now: u64,
        app: &str,
        target: (&str, &str, &str),
        contract: &tilo_actions::ActionContract,
    ) -> bool {
        actor
            .action_answer_in(workspace, app, target, &contract.need(), now)
            .admits(actor.direct())
    }

    /// Every live published surface, carrying its actions only where
    /// `scope` builds that surface's rows: who publishes is copied, not every
    /// app's whole surface, when that is all a caller asked.
    pub(super) fn live_surfaces(&self, scope: RowScope<'_>) -> Vec<PublishedSurface> {
        self.surfaces
            .lock()
            .map(|surfaces| {
                surfaces
                    .values()
                    .filter(|surface| surface.live)
                    .map(|surface| PublishedSurface {
                        peer_id: surface.peer_id,
                        app_id: surface.app_id.clone(),
                        route: surface.route.clone(),
                        actions: if scope.wants_peer(surface.peer_id)
                            && scope.wants_app(&surface.app_id)
                        {
                            surface.actions.clone()
                        } else {
                            Vec::new()
                        },
                        live: true,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one authorization pass projects live surfaces, directories, governance and launchers so no source can skip the caller's checks"
    )]
    /// Every eager row `caller` is offered within `scope`: live application
    /// actions its role may invoke, the workspace's own governance actions,
    /// and — for a caller on this machine — launching an application it may
    /// open. An action the caller may not use is absent. Alongside them, the
    /// publishers the caller may browse or invoke, whatever the scope.
    ///
    /// A narrower scope only leaves other providers' rows, or other
    /// applications' rows, out: each row it builds passes the same checks,
    /// and comes out the same, as in [`RowScope::All`]. The installed
    /// applications and the keyboard shortcut settings are read at most once,
    /// and only when a workspace provider's rows of an application the scope
    /// asks for are built.
    pub(super) fn eager_catalogue(
        &self,
        caller: &Caller,
        scope: RowScope<'_>,
    ) -> (Vec<CatalogueRow>, Vec<CatalogueProvider>) {
        self.eager_catalogue_with_origin(caller, None, scope)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the catalogue is built in one pass over every provider so installed apps and shortcut settings are read at most once and each row passes the same checks"
    )]
    fn eager_catalogue_with_origin(
        &self,
        caller: &Caller,
        origin: Option<&super::InvocationCaller>,
        scope: RowScope<'_>,
    ) -> (Vec<CatalogueRow>, Vec<CatalogueProvider>) {
        // Retained surfaces of closed apps are the launchable catalogue's,
        // and only the providers whose rows are built need their actions.
        let surfaces = self.live_surfaces(scope);
        let roots = self.action_directory_roots();
        // Listed at most once, for every workspace's open and close rows.
        let mut applications = None;
        let mut rows = Vec::new();
        let mut providers = Vec::new();
        // Keyboard shortcuts are this device's own settings: offered to
        // callers on this machine, never to a remote device.
        let keyboard = self
            .services
            .keyboard_shortcuts
            .as_ref()
            .filter(|_| caller.device_public_key().is_none());
        let mut keymap = None;
        // Listing must retain the same local custody as dispatch. A device key
        // alone does not establish that this is a local rather than remote door.
        let host_steps = if super::host_steps::is_local_host_caller(caller, origin) {
            self.services.host_steps.clone()
        } else {
            Vec::new()
        };
        for (workspace, policy, now) in self.workspaces_for(caller) {
            let first = rows.len();
            let workspace_id = workspace.id().to_string();
            let mut live_apps = std::collections::HashSet::new();
            // The apps with a window here: one running only without a window
            // is still opened, which gives that instance its window.
            let mut windowed_apps = std::collections::HashSet::new();
            for surface in surfaces
                .iter()
                .filter(|surface| surface.live && surface.route.cluster_id == workspace_id)
            {
                live_apps.insert(surface.app_id.clone());
                if !self.is_windowless(surface.peer_id) {
                    windowed_apps.insert(surface.app_id.clone());
                }
                providers.push(CatalogueProvider {
                    provider: surface.peer_id.to_string(),
                    app_id: surface.app_id.clone(),
                    workspace: workspace_id.clone(),
                    record_id: surface.route.record_id.clone(),
                    browsable: roots.iter().any(|root| root.peer_id == surface.peer_id),
                });
                if !scope.wants_peer(surface.peer_id) || !scope.wants_app(&surface.app_id) {
                    continue;
                }
                rows.extend(
                    surface
                        .actions
                        .iter()
                        .filter(|action| {
                            Self::offered(
                                &workspace,
                                &policy,
                                now,
                                &surface.app_id,
                                (&action.resource_kind, &action.resource, &action.action),
                                &action.contract,
                            )
                        })
                        .map(|action| {
                            CatalogueRow::for_action(
                                surface.peer_id.to_string(),
                                &surface.app_id,
                                descriptor(action),
                            )
                        }),
                );
            }
            if caller.workspace().is_some() {
                let retained = self.launchable_catalogue(
                    &workspace,
                    &policy,
                    now,
                    &live_apps,
                    (scope, &mut rows),
                    &mut providers,
                );
                if let Some(launcher) = self
                    .services
                    .launcher
                    .as_ref()
                    .filter(|_| scope.wants_application_providers())
                {
                    let applications = applications.get_or_insert_with(|| {
                        (
                            scope.applications(launcher.as_ref()),
                            launcher.closes_applications(),
                        )
                    });
                    Self::offered_catalogue(
                        (&workspace, &policy, now),
                        (&live_apps, &retained),
                        &applications.0,
                        (scope, &mut rows),
                    );
                }
            }
            let unlisted = roots
                .iter()
                .filter(|root| {
                    root.cluster_id == workspace_id
                        && !providers
                            .iter()
                            .any(|provider| provider.provider == root.peer_id.to_string())
                })
                .cloned()
                .collect::<Vec<_>>();
            for root in unlisted {
                providers.push(CatalogueProvider {
                    provider: root.peer_id.to_string(),
                    app_id: root.app_id.clone(),
                    workspace: workspace_id.clone(),
                    record_id: root.record_id.clone(),
                    browsable: true,
                });
            }
            if caller.workspace() == Some(workspace_id.as_str()) {
                for steps in &host_steps {
                    // The host's own services are publishers like an app's
                    // window: listed whatever rows were asked for (a query
                    // asking only who publishes builds none), so a caller
                    // finds them by their app.
                    providers.push(CatalogueProvider {
                        provider: steps.provider().to_owned(),
                        app_id: steps.app_id().to_owned(),
                        workspace: workspace_id.clone(),
                        record_id: String::new(),
                        browsable: true,
                    });
                    if scope.wants(steps.provider()) && scope.wants_app(steps.app_id()) {
                        Self::host_step_rows((&workspace, &policy, now), steps.as_ref(), &mut rows);
                    }
                }
            }
            let provider = workspace_provider(&workspace_id);
            if !scope.wants(&provider) {
                continue;
            }
            if let Some(governance) = self
                .services
                .workspace_governance
                .as_ref()
                .filter(|governance| scope.wants_app(governance.app_id()))
            {
                rows.extend(
                    governance
                        .descriptors(&workspace)
                        .into_iter()
                        .filter(|descriptor| {
                            Self::offered(
                                &workspace,
                                &policy,
                                now,
                                governance.app_id(),
                                (
                                    &descriptor.resource_kind,
                                    &descriptor.address.resource,
                                    &descriptor.address.action,
                                ),
                                &descriptor.contract,
                            )
                        })
                        .map(|descriptor| {
                            CatalogueRow::for_action(
                                provider.clone(),
                                governance.app_id(),
                                descriptor,
                            )
                        }),
                );
            }
            if self.services.shortcut_library.is_some() && scope.wants_app(SHORTCUTS_APP_ID) {
                rows.extend(
                    automation::descriptors(&workspace)
                        .into_iter()
                        .filter(|descriptor| {
                            Self::offered(
                                &workspace,
                                &policy,
                                now,
                                SHORTCUTS_APP_ID,
                                (
                                    &descriptor.resource_kind,
                                    &descriptor.address.resource,
                                    &descriptor.address.action,
                                ),
                                &descriptor.contract,
                            )
                        })
                        .map(|descriptor| {
                            CatalogueRow::for_action(provider.clone(), SHORTCUTS_APP_ID, descriptor)
                        }),
                );
            }
            if let Some(service) = keyboard.filter(|service| scope.wants_app(service.app_id())) {
                let keymap =
                    keymap.get_or_insert_with(settings_store::hardware::ShortcutConfig::load);
                rows.extend(
                    service
                        .catalogue_rows(&provider, keymap)
                        .into_iter()
                        .filter(|row| {
                            row.action.as_ref().is_some_and(|descriptor| {
                                Self::offered(
                                    &workspace,
                                    &policy,
                                    now,
                                    service.app_id(),
                                    (
                                        &descriptor.resource_kind,
                                        &descriptor.address.resource,
                                        &descriptor.address.action,
                                    ),
                                    &descriptor.contract,
                                )
                            })
                        }),
                );
            }
            if caller.workspace().is_some()
                && let Some(launcher) = self.services.launcher.as_ref()
            {
                let applications = applications.get_or_insert_with(|| {
                    (
                        scope.applications(launcher.as_ref()),
                        launcher.closes_applications(),
                    )
                });
                let (applications, closes_applications) = (&applications.0, applications.1);
                // An agent of this machine is told every installed app, one
                // it may not open by name alone: an empty list would tell it
                // there are none, and it could only guess why. Its own
                // process reads the same list from disk; a remote caller is
                // told only what it may open. A scope asking for one
                // application is told only that one's.
                let names_unopened = matches!(caller.principal(), CallerPrincipal::Agent(_))
                    && caller.device_public_key().is_none();
                for app in applications
                    .iter()
                    .filter(|app| scope.wants_app(&app.package_id))
                    .filter(|app| !windowed_apps.contains(&app.package_id))
                {
                    if policy.may_in(
                        &workspace,
                        &app_node(&workspace, &app.package_id),
                        tilo_action_policy::ActionKind::Undoable,
                        now,
                    ) {
                        rows.push(CatalogueRow::for_action(
                            provider.clone(),
                            &app.package_id,
                            app_open_descriptor(app),
                        ));
                    } else if names_unopened {
                        rows.push(app_named_row(&provider, app));
                    }
                }
                // A running app is offered to close where the host closes
                // apps; where it does not, an agent of this machine is still
                // told its name, which is how it addresses the app.
                rows.extend(
                    applications
                        .iter()
                        .filter(|app| scope.wants_app(&app.package_id))
                        .filter(|app| live_apps.contains(&app.package_id))
                        .filter(|app| {
                            policy.may_in(
                                &workspace,
                                &app_node(&workspace, &app.package_id),
                                tilo_action_policy::ActionKind::Undoable,
                                now,
                            )
                        })
                        .filter_map(|app| {
                            if closes_applications {
                                Some(CatalogueRow::for_action(
                                    provider.clone(),
                                    &app.package_id,
                                    app_close_descriptor(&app.package_id, &app.display_name),
                                ))
                            } else {
                                names_unopened.then(|| app_named_row(&provider, app))
                            }
                        }),
                );
            }
            if let Some(added) = rows.get_mut(first..) {
                Self::mark_asks(&workspace, &policy, now, added);
            }
        }
        (rows, providers)
    }

    /// Mark each of `rows` whose action the workspace's grants let `actor` take
    /// only after asking: it asks for confirmation, which the person gives
    /// on their own device.
    /// The rows of host steps `steps` that `actor` is offered in
    /// `workspace` at `now`, appended to `rows`, each marked where it asks
    /// first.
    fn host_step_rows(
        (workspace, actor, now): (&Workspace, &BrokerActor, u64),
        steps: &dyn crate::services::HostStepServices,
        rows: &mut Vec<CatalogueRow>,
    ) {
        let start = rows.len();
        rows.extend(
            steps
                .rows()
                .into_iter()
                .filter(|placed| {
                    Self::offered(
                        workspace,
                        actor,
                        now,
                        steps.app_id(),
                        (
                            &placed.action.resource_kind,
                            &placed.action.address.resource,
                            &placed.action.address.action,
                        ),
                        &placed.action.contract,
                    )
                })
                .map(|placed| {
                    CatalogueRow::for_placed_action(
                        steps.provider().to_owned(),
                        steps.app_id(),
                        placed,
                    )
                }),
        );
        if let Some(added) = rows.get_mut(start..) {
            Self::mark_asks(workspace, actor, now, added);
        }
    }

    fn mark_asks(workspace: &Workspace, actor: &BrokerActor, now: u64, rows: &mut [CatalogueRow]) {
        for row in rows {
            let app = row.app_id.clone();
            let Some(descriptor) = row.action.as_mut() else {
                continue;
            };
            if descriptor.contract.confirmation != tilo_actions::ConfirmationPolicy::None {
                continue;
            }
            if actor.action_answer_in(
                workspace,
                &app,
                (
                    &descriptor.resource_kind,
                    &descriptor.address.resource,
                    &descriptor.address.action,
                ),
                &descriptor.contract.need(),
                now,
            ) == tilo_action_policy::Answer::Ask
            {
                descriptor.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
            }
        }
    }

    /// Actions retained from applications that are closed in `workspace`,
    /// offered to a caller on this machine that may launch the application
    /// and use the action. Invoking one launches the app first. Returns
    /// every `(app, resource, action)` retained in the workspace, offered or
    /// not: what an app published supersedes what its manifest declares.
    fn launchable_catalogue(
        &self,
        workspace: &Workspace,
        policy: &BrokerActor,
        now: u64,
        live_apps: &std::collections::HashSet<String>,
        (scope, rows): (RowScope<'_>, &mut Vec<CatalogueRow>),
        providers: &mut Vec<CatalogueProvider>,
    ) -> RetainedAddresses {
        let workspace_id = workspace.id().to_string();
        let in_room = self
            .launchable
            .surfaces()
            .into_iter()
            .filter(|surface| surface.route.cluster_id == workspace_id)
            .collect::<Vec<_>>();
        let retained = in_room
            .iter()
            .flat_map(|surface| {
                surface.actions.iter().map(|action| {
                    (
                        surface.app_id.clone(),
                        action.resource.clone(),
                        action.action.clone(),
                    )
                })
            })
            .collect::<RetainedAddresses>();
        for surface in in_room.into_iter().filter(|surface| {
            !live_apps.contains(&surface.app_id)
                && policy.may_in(
                    workspace,
                    &app_node(workspace, &surface.app_id),
                    tilo_action_policy::ActionKind::Undoable,
                    now,
                )
        }) {
            let provider = launchable_provider(&surface);
            if scope.wants(&provider) && scope.wants_app(&surface.app_id) {
                rows.extend(
                    surface
                        .actions
                        .iter()
                        .filter(|action| {
                            Self::offered(
                                workspace,
                                policy,
                                now,
                                &surface.app_id,
                                (&action.resource_kind, &action.resource, &action.action),
                                &action.contract,
                            )
                        })
                        .map(|action| {
                            CatalogueRow::for_action(
                                provider.clone(),
                                &surface.app_id,
                                descriptor(action),
                            )
                        }),
                );
            }
            providers.push(CatalogueProvider {
                provider,
                app_id: surface.app_id.clone(),
                workspace: workspace_id.clone(),
                record_id: surface.route.record_id.clone(),
                browsable: false,
            });
        }
        retained
    }

    /// The actions installed applications' signed manifests offer while
    /// they are closed (`[[offers]]`), for the apps not running in
    /// `workspace`, offered on the same terms as an action they published:
    /// the caller may launch the app and use the action. An action the app
    /// already published there, retained in the launchable catalogue, is
    /// listed from that instead. Invoking one launches the app, and the
    /// action it publishes admits the invocation.
    fn offered_catalogue(
        (workspace, policy, now): (&Workspace, &BrokerActor, u64),
        (live_apps, retained): (&std::collections::HashSet<String>, &RetainedAddresses),
        applications: &[crate::services::LaunchableApplication],
        (scope, rows): (RowScope<'_>, &mut Vec<CatalogueRow>),
    ) {
        let workspace_id = workspace.id().to_string();
        for app in applications.iter().filter(|app| {
            scope.wants_app(&app.package_id)
                && !app.unconfined
                && !app.offers.is_empty()
                && !live_apps.contains(&app.package_id)
                && policy.may_in(
                    workspace,
                    &app_node(workspace, &app.package_id),
                    tilo_action_policy::ActionKind::Undoable,
                    now,
                )
        }) {
            let provider = tilo_actions::remote::application_action_target(
                &workspace_id,
                &app.package_id,
                "",
                "",
            )
            .provider;
            if !scope.wants(&provider) {
                continue;
            }
            rows.extend(
                app.offers
                    .iter()
                    .filter(|offer| {
                        !retained.contains(&(
                            app.package_id.clone(),
                            offer.resource.clone(),
                            offer.action.clone(),
                        ))
                    })
                    .map(|offer| offered_action(app, offer))
                    .filter(|action| {
                        Self::offered(
                            workspace,
                            policy,
                            now,
                            &app.package_id,
                            (&action.resource_kind, &action.resource, &action.action),
                            &action.contract,
                        )
                    })
                    .map(|action| {
                        CatalogueRow::for_action(
                            provider.clone(),
                            &app.package_id,
                            descriptor(&action),
                        )
                    }),
            );
        }
    }

    /// One page of what `caller` is offered. A provider publishing a live
    /// directory is browsed there, on the broker's local set; everything
    /// else is answered at once from the eager surfaces.
    pub fn action_query(
        self: &Arc<Self>,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.action_query_with_origin(caller, None, query)
    }

    pub(super) fn action_query_with_origin(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::InvocationCaller>,
        query: &CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        if let Err(error) = check_execution(caller) {
            return Pending::ready(Err(error));
        }
        if query.canonical_route.is_some() {
            return Pending::ready(self.canonical_provider_page(caller, query));
        }
        if let Some(token) = query
            .provider
            .as_deref()
            .filter(|provider| provider.starts_with(super::canonical_route::PROVIDER_PREFIX))
        {
            let root = match self.canonical_token_directory(token) {
                Ok(root)
                    if self.directory_root_for(caller, root.peer_id).as_ref() == Some(&root) =>
                {
                    root
                }
                Ok(_) => return Pending::ready(Err(ActionHostError::NotPermitted)),
                Err(error) => return Pending::ready(Err(error)),
            };
            let state = self.clone();
            let token = token.to_owned();
            return self
                .query_directory(caller, &root, query)
                .map(move |result| {
                    let mut page = result?;
                    if state.canonical_token_directory(&token)? != root {
                        return Err(ActionHostError::UnknownAction);
                    }
                    for row in &mut page.rows {
                        row.provider.clone_from(&token);
                    }
                    Ok(page)
                });
        }
        if let Some(root) = query
            .provider
            .as_deref()
            // ignored-failure: a provider that is not a peer number names a canonical, launchable or host provider, not a live publication
            .and_then(|provider| provider.parse::<u64>().ok())
            .and_then(|peer_id| self.directory_root_for(caller, peer_id))
        {
            return self.query_directory(caller, &root, query);
        }
        // `max_results: 0` asks only who publishes, and a named provider
        // or application only its own rows: build no row nobody asked for.
        let scope = match (query.provider.as_deref(), query.app_id.as_deref()) {
            (Some(provider), Some(app_id)) => RowScope::App {
                app_id,
                provider: Some(provider),
            },
            (Some(provider), None) => RowScope::Provider(provider),
            (None, _) if query.max_results == 0 => RowScope::None,
            (None, Some(app_id)) => RowScope::App {
                app_id,
                provider: None,
            },
            (None, None) => RowScope::All,
        };
        let (rows, providers) = self.eager_catalogue_with_origin(caller, origin, scope);
        // Nothing at all may mean the workspace holds nothing, or that this
        // agent cannot reach it: only the first is an empty page.
        if rows.is_empty()
            && providers.is_empty()
            && let Some(reason) = self.agent_workspace_unreachable(caller)
        {
            return Pending::ready(Err(ActionHostError::Unavailable { reason }));
        }
        let revision = format!(
            "{}.{}",
            self.published_surfaces_generation(),
            self.action_directory_root_page(None, 0).generation
        );
        let mut page = CataloguePage::from_rows(revision, rows, query);
        if query.provider.is_none() {
            page.providers = providers;
        }
        Pending::ready(Ok(page))
    }

    /// Admit `request` for `caller`, or report the invocation already
    /// admitted under its id. `own_peer` resolves [`OWN_PROVIDER`] for a
    /// caller that is itself an application connection, and `launch` is the
    /// application the launcher authenticated for it. Never blocks: an
    /// action published through a live directory is re-resolved by its owner
    /// on the broker's local set, and the admission arrives when it has.
    pub fn action_invoke(
        self: &Arc<Self>,
        caller: &Caller,
        own_peer: Option<u64>,
        launch: Option<&LaunchScope>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.action_invoke_with_origin(caller, own_peer, launch, None, request)
    }

    /// [`Self::action_invoke`] for a caller whose original invocation custody
    /// the broker holds (an admitted local connection): an application
    /// action it starts carries that custody to the application, so a scoped
    /// effect runs under its original caller rather than without one.
    pub(super) fn action_invoke_with_origin(
        self: &Arc<Self>,
        caller: &Caller,
        own_peer: Option<u64>,
        launch: Option<&LaunchScope>,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(request) {
            return Pending::ready(Err(refused));
        }
        if let Err(error) = check_execution(caller) {
            return Pending::ready(Err(error));
        }
        // Host-service replay is an observation under the original custody,
        // not another dispatch through a possibly changed live catalogue.
        if let Some(origin) = origin {
            match self.host_action_replay(caller, origin, request) {
                Ok(Some(receipt)) => return Pending::ready(Ok(receipt)),
                Ok(None) => {}
                Err(error) => return Pending::ready(Err(error)),
            }
        }
        if request
            .target
            .provider
            .starts_with(super::canonical_route::PROVIDER_PREFIX)
        {
            // Exact UUID/request replay precedes live selection checks. A retired
            // custodian must not turn an admitted request into another dispatch.
            if let Some(job) = self.remote_job(caller, &request.invocation) {
                let replay = self.reserve_invocation(caller, request);
                return Pending::ready(replay.map(|_| self.receipt_for(request.invocation, job)));
            }
            let root = match self.canonical_token_directory(&request.target.provider) {
                Ok(root)
                    if self.directory_root_for(caller, root.peer_id).as_ref() == Some(&root) =>
                {
                    root
                }
                Ok(_) => return Pending::ready(Err(ActionHostError::NotPermitted)),
                Err(error) => return Pending::ready(Err(error)),
            };
            return self.invoke_directory(caller, root, origin, request);
        }
        if is_launchable_provider(&request.target.provider)
            || application_provider(&request.target.provider).is_some()
            || super::windowless_instances::started_app(request).is_some()
        {
            return self.invoke_launchable(caller, origin, request);
        }
        if workspace_nodes::node_ask(request).is_some() {
            return self.invoke_workspace_node(caller, origin, request);
        }
        if let Some(root) = self.directory_target(caller, request) {
            return self.invoke_directory(caller, root, origin, request);
        }
        if self.services.shortcut_library.is_some()
            && let Some(workspace) = provider_workspace(&request.target.provider)
            && let Some(shortcut) = automation::run_target(&request.target.address)
        {
            return Pending::ready(
                self.invoke_shortcut_run(caller, origin, workspace, shortcut, request),
            );
        }
        if let Some(steps) =
            super::host_steps::for_provider(&self.services, &request.target.provider)
        {
            return Pending::ready(self.invoke_host_step(caller, origin, steps.as_ref(), request));
        }
        if request.target.provider == tilo_actions::remote::own_device::PROVIDER {
            return Pending::ready(self.invoke_own_device(caller, origin, request));
        }
        if let Some(origin) = origin {
            if let Some(job) = self.remote_job(caller, &request.invocation) {
                let replay = self.reserve_invocation(caller, request);
                return Pending::ready(replay.map(|_| self.receipt_for(request.invocation, job)));
            }
            // Admission can sign and durably record an invocation. Keep only
            // inert custody on the UI turn; the broker revalidates it at dispatch.
            let (reply, response) = pending();
            return match self.action_directory_request_queue().enqueue(
                super::ActionDirectoryBridgeRequest::InvokeEager(Box::new(
                    super::EagerActionInvokeRequest {
                        caller: caller.clone(),
                        own_peer,
                        launch: launch.cloned(),
                        origin: origin.clone(),
                        request: request.clone(),
                        reply,
                    },
                )),
            ) {
                Ok(()) => response,
                Err(error) => Pending::ready(Err(bridge_error(error))),
            };
        }
        self.invoke_eager(caller, own_peer, launch, None, request)
    }

    /// Invoke an action retained from a closed application: launch the app,
    /// wait for it to publish the action and its invocation stream, and
    /// admit the invocation against what it publishes then — its current
    /// contract, availability and confirmation. The wait is bounded, and a
    /// caller that stops waiting cancels the delivery.
    fn invoke_launchable(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let reservation = self.reserve_invocation(caller, request);
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(Reservation::Admitted) => {
                return self.admitted_receipt(caller, request.invocation);
            }
            Ok(Reservation::Reserved) => {}
        }
        let destination = match self.admit_launchable(caller, request) {
            Ok(admitted) => admitted,
            Err(error) => {
                if let Ok(mut ledger) = self.remote_invocations.lock() {
                    ledger.abandon(&request.invocation);
                }
                return Pending::ready(Err(error));
            }
        };
        let runtime = match super::launchable::delivery_runtime() {
            Ok(runtime) => runtime,
            Err(error) => {
                if let Ok(mut ledger) = self.remote_invocations.lock() {
                    ledger.abandon(&request.invocation);
                }
                return Pending::ready(Err(ActionHostError::Unavailable {
                    reason: format!("the launch could not start: {error}"),
                }));
            }
        };
        let origin = origin.cloned();
        let (completer, answer) = pending();
        let state = Arc::clone(self);
        let caller = caller.clone();
        let request = request.clone();
        // Launching blocks and waiting for the app is an event: the launch
        // runs on the delivery runtime's blocking pool, the wait is a task
        // there, and neither runs on the broker's local set.
        runtime.spawn(async move {
            let started = state
                .relaunch_and_deliver(
                    &completer,
                    &caller,
                    &destination,
                    origin.as_ref(),
                    &request,
                    None,
                )
                .await;
            let result = match state.remote_invocations.lock() {
                Ok(mut ledger) => match started {
                    Ok(job) => {
                        ledger.complete(&request.invocation, job);
                        drop(ledger);
                        Ok(state.receipt_for(request.invocation, job))
                    }
                    Err(error) => {
                        ledger.abandon(&request.invocation);
                        Err(error)
                    }
                },
                Err(_) => Err(ActionHostError::Unavailable {
                    reason: "the invocation ledger is unavailable".into(),
                }),
            };
            completer.complete(result);
        });
        answer
    }

    /// Check a launchable invocation before anything starts: the caller may
    /// launch the app and use the action, and carries the confirmation its
    /// contract asks for, so a refusal never costs a launch. An application
    /// target names an app that may never have run in the workspace; what it
    /// offers is then only known once it publishes, and the delivery admits
    /// the invocation against that. Returns the workspace to launch into and
    /// the application.
    fn admit_launchable(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<LaunchDestination, ActionHostError> {
        // A target written under an id its app had before (a reference kept in a note) opens the
        // app that took its place, on the resource as that app publishes it; the request stays
        // as written.
        let earlier = self.earlier_ids();
        let resource = earlier.current_resource(&request.target.address.resource);
        let address = &current_address(resource.as_deref(), request);
        let (workspace, app_id, record, offered) =
            if let Some((workspace, app_id)) = super::windowless_instances::started_app(request) {
                // A workspace's `start` of its app offers nothing of the app: it
                // runs the app, so the caller may launch it.
                (workspace.to_owned(), app_id.to_owned(), None, None)
            } else if let Some((workspace, app_id, offered)) = self.application_offer(request) {
                let record = application_provider_record(&request.target.provider);
                (workspace, app_id, record.map(str::to_owned), offered)
            } else {
                let retained = self
                    .launchable
                    .surface_for(&request.target.provider)
                    .ok_or(ActionHostError::UnknownAction)?;
                let offered = retained
                    .actions
                    .into_iter()
                    .find(|action| {
                        action.resource == address.resource && action.action == address.action
                    })
                    .ok_or(ActionHostError::UnknownAction)?;
                (
                    retained.route.cluster_id,
                    retained.app_id,
                    Some(retained.route.record_id),
                    Some(offered),
                )
            };
        // Only a caller on this machine launches apps on this desktop.
        if caller.workspace().is_none() {
            return Err(ActionHostError::UnknownAction);
        }
        // The launcher keeps the workspace past the read: only this
        // workspace's authority is copied, once every check on the same sample passed.
        let workspace_id = workspace;
        let workspace = self
            .decide_for(caller, &workspace_id, |workspace, policy, now| {
                // A live application target is a route to an existing action,
                // not a request to launch another instance. Reads retain their
                // read-only authority while actual launches still require
                // write, unless they only show what exists (`launch_kind`).
                if self
                    .ready_delivery(&app_id, &workspace_id, record.as_deref(), address)
                    .is_none()
                    && !policy.may_in(
                        workspace,
                        &app_node(workspace, &app_id),
                        launch_kind(request),
                        now,
                    )
                {
                    return Err(refusal(
                        "the caller is not allowed to launch that application".into(),
                    ));
                }
                if let Some(action) = &offered {
                    if !Self::allowed(
                        workspace,
                        policy,
                        now,
                        &app_id,
                        (&action.resource_kind, &action.resource, &action.action),
                        &action.contract,
                    ) {
                        return Err(ActionHostError::UnknownAction);
                    }
                    let argument = request.argument.to_slot();
                    if !confirmation_satisfied(
                        &action
                            .contract
                            .confirmation_for_argument(argument.as_deref()),
                        request.confirmation.as_ref(),
                    ) {
                        return Err(ActionHostError::confirmation_required());
                    }
                }
                Ok(workspace.clone())
            })
            .ok_or(ActionHostError::UnknownAction)??;
        Ok(LaunchDestination {
            workspace,
            app_id,
            record,
            resource,
        })
    }

    /// The ids installed apps had before, as the launcher's catalogue believes them.
    fn earlier_ids(&self) -> tilo_action_policy::EarlierIds {
        self.services
            .launcher
            .as_ref()
            .map(|launcher| launcher.earlier_ids())
            .unwrap_or_default()
    }

    /// The action the installed app `app_id`'s signed manifest offers at
    /// `address` while it is closed: what a launch is admitted against until
    /// the app publishes its own.
    /// The workspace, app and offered action an application target of
    /// `request` names (`application:<workspace>:<app>`): the app under the id
    /// it has now, and the action as one of its instances publishes it, as it
    /// is kept for launching, or as its manifest declares it. `None` for a
    /// target that names no application; no action when the app offers none
    /// at that address.
    pub(super) fn application_offer(
        &self,
        request: &InvokeRequest,
    ) -> Option<(String, String, Option<PublishedAction>)> {
        let (workspace, app_id) = application_provider(&request.target.provider)?;
        let earlier = self.earlier_ids();
        let resource = earlier.current_resource(&request.target.address.resource);
        let address = &current_address(resource.as_deref(), request);
        let app_id = earlier.current(app_id).to_owned();
        let offered = self
            .published_surfaces()
            .into_iter()
            .chain(self.launchable.surfaces())
            .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == workspace)
            .find_map(|surface| {
                surface.actions.into_iter().find(|action| {
                    action.resource == address.resource && action.action == address.action
                })
            })
            .or_else(|| self.manifest_offer(&app_id, address));
        Some((workspace.to_owned(), app_id, offered))
    }

    fn manifest_offer(&self, app_id: &str, address: &ActionAddress) -> Option<PublishedAction> {
        let applications = self.services.launcher.as_ref()?.applications();
        let app = applications
            .iter()
            .find(|app| app.package_id == app_id && !app.unconfined)?;
        app.offers
            .iter()
            .find(|offer| offer.resource == address.resource && offer.action == address.action)
            .map(|offer| offered_action(app, offer))
    }

    /// Hand `request` to the publisher `peer_id` that offers it at `address` now: the native
    /// task's own delivery, or a job on that connection.
    async fn deliver_to(
        self: &Arc<Self>,
        peer_id: u64,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        (request, address): (&InvokeRequest, &tilo_actions::ActionAddress),
        native: Option<&super::native_task_execution::NativeLaunch>,
    ) -> Result<u64, ActionHostError> {
        if let Some(native) = native {
            return native.deliver(self, peer_id).await.map_err(refusal);
        }
        let mut delivered = request.clone();
        delivered.target.provider = peer_id.to_string();
        delivered.target.address = address.clone();
        self.start_job(caller, origin, &delivered, request).await
    }

    /// The connection ready to take `address` in `destination`, and the address it takes it
    /// at: the native task's own, an instance of the app for a `start` (`starts`), else the
    /// app's ready publisher of it.
    fn ready_for(
        &self,
        destination: &LaunchDestination,
        native: Option<&super::native_task_execution::NativeLaunch>,
        starts: bool,
        address: &tilo_actions::ActionAddress,
    ) -> Option<(u64, tilo_actions::ActionAddress)> {
        let workspace = destination.workspace.id().to_string();
        match native {
            Some(native) => native.ready(self).map(|peer_id| (peer_id, address.clone())),
            None if starts => self
                .running_instance(&destination.app_id, &workspace, &address.action)
                .map(|peer_id| (peer_id, address.clone())),
            None => self.ready_delivery(
                &destination.app_id,
                &workspace,
                destination.record.as_deref(),
                address,
            ),
        }
    }

    /// Answer `request`, a workspace's `start` of its app ([`super::windowless_instances::started_app`]),
    /// with `peer_id`, the instance that runs now. The journal blocks, so it answers on the
    /// delivery runtime's blocking pool.
    async fn answer_started(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
        (workspace, app_id): (&str, &str),
        peer_id: u64,
    ) -> Result<u64, ActionHostError> {
        let state = Arc::clone(self);
        let (caller, origin, request) = (caller.clone(), origin.cloned(), request.clone());
        let reason = format!("{app_id} start worker stopped");
        let (workspace, app_id) = (workspace.to_owned(), app_id.to_owned());
        tokio::task::spawn_blocking(move || {
            state.answer_start(
                &caller,
                origin.as_ref(),
                &request,
                (&app_id, &workspace),
                peer_id,
            )
        })
        .await
        .map_err(|error| ActionHostError::Unavailable {
            reason: format!("{reason}: {error}"),
        })?
    }

    /// Launch `app_id` unless it already publishes the action, then deliver
    /// the invocation to the connection that does, through the same
    /// admission a live action takes.
    ///
    /// The wait wakes on what readiness depends on — a surface published or
    /// withdrawn, an invocation stream opened or closed, in either order —
    /// on the delivery deadline, and on the caller dropping its answer.
    pub(super) async fn relaunch_and_deliver<T: Send>(
        self: &Arc<Self>,
        completer: &Completer<T>,
        caller: &Caller,
        destination: &LaunchDestination,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
        native: Option<&super::native_task_execution::NativeLaunch>,
    ) -> Result<u64, ActionHostError> {
        let stopped_waiting = || ActionHostError::Unavailable {
            reason: "the caller stopped waiting for admission".into(),
        };
        let workspace_id = destination.workspace.id().to_string();
        let app_id = &destination.app_id;
        let address = &current_address(destination.resource.as_deref(), request);
        let timeout = self.launchable.delivery_timeout();
        let deadline = tokio::time::sleep(timeout);
        let expired = || self.not_offered(app_id, &workspace_id, address, timeout);
        let abandoned = completer.abandoned();
        tokio::pin!(deadline, abandoned);
        let shows = self.action_shows(app_id, &workspace_id, address);
        let started = super::windowless_instances::started_app(request);
        // Taken before the first look, so a publication between that look
        // and the wait below still wakes it.
        let mut changes = self.publisher_changes();
        let mut launched = false;
        // The start of the instance with no window asked for, until it ends.
        let mut starting: Option<crate::services::WindowlessStart> = None;
        loop {
            self.check_launch_origin(caller, origin, &workspace_id)?;
            if let Some(native) = native {
                native.check(self).map_err(refusal)?;
            }
            if completer.is_abandoned() {
                return Err(stopped_waiting());
            }
            if tokio::time::Instant::now() >= deadline.deadline() {
                return Err(expired());
            }
            if let Some((peer_id, published)) =
                self.ready_for(destination, native, started.is_some(), address)
            {
                if native.is_none()
                    && let Some(started) = started
                {
                    return self
                        .answer_started(caller, origin, request, started, peer_id)
                        .await;
                }
                return self
                    .deliver_to(peer_id, caller, origin, (request, &published), native)
                    .await;
            }
            // An action that shows nothing runs in an instance of the app
            // with no window, where the host runs one: it publishes, and the
            // wait below delivers to it. Only an `open` launches a window.
            if !launched
                && !shows
                && address.action != APPLICATION_OPEN_ACTION
                && let Some(start) = self.run_windowless(app_id, &workspace_id)?
            {
                starting = Some(start);
                launched = true;
                continue;
            }
            if !launched {
                let mut permit = Some(tokio::select! {
                    biased;
                    () = &mut abandoned => return Err(stopped_waiting()),
                    () = &mut deadline => return Err(expired()),
                    seen = changes.changed() => {
                        if seen.is_err() {
                            return Err(ActionHostError::Unavailable { reason: "the broker stopped".into() });
                        }
                        continue;
                    }
                    () = tilo_rt::sleep(std::time::Duration::from_millis(50)), if native.is_some() => continue,
                    permit = super::launchable::launch_permits().acquire_owned() => {
                        permit.map_err(|_| ActionHostError::Unavailable { reason: "the launch queue stopped".into() })?
                    }
                });
                let current = self.readmit_launch(caller, origin, request, destination, native)?;
                let launch = self.finish_retained_launch(
                    completer,
                    caller,
                    (origin, request),
                    current,
                    native,
                    permit.take().ok_or(ActionHostError::NotPermitted)?,
                    (deadline.deadline(), &address.action),
                );
                drop(permit);
                launch.await?;
                launched = true;
                continue;
            }
            tokio::select! {
                seen = changes.changed() => if seen.is_err() {
                    return Err(ActionHostError::Unavailable {
                        reason: "the broker stopped".into(),
                    });
                },
                () = &mut deadline => return Err(expired()),
                () = &mut abandoned => return Err(stopped_waiting()),
                () = tilo_rt::sleep(std::time::Duration::from_millis(50)), if native.is_some() => {},
                // A start that failed is the action's answer now, with why.
                failed = super::windowless_instances::start_failed(&mut starting, app_id) => {
                    return Err(failed);
                }
            }
        }
    }

    /// `app_id` was launched in `workspace` but did not offer `address` within
    /// `timeout`: why, with what its connections publish.
    fn not_offered(
        &self,
        app_id: &str,
        workspace: &str,
        address: &tilo_actions::ActionAddress,
        timeout: std::time::Duration,
    ) -> ActionHostError {
        ActionHostError::Unavailable {
            reason: format!(
                "{app_id} was launched but did not offer \"{}\" within {:.1} s ({})",
                address.action,
                timeout.as_secs_f32(),
                self.publishers_seen(app_id, workspace, address),
            ),
        }
    }

    async fn finish_retained_launch<T: Send>(
        self: &Arc<Self>,
        completer: &Completer<T>,
        caller: &Caller,
        custody: (Option<&super::InvocationCaller>, &InvokeRequest),
        current: LaunchDestination,
        native: Option<&super::native_task_execution::NativeLaunch>,
        permit: tokio::sync::OwnedSemaphorePermit,
        deadline: (tokio::time::Instant, &str),
    ) -> Result<(), ActionHostError> {
        let app_id = current.app_id.clone();
        let workspace_id = current.workspace.id().to_string();
        let timeout = self.launchable.delivery_timeout();
        let stopped_waiting = || ActionHostError::Unavailable {
            reason: "the caller stopped waiting for admission".into(),
        };
        let expired = || ActionHostError::Unavailable {
            reason: format!(
                "{app_id} was launched but did not offer \"{}\" within {:.1} s",
                deadline.1,
                timeout.as_secs_f32()
            ),
        };
        let deadline = tokio::time::sleep_until(deadline.0);
        let abandoned = completer.abandoned();
        tokio::pin!(deadline, abandoned);
        let state = Arc::clone(self);
        let place = self.launch_place(caller, &workspace_id, None);
        let caller = caller.clone();
        let origin = custody.0.cloned();
        let parent = custody.1.clone();
        let native = native.cloned();
        let mut launch = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            match native {
                // Native tasks retain different execution custody, not an
                // InvocationCaller. Keep their checked launch path; actor text
                // is not sufficient authority for a signed launch child.
                Some(native) => native.launch(&state, &current, &place).map_err(refusal),
                None => {
                    state.launch_retained_child(&caller, origin.as_ref(), &parent, &current, &place)
                }
            }
        });
        let launched_result = tokio::select! {
            biased;
            () = &mut abandoned => {
                launch.abort();
                return Err(stopped_waiting());
            }
            () = &mut deadline => {
                launch.abort();
                return Err(expired());
            }
            result = &mut launch => result,
        };
        launched_result.map_err(|error| ActionHostError::Unavailable {
            reason: format!("{app_id} launch worker stopped: {error}"),
        })??;
        Ok(())
    }

    /// Recheck the original signed custody while an asynchronous launch waits.
    /// The numeric dispatcher checks it again when capturing the actual offer.
    pub(super) fn check_launch_origin(
        &self,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        workspace: &str,
    ) -> Result<(), ActionHostError> {
        check_execution(caller)?;
        if !self.is_accepting() {
            return Err(ActionHostError::NotPermitted);
        }
        let admitted = match origin {
            Some(super::published_invocation::InvocationCaller::Certified {
                caller: original,
                ..
            }) => {
                if original.workspace != workspace {
                    return Err(ActionHostError::NotPermitted);
                }
                // A durable barrier can outlive the synchronous admission
                // memo too. Verify original custody without borrowing that
                // earlier admission, even on the same dispatch thread.
                self.checked_policy_time(original.policy_time.as_ref())
                    .map_err(refusal)?;
                let current = self
                    .verify_action_caller(&original.workspace, &original.presentation)
                    .map_err(refusal)?;
                // The barrier re-verifies the original person and device, and
                // refuses a delegated caller whose admission can no longer
                // name its owner instead of dropping the delegation.
                original
                    .reverified(&current)
                    .map_err(|_| ActionHostError::NotPermitted)?;
                original
                    .owner()
                    .map_err(|_| ActionHostError::NotPermitted)?;
                if caller.device_public_key().is_none() {
                    self.current_native_person(
                        workspace,
                        current
                            .admitted
                            .certificate
                            .as_ref()
                            .ok_or(ActionHostError::NotPermitted)?,
                    )
                    .map_err(refusal)?;
                }
                Some(current.admitted.delegated_to(original.delegated.as_deref()))
            }
            Some(super::published_invocation::InvocationCaller::Local {
                route,
                admitted,
                time,
            }) => {
                self.checked_policy_time(time.as_ref()).map_err(refusal)?;
                if route.cluster_id != workspace {
                    return Err(ActionHostError::NotPermitted);
                }
                // Revalidation checks current certificate and workspace policy, but
                // carries the registration forward without checking its liveness.
                // Local custody must still name the exact private connection.
                if let Some(registration) = &admitted.worker_registration {
                    let workers = self
                        .host_workers
                        .lock()
                        .map_err(|_| refusal("worker registry unavailable".into()))?;
                    if !workers
                        .values()
                        .any(|worker| worker.worker_registration.as_ref() == Some(registration))
                    {
                        return Err(ActionHostError::NotPermitted);
                    }
                }
                let current = self
                    .revalidate_admission(route, admitted)
                    .map_err(|error| refusal(error.to_string()))?;
                self.checked_policy_time(time.as_ref()).map_err(refusal)?;
                Some(current)
            }
            Some(super::published_invocation::InvocationCaller::NativePerson {
                workspace: original_workspace,
                certificate,
                time,
                ..
            }) => {
                self.checked_policy_time(Some(time)).map_err(refusal)?;
                if original_workspace != workspace
                    || caller.principal()
                        != &tilo_actions::remote::CallerPrincipal::Person(
                            super::principal_id_for_root(certificate.user_public_key),
                        )
                {
                    return Err(ActionHostError::NotPermitted);
                }
                self.current_native_person(workspace, certificate)
                    .map_err(refusal)?;
                None
            }
            None => None,
        };
        if let Some(admitted) = admitted {
            let expected = admitted.broker_actor();
            let actual = BrokerActor::for_caller(caller);
            // The connection's app may act under its own identity, by what
            // the workspace grants it.
            let own_app = match origin {
                Some(super::published_invocation::InvocationCaller::Local { route, .. }) => {
                    actual.is_agent && actual.id == BrokerActor::app(&route.app_id).id
                }
                _ => false,
            };
            if !own_app && (expected.id != actual.id || expected.is_agent != actual.is_agent) {
                return Err(ActionHostError::NotPermitted);
            }
        }
        Ok(())
    }

    /// Durable admission can block: recheck original custody and sample the
    /// committed workspace and clock again immediately before the host effect.
    fn reauthorize_launch_effect(
        &self,
        custody: (&Caller, Option<&super::InvocationCaller>, &str),
        admitted: &super::host_action_admission::AdmittedHostAction,
        app_id: &str,
    ) -> Result<Workspace, ActionHostError> {
        self.reauthorize_launch_effect_as(
            custody,
            admitted,
            (app_id, tilo_action_policy::ActionKind::Undoable),
        )
    }

    /// [`Self::reauthorize_launch_effect`] for a launch that needs `kind`
    /// on the app ([`launch_kind`]).
    fn reauthorize_launch_effect_as(
        &self,
        custody: (&Caller, Option<&super::InvocationCaller>, &str),
        admitted: &super::host_action_admission::AdmittedHostAction,
        (app_id, kind): (&str, tilo_action_policy::ActionKind),
    ) -> Result<Workspace, ActionHostError> {
        let (caller, origin, workspace) = custody;
        let result = self
            .current_framework_authority(caller, origin, workspace)
            .and_then(|(workspace, policy, now)| {
                if policy.may_in(&workspace, &app_node(&workspace, app_id), kind, now) {
                    return Ok(workspace);
                }
                tracing::warn!(
                    %app_id,
                    caller = %policy.id,
                    owner = workspace.owner().map_or("none", |owner| owner.as_str()),
                    "an app's launch was refused: the workspace does not let the caller open it"
                );
                Err(ActionHostError::NotPermitted)
            });
        if result.is_err() {
            self.complete_host_action(
                admitted,
                tilo_actions::InvocationUpdate::Failed {
                    invocation: admitted.invocation,
                    error: tilo_actions::ActionFailure {
                        code: "launch_authority_changed".into(),
                        message: "application authority is no longer available".into(),
                        details: None,
                        retryable: false,
                    },
                },
            )?;
        }
        result
    }

    /// Start `app_id` in `workspace` on `record` at `place` through the
    /// host's launcher, or the one a test installed in its place.
    pub(super) fn launch_retained(
        &self,
        app_id: &str,
        workspace: &Workspace,
        record: Option<&str>,
        place: &LaunchPlace,
    ) -> Result<(), String> {
        if let Some(launcher) = self.launchable.launcher() {
            return launcher(app_id, workspace, record, place);
        }
        self.services
            .launcher
            .as_ref()
            .ok_or_else(|| "application launch unsupported".to_string())?
            .launch(app_id, workspace, record, place)
    }

    /// Where a view `caller`'s action needs opens in `workspace`: where a person
    /// is looking, for the person's own invocation and for an app's request
    /// the person's own press in it is behind (a link clicked in Chat, a
    /// file opened from Files), which takes them there; beside the asking
    /// app's own view for an app's request nobody pressed for; else beside
    /// the view the actor last worked in, so an agent or a Shortcut keeps
    /// working next to where it already is, and beside the person's view
    /// without taking their focus when it has none yet: nobody but the
    /// person moves the person's view.
    pub(super) fn launch_place(
        &self,
        caller: &Caller,
        workspace: &str,
        launch: Option<&LaunchScope>,
    ) -> LaunchPlace {
        let pressed_for = |app_id: &str| {
            self.services
                .launcher
                .as_ref()
                .is_some_and(|launcher| launcher.person_initiated(app_id))
        };
        if let Some(scope) = launch {
            if pressed_for(&scope.app_id) {
                return LaunchPlace::Focus;
            }
            return LaunchPlace::Beside {
                app_id: scope.app_id.clone(),
            };
        }
        match caller.principal() {
            CallerPrincipal::Person(_) => LaunchPlace::Focus,
            CallerPrincipal::App(app_id) if pressed_for(app_id) => LaunchPlace::Focus,
            CallerPrincipal::App(app_id) => LaunchPlace::Beside {
                app_id: app_id.clone(),
            },
            CallerPrincipal::Agent(_) | CallerPrincipal::Bot(_) => self
                .launchable
                .view_of(workspace, &BrokerActor::for_caller(caller).id)
                .map_or(LaunchPlace::Aside, |app_id| LaunchPlace::Beside { app_id }),
        }
    }

    /// The live connection of `app_id` in `workspace` that publishes `address`
    /// and can receive invocations.
    pub(super) fn ready_publisher(
        &self,
        app_id: &str,
        workspace: &str,
        record: Option<&str>,
        address: &tilo_actions::ActionAddress,
    ) -> Option<u64> {
        self.publisher_of(app_id, workspace, record, address, false)
            .map(|(peer_id, _)| peer_id)
    }

    /// [`Self::ready_publisher`] and the address that connection publishes
    /// `address` under, which is what it is delivered as.
    ///
    /// An app names its resources by its own ids (`tasks.tracker`); a
    /// component surface publishes each under its instance's alias
    /// ([`tilo_actions::BOUND_RESOURCE_PREFIX`]), which only that instance
    /// resolves. A target written with the app's id (an application target,
    /// a run's report) reaches the instance publishing it under either name,
    /// and is handed over under the one that instance published.
    pub(super) fn ready_delivery(
        &self,
        app_id: &str,
        workspace: &str,
        record: Option<&str>,
        address: &tilo_actions::ActionAddress,
    ) -> Option<(u64, tilo_actions::ActionAddress)> {
        self.publisher_of(app_id, workspace, record, address, true)
    }

    /// What `app_id`'s connections in `workspace` publish of `address`'s action
    /// and why none is ready for it: said with a delivery that found none.
    fn publishers_seen(
        &self,
        app_id: &str,
        workspace: &str,
        address: &tilo_actions::ActionAddress,
    ) -> String {
        let Ok(subscribers) = self.invocation_subscribers.lock() else {
            return "the invocation streams are unavailable".into();
        };
        let Ok(surfaces) = self.surfaces.lock() else {
            return "the published surfaces are unavailable".into();
        };
        let seen: Vec<String> = surfaces
            .values()
            .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == workspace)
            .map(|surface| {
                let resources: Vec<String> = surface
                    .actions
                    .iter()
                    .filter(|action| action.action == address.action)
                    .map(|action| {
                        tilo_actions::selection::logical_resource_id(&action.resource).into_owned()
                    })
                    .collect();
                format!(
                    "connection {} on {}: {}{}{}publishes \"{}\" on [{}]",
                    surface.peer_id,
                    surface.route.record_id,
                    if surface.live { "" } else { "closed, " },
                    if subscribers.contains_key(&surface.peer_id) {
                        ""
                    } else {
                        "takes no invocations, "
                    },
                    if self.shows(surface.peer_id, &address.action) {
                        ""
                    } else {
                        "has no window for it, "
                    },
                    address.action,
                    resources.join(", ")
                )
            })
            .collect();
        if seen.is_empty() {
            format!("no connection of {app_id} published in this workspace")
        } else {
            seen.join("; ")
        }
    }

    /// The ready publisher of `address` and the address it publishes it
    /// under: the exact one, else (when `aliases`) one whose resource is a
    /// bound alias of `address`'s resource, or the one alias of the same
    /// resource now when `address` names an alias an instance that ended
    /// published ([`super::PublishedSurface::current_resource`]).
    fn publisher_of(
        &self,
        app_id: &str,
        workspace: &str,
        record: Option<&str>,
        address: &tilo_actions::ActionAddress,
        aliases: bool,
    ) -> Option<(u64, tilo_actions::ActionAddress)> {
        // Asked before the tables are held: it reads them itself. An action
        // that shows something needs a window to show it in.
        let shows = self.action_shows(app_id, workspace, address);
        let subscribers = self.invocation_subscribers.lock().ok()?;
        let surfaces = self.surfaces.lock().ok()?;
        surfaces
            .values()
            .filter(|surface| {
                surface.live
                    && self.shows(surface.peer_id, &address.action)
                    && !(shows && self.is_windowless(surface.peer_id))
                    && surface.app_id == app_id
                    && surface.route.cluster_id == workspace
                    && record.is_none_or(|record| surface.route.record_id == record)
                    && subscribers.contains_key(&surface.peer_id)
            })
            .filter_map(|surface| {
                let resource = if aliases {
                    surface
                        .current_resource(&address.resource, &address.action)
                        .or_else(|| {
                            surface
                                .actions
                                .iter()
                                .find(|action| {
                                    action.action == address.action
                                        && tilo_actions::selection::logical_resource_id(
                                            &action.resource,
                                        ) == address.resource
                                })
                                .map(|action| action.resource.as_str())
                        })?
                } else {
                    surface
                        .actions
                        .iter()
                        .find(|action| {
                            action.action == address.action && action.resource == address.resource
                        })?
                        .resource
                        .as_str()
                };
                Some((
                    surface.peer_id,
                    tilo_actions::ActionAddress::new(resource, &address.action),
                ))
            })
            .min_by_key(|(peer_id, _)| *peer_id)
    }

    pub(super) fn invoke_eager(
        self: &Arc<Self>,
        caller: &Caller,
        own_peer: Option<u64>,
        launch: Option<&LaunchScope>,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Some(forward) = self.eager_forward(caller, origin, request) {
            return self.send_to_host(caller, forward);
        }
        let reservation = self.reserve_invocation(caller, request);
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(Reservation::Admitted) => {
                return self.admitted_receipt(caller, request.invocation);
            }
            Ok(Reservation::Reserved) => {}
        }
        // Signing and durable admission can block. Keep the UUID reserved, but
        // never hold the UI's lookup/enqueue mutex across those operations.
        let started = self.start_for(caller, own_peer, launch, origin, request, request);
        self.finish_started(request.invocation, started)
    }

    /// Start `delivered` for `caller`: the job, once its admission is
    /// durable and delivered.
    async fn start_job(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        delivered: &InvokeRequest,
        request: &InvokeRequest,
    ) -> Result<u64, ActionHostError> {
        self.start_for(caller, None, None, origin, delivered, request)?
            .job(self)
            .await
    }

    /// The receipt for `invocation`, given once what `started` admitted is
    /// durable and delivered.
    fn finish_started(
        self: &Arc<Self>,
        invocation: tilo_actions::InvocationId,
        started: Result<Started, ActionHostError>,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        match started {
            Err(error) => Pending::ready(self.finish_directory_admission(invocation, Err(error))),
            Ok(Started::Now(job)) => {
                Pending::ready(self.finish_directory_admission(invocation, Ok(job)))
            }
            Ok(Started::Staged(staged)) => {
                // A retry finds the job at once, and its answer waits for
                // the same write (`admitted_receipt`).
                if let Ok(mut ledger) = self.remote_invocations.lock() {
                    ledger.complete(&invocation, staged.id());
                }
                let (receipt, answered) = pending();
                let state = Arc::clone(self);
                staged.start_then(self, move |started| {
                    let started = started.map(|job| job.id).map_err(refusal);
                    receipt.complete(state.finish_directory_admission(invocation, started));
                });
                answered
            }
        }
    }

    /// The receipt for `invocation`, admitted for `caller` already, once its
    /// admission is durable and delivered: a retry is told no sooner than
    /// the first asker.
    fn admitted_receipt(
        self: &Arc<Self>,
        caller: &Caller,
        invocation: tilo_actions::InvocationId,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let Some(job) = self.remote_job(caller, &invocation) else {
            return Pending::ready(Err(ActionHostError::UnknownInvocation));
        };
        let (receipt, answered) = pending();
        let state = Arc::clone(self);
        self.journal_in_flight(&invocation).then(move |outcome| {
            receipt.complete(
                outcome
                    .map(|()| state.receipt_for(invocation, job))
                    .map_err(|reason| ActionHostError::Unavailable { reason }),
            );
        });
        answered
    }

    pub(super) fn receipt_for(
        &self,
        invocation: tilo_actions::InvocationId,
        job: u64,
    ) -> InvocationReceipt {
        self.invocation_status(job).map_or(
            InvocationReceipt {
                invocation,
                status: ReceiptStatus::Accepted,
            },
            |update| InvocationState::new(invocation, update).receipt(),
        )
    }

    fn finish_directory_admission(
        &self,
        invocation: tilo_actions::InvocationId,
        started: Result<u64, ActionHostError>,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let mut ledger =
            self.remote_invocations
                .lock()
                .map_err(|_| ActionHostError::Unavailable {
                    reason: "the invocation ledger is unavailable".into(),
                })?;
        match started {
            Ok(job) => {
                ledger.complete(&invocation, job);
                drop(ledger);
                Ok(self.receipt_for(invocation, job))
            }
            Err(error) => {
                ledger.abandon(&invocation);
                drop(ledger);
                Err(error)
            }
        }
    }

    fn start_for(
        self: &Arc<Self>,
        caller: &Caller,
        own_peer: Option<u64>,
        launch: Option<&LaunchScope>,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
        original_request: &InvokeRequest,
    ) -> Result<Started, ActionHostError> {
        let address = &request.target.address;
        let argument = request.argument.to_slot();
        let provider = request.target.provider.as_str();
        if provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER {
            return self.start_governance_action(
                caller,
                origin,
                original_request,
                request,
                argument.as_deref(),
            );
        }
        if let Some(workspace) = provider_workspace(provider) {
            return self
                .start_workspace_action(
                    caller,
                    workspace,
                    launch,
                    origin,
                    request,
                    argument.as_deref(),
                )
                .map(Started::Now);
        }
        let peer_id = if provider == OWN_PROVIDER {
            own_peer.ok_or(ActionHostError::UnknownAction)?
        } else {
            provider
                .parse::<u64>()
                .map_err(|_| ActionHostError::UnknownAction)?
        };
        // Only the one action this peer offers is copied out, not every
        // app's published surface.
        let (app_id, workspace, action) = self
            .surfaces
            .lock()
            .ok()
            .and_then(|surfaces| {
                let surface = surfaces.get(&peer_id).filter(|surface| surface.live)?;
                let action = surface.actions.iter().find(|action| {
                    action.resource == address.resource && action.action == address.action
                })?;
                Some((
                    surface.app_id.clone(),
                    surface.route.cluster_id.clone(),
                    action.clone(),
                ))
            })
            .ok_or(ActionHostError::UnknownAction)?;
        // Offered when the caller may take it or would be asked first;
        // admission requires the confirmation an ask needs.
        let (offered, workspace) = self
            .decide_for(caller, &workspace, |workspace, policy, now| {
                Self::offered(
                    workspace,
                    policy,
                    now,
                    &app_id,
                    (&action.resource_kind, &action.resource, &action.action),
                    &action.contract,
                )
            })
            .and_then(|allowed| allowed.then_some((action, workspace)))
            .ok_or(ActionHostError::UnknownAction)?;
        let actor = self.caller_actor(caller, &workspace);
        let token = self.confirmation_token(
            peer_id,
            &app_id,
            &offered,
            argument.as_deref(),
            &actor,
            request.confirmation.as_ref(),
            &self.witness_binding(actor.id(), &workspace, original_request),
        )?;
        self.start_invocation_confirmed_for_origin(
            peer_id,
            &address.resource,
            &address.action,
            InvocationInputs {
                original_request: Some(original_request),
                caller: Some(caller),
                requested: Some(request.invocation),
                execution: super::ExecutionScope::for_call(caller, request.invocation),
                argument: argument.as_deref(),
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: request.context.clone(),
            },
            origin.cloned(),
        )
        .map(|staged| {
            self.launchable.note_view(&workspace, &actor.id, &app_id);
            Started::Staged(staged)
        })
        .map_err(refusal)
    }

    fn start_workspace_action(
        self: &Arc<Self>,
        caller: &Caller,
        workspace_id: &str,
        launch: Option<&LaunchScope>,
        origin: Option<&super::InvocationCaller>,
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let address = &request.target.address;
        // Workspace actions hand the workspace to host services (launcher,
        // shortcut library), which cannot run under the borrow: copy only
        // this one workspace's authority.
        let Some((workspace, policy, now)) =
            self.decide_for(caller, workspace_id, |workspace, policy, now| {
                (workspace.clone(), policy.clone(), now)
            })
        else {
            return Err(ActionHostError::UnknownAction);
        };
        if let Some(result) = self.start_remaining_workspace_action(
            caller,
            origin,
            (&workspace, &policy, now),
            request,
            argument,
        ) {
            return result;
        }
        if let Some(result) = self.start_app_open(
            caller,
            workspace_id,
            launch,
            origin,
            request,
            argument,
            (workspace.clone(), policy.clone(), now),
        )? {
            return Ok(result);
        }
        if let Some(app_id) = address.resource.strip_prefix(APPLICATION_RESOURCE_PREFIX)
            && address.action == APPLICATION_CLOSE_ACTION
        {
            if caller.workspace().is_none() {
                return Err(ActionHostError::UnknownAction);
            }
            if !policy.may_in(
                &workspace,
                &app_node(&workspace, app_id),
                tilo_action_policy::ActionKind::Undoable,
                now,
            ) {
                return Err(ActionHostError::NotPermitted);
            }
            if !self
                .services
                .launcher
                .as_ref()
                .is_some_and(|launcher| launcher.closes_applications())
            {
                return Err(refusal(
                    "closing applications is unsupported on this host".into(),
                ));
            }
            let admitted = self.admit_host_action(
                caller,
                origin,
                super::host_action_admission::HostActionAdmission {
                    workspace: workspace_id,
                    app_id,
                    descriptor: &app_close_descriptor(app_id, app_id),
                    request,
                },
            )?;
            return self.close_application((caller, origin, workspace_id), admitted, app_id);
        }
        Err(ActionHostError::UnknownAction)
    }

    /// Admit `descriptor`, published by `app`, for `policy` in `workspace`
    /// at `now`: allowed; or, where the workspace's grants ask first, with the
    /// confirmation of the person — their own word when they act
    /// themselves, else the answer they gave on their device. Admitted, it
    /// spends the `Once` grants it relied on.
    pub(super) fn admit_answer(
        &self,
        (workspace, policy, now): (&Workspace, &BrokerActor, u64),
        app: &str,
        descriptor: &ActionDescriptor,
        (_argument, confirmation): (Option<&str>, Option<&Confirmation>),
        witnessed: bool,
    ) -> Result<(), ActionHostError> {
        let (answer, spent) = policy.action_verdict_in(
            workspace,
            app,
            (
                &descriptor.resource_kind,
                &descriptor.address.resource,
                &descriptor.address.action,
            ),
            &descriptor.contract.need(),
            now,
        );
        let confirmed = match answer {
            tilo_action_policy::Answer::Allow => true,
            tilo_action_policy::Answer::Deny => {
                tracing::warn!(
                    app,
                    caller = %policy.id,
                    owner = workspace.owner().map_or("none", |owner| owner.as_str()),
                    action = %descriptor.address.action,
                    "an action was refused: the workspace does not grant it to the caller"
                );
                return Err(ActionHostError::NotPermitted);
            }
            tilo_action_policy::Answer::Ask => match confirmation {
                Some(Confirmation::Witnessed(_)) => witnessed,
                Some(Confirmation::Confirmed) => policy.direct(),
                _ => false,
            },
        };
        if !confirmed {
            return Err(ActionHostError::confirmation_required());
        }
        self.spend_once_grants(&workspace.id().to_string(), &spent);
        Ok(())
    }

    /// Whether `confirmation` answers `descriptor`'s own confirmation
    /// policy for `actor` with `argument`: the person's own word when they
    /// act themselves, else the answer they gave on their device.
    pub(super) fn confirmed_as_asked(
        actor: &BrokerActor,
        descriptor: &ActionDescriptor,
        argument: Option<&str>,
        confirmation: Option<&Confirmation>,
        witnessed: bool,
    ) -> bool {
        let policy = descriptor.contract.confirmation_for_argument(argument);
        if policy == tilo_actions::ConfirmationPolicy::None {
            return true;
        }
        let own = actor.direct().then_some(confirmation).flatten();
        witnessed || confirmation_satisfied(&policy, own)
    }

    /// Agent runs of workspace tasks that ended since the last call, oldest
    /// first.
    pub fn take_ended_task_runs(&self) -> Vec<crate::task_runs::EndedTaskRun> {
        self.ended_runs.tasks.take()
    }

    /// Whether an ended task run is waiting to be taken.
    pub fn has_ended_task_runs(&self) -> bool {
        self.ended_runs.tasks.has_ended()
    }

    /// Launch `app_id` into `workspace`, optionally opening content-addressed
    /// bytes read-only, and record the terminal outcome as an invocation.
    pub(super) fn launch_application(
        &self,
        custody: (&Caller, Option<&super::InvocationCaller>, &str),
        admitted: super::host_action_admission::AdmittedHostAction,
        app_id: &str,
        content: crate::services::LaunchContent<'_>,
        place: &LaunchPlace,
    ) -> Result<u64, ActionHostError> {
        // Content is only ever read through the asking application's own
        // scope; a caller without one cannot open it by hash.
        if !content.hash.is_empty() && content.caller.is_none() {
            return Err(refusal(
                "launch content requires an application the launcher authenticated".into(),
            ));
        }
        let launcher = self
            .services
            .launcher
            .as_ref()
            .ok_or_else(|| refusal("application launch unsupported".into()))?;
        let id = admitted.id;
        let invocation = admitted.invocation;
        let workspace = self.reauthorize_launch_effect(custody, &admitted, app_id)?;
        match launcher.launch_content(app_id, &workspace, content, place) {
            // Answered with the instance it started, once that instance
            // connects with the launch's ticket and publishes.
            Ok(started) => {
                self.record_launch(admitted, (app_id, &workspace.id().to_string()), started)?;
            }
            Err(error) => self.complete_host_action(
                &admitted,
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "launch_failed".into(),
                        message: error,
                        details: None,
                        retryable: false,
                    },
                },
            )?,
        }
        Ok(id)
    }

    /// Close `app_id`'s windows in `workspace` and record the terminal
    /// outcome as an invocation.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the dispatch permit is spent here: taken by value, a caller cannot complete it twice"
    )]
    pub(super) fn close_application(
        &self,
        custody: (&Caller, Option<&super::InvocationCaller>, &str),
        admitted: super::host_action_admission::AdmittedHostAction,
        app_id: &str,
    ) -> Result<u64, ActionHostError> {
        let launcher = self
            .services
            .launcher
            .as_ref()
            .filter(|launcher| launcher.closes_applications())
            .ok_or_else(|| refusal("closing applications is unsupported on this host".into()))?;
        let id = admitted.id;
        let invocation = admitted.invocation;
        let workspace = self.reauthorize_launch_effect(custody, &admitted, app_id)?;
        let update = match launcher.close(app_id, &workspace) {
            Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            Err(error) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "close_failed".into(),
                    message: error,
                    details: None,
                    retryable: false,
                },
            },
        };
        self.complete_host_action(&admitted, update)?;
        Ok(id)
    }

    /// The request as the idempotency ledger identifies it. The confirmation
    /// it carries is evidence an admission already used, not part of the
    /// invocation: a retry carrying another grant — or none, as a caller
    /// that already saw the answer sends — is the same invocation and gets
    /// its receipt, never a conflict.
    pub(crate) fn invocation_identity(request: &InvokeRequest) -> InvokeRequest {
        InvokeRequest {
            confirmation: None,
            ..request.clone()
        }
    }

    /// Reserves `request`'s invocation in the idempotency ledger, keyed by
    /// [`Self::invocation_identity`].
    pub(crate) fn reserve_invocation(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Reservation, ActionHostError> {
        self.remote_invocations
            .lock()
            .map_err(|_| ActionHostError::Unavailable {
                reason: "the invocation ledger is unavailable".into(),
            })?
            .reserve(caller, &Self::invocation_identity(request))
    }

    #[must_use]
    pub fn remote_job(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<u64> {
        check_execution(caller).ok()?;
        self.remote_invocations
            .lock()
            .ok()
            .and_then(|ledger| ledger.get(caller, id).copied())
    }

    /// The latest state of an invocation `caller` admitted.
    pub fn action_observe(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let job = self.remote_job(caller, id)?;
        self.invocation_status(job)
            .map(|update| InvocationState::new(*id, update).with_untrusted(self.untrusted_job(job)))
    }

    /// Resolves once an invocation changes after what `seen` has seen, or
    /// after [`CHANGE_SAFETY_INTERVAL`] without one — the
    /// [`ActionHost::next_change`](tilo_actions::remote::ActionHost::next_change)
    /// of the broker's hosts. `None` for an invocation `caller` did not
    /// admit.
    pub(super) fn action_next_change(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
        mut seen: tokio::sync::watch::Receiver<u64>,
    ) -> Option<Pending<()>> {
        self.remote_job(caller, id)?;
        let (changed, change) = pending();
        let wait = async move {
            tokio::select! {
                _ = seen.changed() => {}
                () = tilo_rt::sleep(CHANGE_SAFETY_INTERVAL) => {}
                // A waiter that found its answer some other way (it observed
                // the invocation finished) holds no task for the interval.
                () = changed.abandoned() => return,
            }
            changed.complete(());
        };
        // A waiter on a thread of its own (the control door's connections) waits on the app
        // runtime: without one it would hear no change and ask again at once.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn(wait));
        } else {
            drop(tilo_rt::try_app().ok()?.spawn(wait));
        }
        Some(change)
    }

    /// `Self::action_next_change` counting only changes after this call:
    /// the `next_change` of a host whose waiter asks before it observes, so a
    /// change between the two still wakes it. Authorized exactly
    /// as [`Self::action_observe`]: `None` for an invocation `caller` did not
    /// admit.
    pub fn action_next_change_after_now(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<Pending<()>> {
        self.action_next_change(caller, id, self.invocation_changes())
    }

    /// Ask an invocation `caller` admitted to stop, answered once the
    /// request is durable.
    ///
    /// The answer is an error when `caller` admitted no such invocation or it
    /// cannot be cancelled.
    pub fn action_cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionHostError>> {
        let Some(job) = self.remote_job(caller, id) else {
            return Pending::ready(Err(ActionHostError::UnknownInvocation));
        };
        if let Some(cancelled) = self.live_views.cancel(job, None) {
            return Pending::ready(
                cancelled.map_err(|reason| ActionHostError::NotCancellable { reason }),
            );
        }
        answered_when_durable(
            self.request_cancellation(job, &BrokerActor::for_caller(caller).id)
                .map_err(|reason| ActionHostError::NotCancellable { reason }),
        )
    }
}

/// The answer to a request `staged` in the journal: given once its write
/// resolves, and never sooner.
pub(super) fn answered_when_durable(
    staged: Result<super::invocation_journal::Durable, ActionHostError>,
) -> Pending<Result<(), ActionHostError>> {
    let durable = match staged {
        Ok(durable) => durable,
        Err(error) => return Pending::ready(Err(error)),
    };
    let (answer, answered) = tilo_actions::remote::pending();
    durable.then(move |outcome| {
        answer.complete(outcome.map_err(|reason| ActionHostError::Unavailable { reason }));
    });
    answered
}
#[cfg(test)]
mod launch_content_tests {
    use super::launch_content_hash;

    #[test]
    fn an_empty_typed_argument_map_opens_without_content() {
        let empty = format!("{}{{}}", tilo_actions::ARGUMENTS_JSON_PREFIX);
        assert_eq!(launch_content_hash(Some(&empty)), "");
        assert_eq!(launch_content_hash(None), "");
        assert_eq!(launch_content_hash(Some("sha256:abc")), "sha256:abc");
        let nonempty = format!(
            "{}{{\"content\":\"x\"}}",
            tilo_actions::ARGUMENTS_JSON_PREFIX
        );
        assert_eq!(launch_content_hash(Some(&nonempty)), nonempty);
    }
}

#[cfg(test)]
mod foreign_caller_tests {
    use super::super::{AppBrokerState, BrokerChannels, FanoutState, PeerPublisher};
    use crate::test_support::{Workspace, WorkspaceSlot, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_action_policy::roles;
    use tilo_actions::remote::Caller;

    #[test]
    fn an_admitted_bot_sees_only_its_confined_workspace() {
        let user = [27; 32];
        let mut tree = WorkspaceTree::default();
        for id in [7, 8] {
            let mut workspace = Workspace::new(WorkspaceSlot(id), "Bot workspace");
            workspace.set_owner(hex::encode(user));
            tree.workspaces.push(workspace);
        }
        let workspace_id = tree.workspaces[0].id().to_string();
        let (state, _bridge_rx) = AppBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared()),
            None,
        );
        let bot = Caller::bot("bridge", user, [9; 32]);
        assert!(state.workspace_ids_for(&bot).is_empty());
        assert_eq!(
            state.workspace_ids_for(&bot.clone().in_workspace(&workspace_id)),
            vec![workspace_id]
        );
        assert!(
            state
                .workspace_ids_for(&bot.in_workspace("unknown-workspace"))
                .is_empty()
        );
    }

    /// Every workspace is governed by one device.
    struct GovernedBy([u8; 32]);

    impl crate::certificate_authority::CertificateAuthority for GovernedBy {
        fn authorize(
            &self,
            _workspace: uuid::Uuid,
            _certificate: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            Ok(())
        }
        fn governs(&self, _workspace: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
            device == Some(&self.0)
        }
    }

    #[test]
    fn a_foreign_caller_is_capped_only_in_workspaces_its_device_does_not_govern() {
        let user = [26; 32];
        let mut workspace = Workspace::new(WorkspaceSlot(6), "Callers");
        workspace.set_owner(hex::encode(user));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = AppBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared()),
            None,
        );
        let governor = [7; 32];
        let authority: Arc<dyn crate::certificate_authority::CertificateAuthority> =
            Arc::new(GovernedBy(governor));
        assert!(state.certificate_authority.set(authority).is_ok());
        let may_administer = |caller: &Caller| {
            let (workspace, policy, now) = state.workspaces_for(caller).pop().unwrap();
            policy.may_in(
                &workspace,
                &roles::governance_action(&workspace.policy.workspace, roles::governance::SETTINGS),
                roles::GOVERNANCE_KIND,
                now,
            )
        };
        let caller =
            |device, foreign| Caller::authenticated(user, device).from_foreign_device(foreign);

        assert!(may_administer(&caller(governor, true)));
        assert!(!may_administer(&caller([8; 32], true)));
        assert!(may_administer(&caller([8; 32], false)));
    }
}

#[cfg(test)]
mod app_close_tests {
    use super::super::{AppBrokerState, PublishedSurface};
    use super::RowScope;
    use crate::route::CrdtRoute;
    use crate::test_support::{Workspace, WorkspaceSlot, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::Caller;

    #[derive(Default)]
    struct Launcher {
        closes: bool,
        closed: Mutex<Vec<(String, String)>>,
        /// How often the installed-application list was read.
        listed: std::sync::atomic::AtomicUsize,
    }

    impl crate::services::LauncherServices for Launcher {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            self.listed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            vec![crate::services::LaunchableApplication {
                capabilities: Vec::new(),
                package_id: "so.tilo.word-processor".into(),
                display_name: "Writer".into(),
                unconfined: false,
                offers: Vec::new(),
            }]
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &crate::services::LaunchPlace,
        ) -> Result<(), String> {
            Ok(())
        }
        fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
            false
        }
        fn closes_applications(&self) -> bool {
            self.closes
        }
        fn close(
            &self,
            app: &str,
            workspace: &tilo_desktop_model::Workspace,
        ) -> Result<(), String> {
            self.closed
                .lock()
                .unwrap()
                .push((app.to_string(), workspace.id().to_string()));
            Ok(())
        }
    }

    fn broker(launcher: Arc<Launcher>, live: bool) -> (Arc<AppBrokerState>, Caller, String) {
        let user = [31; 32];
        let mut workspace = Workspace::new(WorkspaceSlot(8), "Close");
        workspace.set_owner(hex::encode(user));
        let workspace_id = workspace.id().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            crate::services::HostServices {
                launcher: Some(launcher),
                ..Default::default()
            },
        );
        if live {
            state.replace_published_surfaces_for_test(vec![PublishedSurface {
                peer_id: 4,
                app_id: "so.tilo.word-processor".into(),
                route: CrdtRoute::record("so.tilo.word-processor", &workspace_id, "doc"),
                actions: Vec::new(),
                live: true,
            }]);
        }
        let caller = Caller::local_person(hex::encode(user)).in_workspace(workspace_id.clone());
        (state, caller, workspace_id)
    }

    fn app_actions(state: &AppBrokerState, caller: &Caller) -> Vec<String> {
        state
            .eager_catalogue(caller, RowScope::All)
            .0
            .into_iter()
            .filter_map(|row| row.action)
            .filter(|action| action.address.resource == "app.so.tilo.word-processor")
            .map(|action| action.address.action)
            .collect()
    }

    fn close_request(workspace: &str) -> tilo_actions::remote::InvokeRequest {
        tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::workspace_provider(workspace),
                address: tilo_actions::ActionAddress::new("app.so.tilo.word-processor", "close"),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        }
    }

    fn invoke_close(
        state: &Arc<AppBrokerState>,
        caller: &Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<u64, tilo_actions::remote::ActionHostError> {
        state
            .action_invoke(caller, None, None, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("close admission answers immediately")?;
        Ok(state.remote_job(caller, &request.invocation).unwrap())
    }

    #[test]
    fn a_running_app_offers_close_where_the_host_can_close_it() {
        let launcher = Arc::new(Launcher {
            closes: true,
            ..Default::default()
        });
        let (state, caller, _) = broker(Arc::clone(&launcher), false);
        assert_eq!(app_actions(&state, &caller), ["open"]);
        let (state, caller, workspace) = broker(Arc::clone(&launcher), true);
        assert_eq!(app_actions(&state, &caller), ["close"]);

        let request = close_request(&workspace);
        let job = invoke_close(&state, &caller, &request).unwrap();
        assert!(matches!(
            state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert_eq!(
            *launcher.closed.lock().unwrap(),
            [("so.tilo.word-processor".to_string(), workspace)]
        );

        let unsupported = Arc::new(Launcher::default());
        let (state, caller, workspace) = broker(unsupported, true);
        assert!(app_actions(&state, &caller).is_empty());
        assert!(invoke_close(&state, &caller, &close_request(&workspace)).is_err());
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its fixture and guards alive through its assertions"
    )]
    #[test]
    fn close_preserves_the_requested_uuid_in_its_terminal_journal_record() {
        let launcher = Arc::new(Launcher {
            closes: true,
            ..Default::default()
        });
        let (state, caller, workspace) = broker(Arc::clone(&launcher), true);
        let scratch = tempfile::tempdir().unwrap();
        *state.journal.lock().unwrap() = Ok(Some(
            super::super::invocation_journal::InvocationJournal::open(
                &scratch.path().join("journal"),
            )
            .unwrap(),
        ));
        let request = close_request(&workspace);
        let job = invoke_close(&state, &caller, &request).unwrap();
        let terminal = state.invocation_status(job).unwrap();
        assert_eq!(terminal.invocation(), request.invocation);
        assert!(matches!(
            terminal,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));
        let journal = state.journal.lock().unwrap();
        let journal = journal.as_ref().unwrap().as_ref().unwrap();
        let key = super::super::InvocationKey {
            workspace,
            host: journal.host(),
            owner: super::super::InvocationOwner::Local {
                actor: hex::encode([31; 32]),
            },
            invocation: request.invocation,
        };
        assert_eq!(journal.lookup(&key).unwrap().unwrap().update, terminal);
        assert_eq!(launcher.closed.lock().unwrap().len(), 1);
    }

    #[test]
    fn close_requires_original_custody_before_calling_the_launcher() {
        let launcher = Arc::new(Launcher {
            closes: true,
            ..Default::default()
        });
        let (state, _, workspace) = broker(Arc::clone(&launcher), true);
        let caller = Caller::authenticated([31; 32], [32; 32]).in_workspace(&workspace);
        assert!(invoke_close(&state, &caller, &close_request(&workspace)).is_err());
        assert!(launcher.closed.lock().unwrap().is_empty());
        assert!(state.completed_invocations.lock().unwrap().is_empty());
    }

    #[test]
    fn close_does_not_run_when_the_admission_barrier_fails() {
        let launcher = Arc::new(Launcher {
            closes: true,
            ..Default::default()
        });
        let (state, caller, workspace) = broker(Arc::clone(&launcher), true);
        let scratch = tempfile::tempdir().unwrap();
        let mut journal = super::super::invocation_journal::InvocationJournal::open(
            &scratch.path().join("journal"),
        )
        .unwrap();
        journal.fail_at = Some(super::super::invocation_journal::BarrierFailure::File);
        *state.journal.lock().unwrap() = Ok(Some(journal));
        assert!(invoke_close(&state, &caller, &close_request(&workspace)).is_err());
        assert!(launcher.closed.lock().unwrap().is_empty());
        assert!(state.completed_invocations.lock().unwrap().is_empty());
    }

    /// The page `query` answers `caller`, and how often it read the
    /// installed-application list.
    fn answered(
        state: &Arc<AppBrokerState>,
        launcher: &Launcher,
        caller: &Caller,
        query: &tilo_actions::remote::CatalogueQuery,
    ) -> (tilo_actions::remote::CataloguePage, usize) {
        let before = launcher.listed.load(std::sync::atomic::Ordering::SeqCst);
        let page = state
            .action_query(caller, query)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("an eager query answers at once")
            .expect("the caller may query");
        let after = launcher.listed.load(std::sync::atomic::Ordering::SeqCst);
        (page, after.saturating_sub(before))
    }

    #[test]
    fn a_provider_scoped_query_builds_only_that_provider() {
        let launcher = Arc::new(Launcher {
            closes: true,
            ..Default::default()
        });
        let (state, caller, workspace) = broker(Arc::clone(&launcher), true);
        let provider = tilo_actions::remote::workspace_provider(&workspace);
        let query = |provider: Option<&str>, max_results| tilo_actions::remote::CatalogueQuery {
            provider: provider.map(str::to_owned),
            max_results,
            ..Default::default()
        };

        // An application instance's own rows never read the installed list.
        let (page, listed) = answered(&state, &launcher, &caller, &query(Some("4"), 1));
        assert_eq!(listed, 0);
        assert!(page.rows.iter().all(|row| row.provider == "4"));
        // Nor does asking only who publishes; the instance is still listed.
        let (page, listed) = answered(&state, &launcher, &caller, &query(None, 0));
        assert_eq!(listed, 0);
        assert!(page.rows.is_empty());
        assert!(
            page.providers
                .iter()
                .any(|provider| provider.provider == "4")
        );
        let (page, listed) = answered(&state, &launcher, &caller, &query(Some(&provider), 64));
        assert_eq!(listed, 1);
        assert!(page.rows.iter().all(|row| row.provider == provider));
        assert!(page.rows.iter().any(|row| {
            row.action
                .as_ref()
                .is_some_and(|action| action.address.action == "close")
        }));
        let (_, listed) = answered(&state, &launcher, &caller, &query(None, 64));
        assert_eq!(listed, 1);

        // A scope only leaves other providers' rows out: what it builds is
        // what the whole catalogue offers that provider, and nothing else.
        let (all, providers) = state.eager_catalogue(&caller, RowScope::All);
        for provider in ["4", provider.as_str()] {
            let (scoped, scoped_providers) =
                state.eager_catalogue(&caller, RowScope::Provider(provider));
            let expected = all
                .iter()
                .filter(|row| row.provider == provider)
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(scoped, expected, "{provider}");
            assert_eq!(scoped_providers, providers);
        }
        let (none, none_providers) = state.eager_catalogue(&caller, RowScope::None);
        assert!(none.is_empty());
        assert_eq!(none_providers, providers);

        // Authority is still decided per row: a caller without a role is
        // offered nothing, scoped or not.
        let stranger = Caller::authenticated([99; 32], [98; 32]).in_workspace(workspace);
        let (rows, _) = state.eager_catalogue(&stranger, RowScope::Provider(&provider));
        assert!(rows.is_empty());
    }

    #[test]
    fn a_caller_who_may_not_change_the_app_cannot_close_it() {
        let launcher = Arc::new(Launcher {
            closes: true,
            ..Default::default()
        });
        let (state, _, workspace) = broker(Arc::clone(&launcher), true);
        let stranger = Caller::authenticated([99; 32], [98; 32]).in_workspace(workspace);
        assert!(app_actions(&state, &stranger).is_empty());
        assert!(launcher.closed.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
#[path = "action_host/app_scope_tests.rs"]
mod app_scope_tests;

#[cfg(test)]
mod argument_size_tests {
    use super::*;

    #[test]
    fn an_oversized_argument_says_its_size_and_the_limit() {
        assert_eq!(oversized_argument("{}"), None);
        let big = "x".repeat(DIRECTORY_MAX_PARAMS_BYTES + 1);
        let reason = oversized_argument(&big).unwrap_or_default();
        assert!(reason.contains(&big.len().to_string()), "{reason}");
        assert!(
            reason.contains(&DIRECTORY_MAX_PARAMS_BYTES.to_string()),
            "{reason}"
        );
        assert!(reason.contains("split"), "{reason}");
    }
}

#[cfg(test)]
mod device_pairing_tests {
    use super::super::AppBrokerState;
    use super::RowScope;
    use crate::services::{HostStepOutcome, HostStepRequest, HostStepServices};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::{
        Caller, Completer, Confirmation, InvokeArgument, InvokeRequest, Pending,
        RemoteActionMessage, device_pairing, pending,
    };

    /// The host's pairing, standing in: it records each step it was asked
    /// to start and answers when the test completes it.
    #[derive(Default)]
    struct Host {
        started: Mutex<Vec<(String, String, String)>>,
        answers: Mutex<Vec<Completer<HostStepOutcome>>>,
    }

    impl HostStepServices for Host {
        fn provider(&self) -> &'static str {
            device_pairing::PROVIDER
        }

        fn app_id(&self) -> &'static str {
            "so.tilo.settings"
        }

        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            [device_pairing::DISCOVER, device_pairing::APPROVE]
                .into_iter()
                .map(|step| {
                    let mut contract = tilo_actions::ActionContract {
                        kind: tilo_actions::ActionKind::Outside,
                        ..tilo_actions::ActionContract::default()
                    };
                    if step == device_pairing::APPROVE {
                        contract.risk = tilo_actions::RiskLevel::Privileged;
                        contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
                    }
                    tilo_actions::ActionDescriptor {
                        state: tilo_actions::ActionState::default(),
                        semantic_identity: None,
                        address: tilo_actions::ActionAddress::new(device_pairing::RESOURCE, step),
                        resource_kind: device_pairing::RESOURCE.into(),
                        resource_label: "Nearby device pairing".into(),
                        action_label: step.into(),
                        keywords: Vec::new(),
                        icon: None,
                        description: None,
                        locus: tilo_actions::ExecutionLocus::default(),
                        kind: tilo_actions::OperationKind::Effect,
                        contract,
                        input: tilo_actions::InputSpec::None,
                        params: Vec::new(),
                        view: None,
                    }
                })
                .collect()
        }

        fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
            self.started.lock().unwrap().push((
                request.workspace.to_owned(),
                request.actor.id().to_owned(),
                request.address.action.clone(),
            ));
            let (answer, pending) = pending();
            self.answers.lock().unwrap().push(answer);
            Ok(pending)
        }
    }

    fn request(step: &str, confirmation: Option<Confirmation>) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: device_pairing::target(step),
            argument: InvokeArgument::None,
            context: None,
            confirmation,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }

    fn invoked(
        state: &Arc<AppBrokerState>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>
    {
        state
            .action_invoke(caller, None, None, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("admission answers at once")
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its fixture and guards alive through its assertions"
    )]
    #[test]
    fn settings_pairs_as_its_person_and_the_outcome_follows_the_host() {
        let host = Arc::new(Host::default());
        let fixture = super::super::host_steps::signed_test_support::Fixture::new(
            crate::services::HostServices {
                host_steps: vec![host.clone()],
                ..Default::default()
            },
        );
        let state = fixture.state.clone();
        let person = fixture.caller.clone();
        let workspace = fixture.workspace.clone();
        let owner = match person.principal() {
            tilo_actions::remote::CallerPrincipal::Person(owner) => owner.clone(),
            _ => panic!("the settings caller is a person"),
        };

        let rows = state
            .eager_catalogue(&person, RowScope::Provider(device_pairing::PROVIDER))
            .0;
        assert_eq!(rows.len(), 2, "both steps are offered to the person here");
        assert!(rows.iter().all(|row| row.app_id == "so.tilo.settings"));

        // Settings reaches the steps as the person using it, without
        // declaring `[actions] invoke`; another app does not.
        let discover = RemoteActionMessage::Invoke(request(device_pairing::DISCOVER, None));
        assert_eq!(
            state
                .app_door(&person, "so.tilo.settings", &discover)
                .map(|caller| caller.principal().clone())
                .ok(),
            Some(person.principal().clone())
        );
        assert!(state.app_door(&person, "so.tilo.notes", &discover).is_err());

        let unconfirmed = fixture.invoke(&request(device_pairing::APPROVE, None));
        assert_eq!(
            unconfirmed,
            Err(tilo_actions::remote::ActionHostError::confirmation_required()),
            "approval needs the person's confirmation"
        );
        assert!(host.started.lock().unwrap().is_empty(), "nothing started");

        let approved = request(device_pairing::APPROVE, Some(Confirmation::Confirmed));
        let receipt = fixture.invoke(&approved).expect("admitted");
        fixture.assert_signed(&approved, false);
        assert_eq!(
            *host.started.lock().unwrap(),
            [(
                workspace.clone(),
                owner.clone(),
                device_pairing::APPROVE.to_owned()
            )]
        );
        let job = state
            .remote_job(&person, &receipt.invocation)
            .expect("the invocation has a job");
        assert!(matches!(
            state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Accepted { .. })
        ));
        let answer = host.answers.lock().unwrap().pop().expect("started");
        answer.complete(Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({"device": "aa"})),
            resources: Vec::new(),
        }));
        assert!(matches!(
            state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));

        fixture.assert_signed(&approved, true);

        // A remote device, or a caller confined to no workspace, is never offered
        // pairing on this machine.
        let super::super::InvocationCaller::Local { admitted, .. } = &fixture.origin else {
            panic!("the settings fixture retains local custody")
        };
        let certificate = admitted.certificate.as_ref().unwrap();
        let remote =
            Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
                .in_workspace(workspace);
        assert!(
            state
                .eager_catalogue(&remote, RowScope::Provider(device_pairing::PROVIDER))
                .0
                .is_empty()
        );
        assert!(invoked(&state, &remote, &request(device_pairing::DISCOVER, None)).is_err());
        let unconfined = Caller::local_person(owner);
        assert!(
            invoked(
                &state,
                &unconfined,
                &request(device_pairing::DISCOVER, None)
            )
            .is_err()
        );
        assert_eq!(host.started.lock().unwrap().len(), 1);
    }
}
